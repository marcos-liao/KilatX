//! Download engine with dynamic segmentation.
//!
//! A file starts as one segment. Every connection that has nothing to do takes
//! the active segment with the most bytes left and splits it in half, then
//! downloads the second half. Connections that finish early are reused the same
//! way, so all connections stay busy until the very end.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, Timelike};

use futures_util::StreamExt;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::config::{DownloadOptions, ExistingFile, ProxyMode};
use crate::limiter::Limiter;
use crate::util;

/// A segment is only split if at least this much would be left on each side.
const MIN_SPLIT: u64 = 512 * 1024;
/// Near the end, a segment whose connection makes no progress is split down to this size.
const ENDGAME_SPLIT: u64 = 64 * 1024;
/// A segment without progress for this long may be taken over by an idle connection.
const STEAL_AFTER: Duration = Duration::from_secs(2);
/// A connection that receives nothing for this long is dropped and reconnected.
const STALL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 5;

/// The built-in queue that "Download later" uses. It can't be deleted.
pub const MAIN_QUEUE: u64 = 1;
/// A file that failed in a running queue is retried after 30 s.
const QUEUE_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnDone {
    #[default]
    Nothing,
    ExitApp,
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queue {
    pub id: u64,
    pub name: String,
    /// How many files of this queue download at the same time.
    pub max_parallel: u32,
    pub start_enabled: bool,
    /// Minutes after midnight.
    pub start_minute: u32,
    /// Days the scheduled start applies to, Monday first.
    pub days: [bool; 7],
    pub stop_enabled: bool,
    pub stop_minute: u32,
    /// Extra attempts for a file that fails while the queue runs.
    pub retries: u32,
    pub on_done: OnDone,
    #[serde(skip)]
    pub running: bool,
    /// Set once the current run has started at least one file, so an empty
    /// queue never triggers its "when done" action.
    #[serde(skip)]
    processed: bool,
    #[serde(skip)]
    fired_start: Option<i64>,
    #[serde(skip)]
    fired_stop: Option<i64>,
}

impl Queue {
    pub fn new(id: u64, name: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            max_parallel: 2,
            start_enabled: false,
            start_minute: 23 * 60,
            days: [true; 7],
            stop_enabled: false,
            stop_minute: 7 * 60,
            retries: 3,
            on_done: OnDone::Nothing,
            running: false,
            processed: false,
            fired_start: None,
            fired_stop: None,
        }
    }
}

pub enum QueueEvent {
    Finished { name: String, on_done: OnDone },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Queued,
    Connecting,
    Downloading,
    Paused,
    Complete,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    /// Exclusive. `u64::MAX` when the size is unknown.
    pub end: u64,
    pub pos: u64,
    #[serde(skip)]
    pub active: bool,
    /// Last time this segment was claimed or received data.
    #[serde(skip)]
    pub touched: Option<Instant>,
}

impl Segment {
    fn new(start: u64, end: u64) -> Self {
        Self { start, end, pos: start, active: false, touched: None }
    }

    pub fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub url: String,
    pub file_name: String,
    pub save_dir: PathBuf,
    #[serde(default)]
    pub custom_dir: bool,
    pub category: String,
    pub size: Option<u64>,
    pub resumable: bool,
    pub status: Status,
    pub segments: Vec<Segment>,
    pub connections: u32,
    pub error: Option<String>,
    pub added_at: i64,
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub queue_id: Option<u64>,
    /// Order inside the queue (lower runs first).
    #[serde(default)]
    pub queue_pos: u64,
    /// Per-download limit in bytes per second, 0 = none.
    #[serde(default)]
    pub speed_limit: u64,
    #[serde(default)]
    pub retries: u32,
    #[serde(skip)]
    pub failed_at: Option<Instant>,
    /// Extra request headers such as Cookie or Referer (from the browser). Saved
    /// encrypted for this Windows account, and dropped once the download is complete.
    #[serde(default, with = "crate::config::secret_headers")]
    pub headers: Vec<(String, String)>,
    /// The name came from the browser or the user: the server's name doesn't replace it.
    #[serde(default)]
    pub fixed_name: bool,
    /// Folder of the partial file while downloading (temporary folder); `None`: `save_dir`.
    #[serde(default)]
    pub part_dir: Option<PathBuf>,
    /// The user's answer for this download when the file already exists.
    #[serde(default)]
    pub on_existing: Option<ExistingFile>,
    /// Set when the file exists and the user has to decide (policy "Ask").
    #[serde(default)]
    pub conflict: Option<String>,
}

impl Item {
    pub fn downloaded(&self) -> u64 {
        self.segments.iter().map(|s| s.pos.saturating_sub(s.start)).sum()
    }

    pub fn active_connections(&self) -> usize {
        self.segments.iter().filter(|s| s.active).count()
    }

    pub fn final_path(&self) -> PathBuf {
        self.save_dir.join(&self.file_name)
    }

    pub fn part_path(&self) -> PathBuf {
        self.part_dir.as_ref().unwrap_or(&self.save_dir).join(util::part_name(&self.file_name))
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, Status::Connecting | Status::Downloading)
    }
}

/// A new download as the Add dialog or the browser describes it.
#[derive(Default)]
pub struct AddRequest {
    pub url: String,
    pub dir: Option<PathBuf>,
    pub connections: u32,
    pub start: bool,
    pub queue: Option<u64>,
    pub headers: Vec<(String, String)>,
    /// Name to save as (from the browser or typed by the user); `None`: from the server.
    pub file_name: Option<String>,
}

/// What a quick check of a link found (see `Engine::check_link`).
#[derive(Debug)]
pub struct LinkInfo {
    pub name: Option<String>,
    pub size: Option<u64>,
}

pub struct Engine {
    items: Arc<Mutex<Vec<Item>>>,
    queues: Mutex<Vec<Queue>>,
    pub dirty: Arc<AtomicBool>,
    cancels: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    /// Live per-download limiters, so a changed limit applies immediately.
    limiters: Mutex<HashMap<u64, Arc<Limiter>>>,
    global_limit: Arc<Limiter>,
    /// Fastest total download speed seen so far (bytes/s): roughly the internet line's capacity.
    line_peak: Arc<AtomicU64>,
    next_id: AtomicU64,
    next_queue_id: AtomicU64,
    next_pos: AtomicU64,
    /// Options and the HTTP client built from them; new downloads use the latest.
    options: std::sync::RwLock<Arc<DownloadOptions>>,
    client: std::sync::RwLock<reqwest::Client>,
    rt: tokio::runtime::Runtime,
}

impl Engine {
    pub fn new(mut items: Vec<Item>, mut queues: Vec<Queue>, global_limit: u64) -> Self {
        // Anything that was running when the app closed is now paused.
        for it in &mut items {
            if it.is_running() {
                it.status = Status::Paused;
            }
        }
        let next_id = items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        let next_pos = items.iter().map(|i| i.queue_pos).max().unwrap_or(0) + 1;
        if !queues.iter().any(|q| q.id == MAIN_QUEUE) {
            queues.insert(0, Queue::new(MAIN_QUEUE, "Main queue"));
        }
        let next_queue_id = queues.iter().map(|q| q.id).max().unwrap_or(MAIN_QUEUE) + 1;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("failed to start async runtime");
        let options = DownloadOptions::default();
        let client = build_client(&options).expect("failed to create HTTP client");
        Self {
            items: Arc::new(Mutex::new(items)),
            queues: Mutex::new(queues),
            dirty: Arc::new(AtomicBool::new(false)),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            limiters: Mutex::new(HashMap::new()),
            global_limit: Arc::new(Limiter::new(global_limit)),
            line_peak: Arc::new(AtomicU64::new(0)),
            next_id: AtomicU64::new(next_id),
            next_queue_id: AtomicU64::new(next_queue_id),
            next_pos: AtomicU64::new(next_pos),
            options: std::sync::RwLock::new(Arc::new(options)),
            client: std::sync::RwLock::new(client),
            rt,
        }
    }

    /// Applies new options (user agent, proxy, folders...). Downloads that are
    /// already running keep the settings they started with.
    pub fn configure(&self, options: DownloadOptions) -> Result<(), String> {
        let client = build_client(&options)?;
        *self.client.write().unwrap() = client;
        *self.options.write().unwrap() = Arc::new(options);
        Ok(())
    }

    pub fn options(&self) -> Arc<DownloadOptions> {
        self.options.read().unwrap().clone()
    }

    /// The user's answer to "the file already exists" for one download.
    pub fn resolve_conflict(&self, id: u64, choice: ExistingFile) {
        {
            let mut items = self.items.lock().unwrap();
            let Some(it) = items.iter_mut().find(|i| i.id == id) else { return };
            it.conflict = None;
            it.on_existing = Some(choice);
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.start(id);
    }

    pub fn snapshot(&self) -> Vec<Item> {
        self.items.lock().unwrap().clone()
    }

    /// Adds a download. With `start` it begins right away; otherwise it waits
    /// in `queue` (or just sits in the list when `queue` is `None`).
    pub fn add(&self, url: &str, dir: Option<PathBuf>, connections: u32, start: bool, queue: Option<u64>) -> u64 {
        self.add_request(AddRequest { url: url.into(), dir, connections, start, queue, ..Default::default() })
    }

    /// Adds a download with everything the browser knows about it (cookies,
    /// referrer, user agent, file name). Headers are in place before it starts.
    pub fn add_request(&self, req: AddRequest) -> u64 {
        let AddRequest { url, dir, connections, start, queue, headers, file_name: given } = req;
        let url = url.as_str();
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let fixed_name = given.as_deref().is_some_and(|n| !util::sanitize(n).is_empty());
        let file_name = given
            .map(|n| util::sanitize(&n))
            .filter(|n| !n.is_empty())
            .or_else(|| util::name_from_url(url))
            .unwrap_or_else(|| "download".into());
        let options = self.options();
        let category = options.category_for(&file_name);
        let custom_dir = dir.is_some();
        let save_dir = dir.unwrap_or_else(|| options.folder_for(&category));
        self.items.lock().unwrap().push(Item {
            id,
            url: url.trim().to_string(),
            file_name,
            save_dir,
            custom_dir,
            category,
            size: None,
            resumable: false,
            status: Status::Queued,
            segments: Vec::new(),
            connections: connections.clamp(1, 32),
            error: None,
            added_at: util::now(),
            finished_at: None,
            queue_id: queue,
            queue_pos: self.next_pos.fetch_add(1, Ordering::SeqCst),
            speed_limit: 0,
            retries: 0,
            failed_at: None,
            headers,
            fixed_name,
            part_dir: None,
            on_existing: None,
            conflict: None,
        });
        self.dirty.store(true, Ordering::Relaxed);
        if start {
            self.start(id);
        }
        id
    }

    pub fn start(&self, id: u64) {
        let mut cancels = self.cancels.lock().unwrap();
        if cancels.contains_key(&id) {
            return;
        }
        {
            let mut items = self.items.lock().unwrap();
            let Some(it) = items.iter_mut().find(|i| i.id == id) else { return };
            if it.status == Status::Complete {
                return;
            }
            it.status = Status::Connecting;
            it.error = None;
            it.conflict = None;
            self.limiters.lock().unwrap().insert(id, Arc::new(Limiter::new(it.speed_limit)));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        cancels.insert(id, cancel.clone());
        let ctx = Ctx {
            id,
            items: self.items.clone(),
            client: self.client.read().unwrap().clone(),
            options: self.options(),
            digest: Arc::new(Mutex::new(None)),
            cancel,
            dirty: self.dirty.clone(),
            local_limit: self.limiters.lock().unwrap()[&id].clone(),
            global_limit: self.global_limit.clone(),
            line_peak: self.line_peak.clone(),
            headers: self.items.lock().unwrap().iter().find(|i| i.id == id).map(|i| i.headers.clone()).unwrap_or_default(),
        };
        let cancels = self.cancels.clone();
        let started = Instant::now();
        self.rt.spawn(async move {
            let items = ctx.items.clone();
            run(ctx).await;
            cancels.lock().unwrap().remove(&id);
            log_timing(&items, id, started);
        });
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self, id: u64) {
        if let Some(c) = self.cancels.lock().unwrap().get(&id) {
            c.store(true, Ordering::SeqCst);
        }
    }

    pub fn pause_all(&self) {
        for c in self.cancels.lock().unwrap().values() {
            c.store(true, Ordering::SeqCst);
        }
    }

    /// Removes a download from the list. The partial file is always deleted;
    /// the finished file only when `delete_file` is set.
    pub fn remove(&self, id: u64, delete_file: bool) {
        self.pause(id);
        let removed = {
            let mut items = self.items.lock().unwrap();
            let idx = items.iter().position(|i| i.id == id);
            idx.map(|i| items.remove(i))
        };
        let Some(item) = removed else { return };
        crate::log::info("download", &format!("#{id} removed from the list{}: {}", if delete_file { " with its file" } else { "" }, item.file_name));
        self.limiters.lock().unwrap().remove(&id);
        self.dirty.store(true, Ordering::Relaxed);
        let cancels = self.cancels.clone();
        self.rt.spawn(async move {
            // Wait for the download task to let go of the file before deleting it.
            for _ in 0..50 {
                if !cancels.lock().unwrap().contains_key(&id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let _ = std::fs::remove_file(item.part_path());
            if delete_file && item.status == Status::Complete {
                let _ = std::fs::remove_file(item.final_path());
            }
        });
    }

    pub fn clear_completed(&self) {
        let removed = {
            let mut items = self.items.lock().unwrap();
            let before = items.len();
            items.retain(|i| i.status != Status::Complete);
            before - items.len()
        };
        crate::log::info("download", &format!("finished downloads cleared from the list ({removed})"));
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// "Change address": points a stopped download at a new link (for example
    /// a fresh signed link) and keeps what was already downloaded.
    pub fn set_url(&self, id: u64, url: &str) -> Result<(), String> {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("Enter a link that starts with http:// or https://".into());
        }
        if self.cancels.lock().unwrap().contains_key(&id) {
            return Err("Pause the download first".into());
        }
        let mut items = self.items.lock().unwrap();
        let it = items.iter_mut().find(|i| i.id == id).ok_or("Download not found")?;
        it.url = url.to_string();
        if it.status == Status::Error {
            it.status = Status::Paused;
            it.error = None;
        }
        self.dirty.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Cookies/Referer for sites that need a login (normally given to `add_request`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_headers(&self, id: u64, headers: Vec<(String, String)>) {
        if let Some(it) = self.items.lock().unwrap().iter_mut().find(|i| i.id == id) {
            it.headers = headers;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Asks the server for `url` the way a download would (same options, login and
    /// `headers`) and stops after the response headers. Fails with a plain-language
    /// reason when the file can't be downloaded this way (error page, login wall, ...).
    pub fn check_link(&self, url: &str, headers: Vec<(String, String)>, timeout: Duration) -> Result<LinkInfo, String> {
        let ctx = Ctx {
            id: u64::MAX,
            items: Arc::new(Mutex::new(Vec::new())),
            client: self.client.read().unwrap().clone(),
            options: self.options(),
            digest: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            local_limit: Arc::new(Limiter::new(0)),
            global_limit: Arc::new(Limiter::new(0)),
            line_peak: Arc::new(AtomicU64::new(0)),
            headers,
        };
        let url = url.to_string();
        self.rt.block_on(async move {
            match tokio::time::timeout(timeout, probe(&ctx, &url)).await {
                // Dropping the response closes the connection without reading the body.
                Ok(Ok(p)) => Ok(LinkInfo { name: p.name, size: p.size }),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("The server did not answer in time.".into()),
            }
        })
    }

    /// The remembered line speed (bytes/s), saved with the settings.
    pub fn line_peak(&self) -> u64 {
        self.line_peak.load(Ordering::Relaxed)
    }

    pub fn set_line_peak(&self, bytes_per_sec: u64) {
        self.line_peak.store(bytes_per_sec, Ordering::Relaxed);
    }

    pub fn global_limit(&self) -> u64 {
        self.global_limit.rate()
    }

    pub fn set_global_limit(&self, bytes_per_sec: u64) {
        self.global_limit.set_rate(bytes_per_sec);
    }

    pub fn set_item_limit(&self, id: u64, bytes_per_sec: u64) {
        if let Some(it) = self.items.lock().unwrap().iter_mut().find(|i| i.id == id) {
            it.speed_limit = bytes_per_sec;
        }
        if let Some(l) = self.limiters.lock().unwrap().get(&id) {
            l.set_rate(bytes_per_sec);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn queues(&self) -> Vec<Queue> {
        self.queues.lock().unwrap().clone()
    }

    pub fn create_queue(&self, name: &str) -> u64 {
        let id = self.next_queue_id.fetch_add(1, Ordering::SeqCst);
        self.queues.lock().unwrap().push(Queue::new(id, name));
        self.dirty.store(true, Ordering::Relaxed);
        id
    }

    /// Saves the settings of a queue (not its running state).
    pub fn update_queue(&self, q: &Queue) {
        if let Some(cur) = self.queues.lock().unwrap().iter_mut().find(|x| x.id == q.id) {
            let name = q.name.trim();
            if !name.is_empty() {
                cur.name = name.to_string();
            }
            cur.max_parallel = q.max_parallel.clamp(1, 20);
            cur.start_enabled = q.start_enabled;
            cur.start_minute = q.start_minute % (24 * 60);
            cur.days = q.days;
            cur.stop_enabled = q.stop_enabled;
            cur.stop_minute = q.stop_minute % (24 * 60);
            cur.retries = q.retries.min(20);
            cur.on_done = q.on_done;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn delete_queue(&self, id: u64) {
        if id == MAIN_QUEUE {
            return;
        }
        self.stop_queue(id);
        self.queues.lock().unwrap().retain(|q| q.id != id);
        for it in self.items.lock().unwrap().iter_mut().filter(|i| i.queue_id == Some(id)) {
            it.queue_id = None;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Puts a download at the end of `queue`, or takes it out of any queue.
    pub fn set_item_queue(&self, id: u64, queue: Option<u64>) {
        if let Some(it) = self.items.lock().unwrap().iter_mut().find(|i| i.id == id) {
            if it.queue_id != queue {
                it.queue_id = queue;
                it.queue_pos = self.next_pos.fetch_add(1, Ordering::SeqCst);
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Swaps a download with its neighbour in the same queue.
    pub fn move_in_queue(&self, id: u64, earlier: bool) {
        let mut items = self.items.lock().unwrap();
        let Some(me) = items.iter().find(|i| i.id == id) else { return };
        let (Some(q), pos) = (me.queue_id, me.queue_pos) else { return };
        let neighbour = items
            .iter()
            .filter(|i| i.queue_id == Some(q) && i.status != Status::Complete)
            .filter(|i| if earlier { i.queue_pos < pos } else { i.queue_pos > pos })
            .min_by_key(|i| if earlier { u64::MAX - i.queue_pos } else { i.queue_pos })
            .map(|i| (i.id, i.queue_pos));
        if let Some((other, other_pos)) = neighbour {
            for it in items.iter_mut() {
                if it.id == id {
                    it.queue_pos = other_pos;
                } else if it.id == other {
                    it.queue_pos = pos;
                }
            }
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn start_queue(&self, id: u64) {
        let mut queues = self.queues.lock().unwrap();
        let Some(q) = queues.iter_mut().find(|q| q.id == id) else { return };
        q.running = true;
        q.processed = false;
        drop(queues);
        // Paused or failed files get a fresh chance when the queue starts.
        for it in self.items.lock().unwrap().iter_mut() {
            if it.queue_id == Some(id) && matches!(it.status, Status::Paused | Status::Error) {
                it.status = Status::Queued;
                it.retries = 0;
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn stop_queue(&self, id: u64) {
        if let Some(q) = self.queues.lock().unwrap().iter_mut().find(|q| q.id == id) {
            q.running = false;
        }
        let running: Vec<u64> = self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.queue_id == Some(id) && i.is_running())
            .map(|i| i.id)
            .collect();
        for id in running {
            self.pause(id);
        }
    }

    /// Runs the scheduler and the queues. Call about twice a second.
    pub fn tick(&self, now: chrono::DateTime<chrono::Local>) -> Vec<QueueEvent> {
        let stamp = now.timestamp() / 60;
        let minute = now.hour() * 60 + now.minute();
        let weekday = now.weekday().num_days_from_monday() as usize;
        let (mut to_start, mut to_stop) = (Vec::new(), Vec::new());
        for q in self.queues.lock().unwrap().iter_mut() {
            if q.start_enabled && q.days[weekday] && q.start_minute == minute && q.fired_start != Some(stamp) {
                q.fired_start = Some(stamp);
                if !q.running {
                    to_start.push(q.id);
                }
            }
            if q.stop_enabled && q.stop_minute == minute && q.fired_stop != Some(stamp) {
                q.fired_stop = Some(stamp);
                if q.running {
                    to_stop.push(q.id);
                }
            }
        }
        to_start.into_iter().for_each(|id| self.start_queue(id));
        to_stop.into_iter().for_each(|id| self.stop_queue(id));

        let queues = self.queues();
        let mut starts = Vec::new();
        let mut finished = Vec::new();
        {
            let mut items = self.items.lock().unwrap();
            // Finished downloads leave their queue.
            for it in items.iter_mut().filter(|i| i.status == Status::Complete && i.queue_id.is_some()) {
                it.queue_id = None;
                self.dirty.store(true, Ordering::Relaxed);
            }
            for q in queues.iter().filter(|q| q.running) {
                let in_q = |i: &Item| i.queue_id == Some(q.id);
                for it in items.iter_mut().filter(|i| in_q(i) && i.status == Status::Error) {
                    let waited = it.failed_at.map_or(true, |t| t.elapsed() >= QUEUE_RETRY_DELAY);
                    if it.retries < q.retries && waited {
                        it.retries += 1;
                        it.status = Status::Queued;
                        it.queue_pos = self.next_pos.fetch_add(1, Ordering::SeqCst); // retry at the end
                    }
                }
                let running = items.iter().filter(|i| in_q(i) && i.is_running()).count();
                let mut pending: Vec<&Item> = items.iter().filter(|i| in_q(i) && i.status == Status::Queued).collect();
                pending.sort_by_key(|i| i.queue_pos);
                let free = (q.max_parallel as usize).saturating_sub(running);
                let picked: Vec<u64> = pending.iter().take(free).map(|i| i.id).collect();
                let retry_later = items.iter().any(|i| in_q(i) && i.status == Status::Error && i.retries < q.retries);
                if running == 0 && picked.is_empty() && !retry_later {
                    finished.push(q.id);
                }
                starts.extend(picked.into_iter().map(|id| (q.id, id)));
            }
        }
        let mut events = Vec::new();
        {
            let mut queues = self.queues.lock().unwrap();
            for (qid, _) in &starts {
                if let Some(q) = queues.iter_mut().find(|q| q.id == *qid) {
                    q.processed = true;
                }
            }
            for qid in finished {
                if let Some(q) = queues.iter_mut().find(|q| q.id == qid) {
                    q.running = false;
                    if q.processed {
                        events.push(QueueEvent::Finished { name: q.name.clone(), on_done: q.on_done });
                    }
                }
            }
        }
        for (_, id) in starts {
            self.start(id);
        }
        events
    }

    /// Pauses everything and waits (up to 3 s) for the tasks to save their position.
    pub fn shutdown(&self) {
        self.pause_all();
        for _ in 0..30 {
            if self.cancels.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        for it in self.items.lock().unwrap().iter_mut() {
            if it.is_running() {
                it.status = Status::Paused;
            }
            for s in &mut it.segments {
                s.active = false;
            }
        }
    }
}

/// Appends "url<TAB>bytes<TAB>milliseconds" when KILAT_TIMING_LOG is set (benchmarks only).
fn log_timing(items: &Mutex<Vec<Item>>, id: u64, started: Instant) {
    let Some(path) = std::env::var_os("KILAT_TIMING_LOG") else { return };
    let items = items.lock().unwrap();
    let Some(it) = items.iter().find(|i| i.id == id && i.status == Status::Complete) else { return };
    let line = format!("{}\t{}\t{}\n", it.url, it.downloaded(), started.elapsed().as_millis());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = std::io::Write::write_all(&mut f, line.as_bytes());
    }
}

/// HTTP/1.1 only: with HTTP/2 every "connection" would be multiplexed over one
/// TCP connection, and servers that limit speed per connection would treat
/// all of them as a single download.
///
/// The default agent is KilatX's own on purpose: sites such as SourceForge answer
/// browser agents with an HTML "your download will start" page instead of the file.
pub fn build_client(options: &DownloadOptions) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent(options.agent())
        .http1_only()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30));
    // Debug builds only: lets end-to-end tests use a local HTTPS server with a
    // throwaway certificate. Release builds always verify certificates.
    #[cfg(debug_assertions)]
    if std::env::var_os("KILATX_TEST_INSECURE_TLS").is_some() {
        builder = builder.danger_accept_invalid_certs(true);
    }
    let p = &options.proxy;
    match p.mode {
        ProxyMode::System => {}
        ProxyMode::None => builder = builder.no_proxy(),
        ProxyMode::Http | ProxyMode::Socks5 => {
            if p.host.trim().is_empty() || p.port == 0 {
                return Err("Enter the proxy address and port.".into());
            }
            // socks5h: names are resolved by the proxy, as a browser would.
            let scheme = if p.mode == ProxyMode::Http { "http" } else { "socks5h" };
            let mut url = reqwest::Url::parse(&format!("{scheme}://{}:{}", p.host.trim(), p.port))
                .map_err(|e| format!("Invalid proxy address: {e}"))?;
            if p.mode == ProxyMode::Socks5 && !p.username.is_empty() {
                let _ = url.set_username(&p.username);
                let _ = url.set_password(Some(&p.password.0));
            }
            let mut proxy = reqwest::Proxy::all(url.as_str()).map_err(|e| format!("Invalid proxy: {e}"))?;
            if p.mode == ProxyMode::Http && !p.username.is_empty() {
                proxy = proxy.basic_auth(&p.username, &p.password.0);
            }
            if !p.bypass.is_empty() {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&p.bypass.join(",")));
            }
            builder = builder.proxy(proxy);
        }
    }
    builder.build().map_err(|e| format!("Cannot set up the connection: {e}"))
}

#[derive(Clone)]
struct Ctx {
    id: u64,
    items: Arc<Mutex<Vec<Item>>>,
    client: reqwest::Client,
    options: Arc<DownloadOptions>,
    /// Digest challenge of a site that asked for one (sent with every later request).
    digest: Arc<Mutex<Option<crate::digest::Challenge>>>,
    cancel: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    local_limit: Arc<Limiter>,
    global_limit: Arc<Limiter>,
    line_peak: Arc<AtomicU64>,
    headers: Vec<(String, String)>,
}

impl Ctx {
    /// A GET request with this download's extra headers and the site's login.
    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.get(url);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let digest = self.digest.lock().unwrap().clone();
        match (digest, self.credentials(url)) {
            (Some(mut ch), Some((user, pass))) => {
                let header = ch.authorization("GET", url, &user, &pass);
                *self.digest.lock().unwrap() = Some(ch); // keeps the request counter
                req = req.header(reqwest::header::AUTHORIZATION, header);
            }
            // Credentials written in the link itself are sent by the client already.
            (None, Some((user, pass))) if !has_userinfo(url) => req = req.basic_auth(user, Some(pass)),
            _ => {}
        }
        req
    }

    /// Username and password for `url`: from the link itself, else from Site logins.
    fn credentials(&self, url: &str) -> Option<(String, String)> {
        if let Ok(u) = reqwest::Url::parse(url) {
            if !u.username().is_empty() {
                let dec = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().to_string();
                return Some((dec(u.username()), dec(u.password().unwrap_or(""))));
            }
        }
        self.options.login_for(url).map(|l| (l.username.clone(), l.password.0.clone()))
    }

    /// Plain-language network error; points at the proxy when one is set.
    fn net(&self, e: reqwest::Error) -> String {
        let p = &self.options.proxy;
        if e.is_connect() && matches!(p.mode, ProxyMode::Http | ProxyMode::Socks5) {
            return format!(
                "Could not connect through the proxy {}:{}. Check its address, port and login in Settings › Proxy.",
                p.host, p.port
            );
        }
        net_err(e)
    }

    /// Sends a GET (optionally a range). Answers a Digest login challenge once.
    async fn send(&self, url: &str, range: Option<&str>) -> reqwest::Result<reqwest::Response> {
        let build = || {
            let req = self.get(url);
            match range {
                Some(r) => req.header(RANGE, r),
                None => req,
            }
        };
        let resp = build().send().await?;
        if resp.status() == StatusCode::UNAUTHORIZED && self.credentials(url).is_some() {
            let challenge = resp
                .headers()
                .get_all(reqwest::header::WWW_AUTHENTICATE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .find_map(crate::digest::Challenge::parse);
            if let Some(ch) = challenge {
                let renewed = self.digest.lock().unwrap().as_ref().is_none_or(|old| old.nonce != ch.nonce);
                if renewed {
                    *self.digest.lock().unwrap() = Some(ch);
                    return build().send().await;
                }
            }
        }
        Ok(resp)
    }

    /// Waits as long as the per-download and global speed limits require.
    async fn throttle(&self, n: usize) {
        self.local_limit.acquire(n, &self.cancel).await;
        self.global_limit.acquire(n, &self.cancel).await;
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Runs `f` on this download's item. Fails if the item was removed from the list.
    fn item<R>(&self, f: impl FnOnce(&mut Item) -> R) -> Result<R, String> {
        let mut items = self.items.lock().unwrap();
        match items.iter_mut().find(|i| i.id == self.id) {
            Some(it) => Ok(f(it)),
            None => Err(REMOVED.into()),
        }
    }
}

const REMOVED: &str = "removed";
/// The file exists and the policy is "Ask": the download waits for the user.
const ASK_USER: &str = "ask user";

fn has_userinfo(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| !u.username().is_empty())
}
/// A range request was answered with the whole file: the server stopped honouring ranges.
const RANGE_REFUSED: &str = "range refused";
/// 503/429: the server wants fewer connections.
const TOO_MANY: &str = "too many connections";

/// Plain-language text for an HTTP error status.
fn http_error(status: StatusCode) -> String {
    let code = status.as_u16();
    let what = status.canonical_reason().unwrap_or("");
    match code {
        401 => format!("Login required ({code} {what}). Add the site in Settings › Site logins, or put the login in the link: https://user:pass@host/..."),
        403 => format!("Access denied ({code} {what}). The link may have expired or need a login. Use \"Change address\" to paste a fresh link."),
        404 | 410 => format!("File not found on the server ({code} {what})."),
        407 => format!("The proxy needs a login ({code} {what}). Enter the proxy username and password in Settings › Proxy."),
        429 | 503 => format!("Server is busy ({code} {what}). Try again later or use fewer connections."),
        500..=599 => format!("Server error ({code} {what}). Try again later."),
        _ => format!("Server responded {code} {what}."),
    }
}

async fn run(ctx: Ctx) {
    let result = run_inner(&ctx).await;
    let cancelled = ctx.cancelled();
    let _ = ctx.item(|it| {
        for s in &mut it.segments {
            s.active = false;
        }
        if it.status == Status::Complete {
            return;
        }
        match result {
            Err(e) if e == ASK_USER => it.status = Status::Paused,
            Err(e) if !cancelled => {
                it.status = Status::Error;
                it.error = Some(e);
                it.failed_at = Some(Instant::now());
            }
            _ => it.status = Status::Paused,
        }
    });
    ctx.dirty.store(true, Ordering::Relaxed);
}

async fn run_inner(ctx: &Ctx) -> Result<(), String> {
    let (url, fresh, resumable) = ctx.item(|it| (it.url.clone(), it.segments.is_empty(), it.resumable))?;

    // The probe's response body is used by the first connection, so the
    // download starts with the very first request.
    let mut first_resp = None;

    // New download, or one that can't resume: (re)start from zero.
    if fresh || !resumable {
        let mut probe = probe(ctx, &url).await?;
        crate::debug_log!("download", "#{} first answer: size {:?}, resumable {}, name {:?}", ctx.id, probe.size, probe.resumable, probe.name);
        first_resp = probe.resp.take();
        if ctx.cancelled() {
            return Ok(());
        }
        let mut items = ctx.items.lock().unwrap();
        let others: Vec<(PathBuf, String)> = items
            .iter()
            .filter(|i| i.id != ctx.id && i.status != Status::Complete)
            .map(|i| (i.save_dir.clone(), i.file_name.clone()))
            .collect();
        let it = items.iter_mut().find(|i| i.id == ctx.id).ok_or(REMOVED)?;
        if fresh {
            if let (Some(name), false) = (probe.name, it.fixed_name) {
                it.file_name = name;
            }
            it.category = ctx.options.category_for(&it.file_name);
            if !it.custom_dir {
                it.save_dir = ctx.options.folder_for(&it.category);
            }
            it.part_dir = ctx.options.temp_folder.clone();
            // Only unfinished downloads into the same folder can clash.
            let taken: Vec<String> = others.into_iter().filter(|(d, _)| *d == it.save_dir).map(|(_, n)| n).collect();
            let exists = it.save_dir.join(&it.file_name).exists();
            match it.on_existing.unwrap_or(ctx.options.existing_file) {
                ExistingFile::Skip if exists => {
                    return Err(format!("Skipped: \"{}\" already exists in {}.", it.file_name, it.save_dir.display()));
                }
                ExistingFile::Ask if exists => {
                    it.conflict = Some(it.file_name.clone());
                    return Err(ASK_USER.into());
                }
                // Overwrite keeps the name (it replaces the file when done) unless
                // another unfinished download uses it.
                ExistingFile::Overwrite if !taken.contains(&it.file_name) => {}
                _ => it.file_name = util::unique_name(&it.save_dir, &it.file_name, &taken),
            }
        }
        it.size = probe.size;
        it.resumable = probe.resumable;
        let end = if it.resumable { probe.size.unwrap_or(u64::MAX) } else { u64::MAX };
        let mut first = Segment::new(0, end);
        // Taken by the connection that already holds the probe response.
        first.active = it.resumable && first_resp.is_some();
        it.segments = vec![first];
        let (dir, part) = (it.save_dir.clone(), it.part_path());
        let prealloc = if it.resumable { probe.size } else { None };
        drop(items);

        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create folder: {e}"))?;
        if let Some(temp) = part.parent() {
            std::fs::create_dir_all(temp).map_err(|e| format!("Cannot create the temporary folder: {e}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&part)
            .map_err(|e| format!("Cannot create file: {e}"))?;
        if let Some(size) = prealloc {
            util::make_sparse(&file);
            file.set_len(size).map_err(|e| format!("Not enough disk space? {e}"))?;
        }
    } else {
        // Resuming: if the partial file vanished, start over.
        let part = ctx.item(|it| it.part_path())?;
        if !part.exists() {
            ctx.item(|it| it.segments.clear())?;
            return Box::pin(run_inner(ctx)).await;
        }
    }

    let (resumable, conns, part) = ctx.item(|it| {
        it.status = Status::Downloading;
        (it.resumable, it.connections, it.part_path())
    })?;
    ctx.dirty.store(true, Ordering::Relaxed);

    if resumable {
        // Smart connections: see `Spread`.
        let spawn = |initial: Option<reqwest::Response>| {
            let (c, u, p) = (ctx.clone(), url.clone(), part.clone());
            tokio::spawn(async move { worker(c, u, p, initial).await })
        };
        let mut handles = vec![spawn(first_resp.take())];
        let mut spawned = 1u32;
        let mut spread = Spread::new(ctx.item(|it| it.downloaded())?);
        let mut first_err = None;
        let mut range_refused = false;
        loop {
            let mut i = 0;
            while i < handles.len() {
                if !handles[i].is_finished() {
                    i += 1;
                    continue;
                }
                match handles.swap_remove(i).await {
                    Ok(Err(e)) if e == RANGE_REFUSED => range_refused = true,
                    Ok(Err(e)) => {
                        first_err.get_or_insert(e);
                    }
                    Err(e) => {
                        first_err.get_or_insert(e.to_string());
                    }
                    Ok(Ok(())) => {}
                }
            }
            if handles.is_empty() {
                break;
            }
            // Once every byte is in (or on pause), don't wait for connections
            // that are stuck on a dead route.
            let complete = ctx.item(|it| it.segments.iter().all(|s| s.pos >= s.end)).unwrap_or(true);
            if complete || ctx.cancelled() {
                handles.iter().for_each(|h| h.abort());
                break;
            }
            let (bytes, left) = ctx.item(|it| (it.downloaded(), it.segments.iter().map(|s| s.remaining()).sum::<u64>()))?;
            for _ in 0..spread.decide(bytes, spawned, conns, left, &ctx.line_peak) {
                handles.push(spawn(None));
                spawned += 1;
                crate::debug_log!("download", "#{} connection {spawned} opened ({} downloaded, {} left)", ctx.id, util::fmt_size(bytes), util::fmt_size(left));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if ctx.cancelled() {
            return Ok(());
        }
        spread.finish(ctx.item(|it| it.downloaded())?, &ctx.line_peak);
        let done = ctx.item(|it| it.segments.iter().all(|s| s.pos >= s.end))?;
        if !done && range_refused {
            // The server stopped honouring ranges: restart with one connection.
            crate::debug_log!("download", "#{} server stopped answering ranges: starting over with one connection", ctx.id);
            ctx.item(|it| {
                it.resumable = false;
                it.segments.clear();
            })?;
            single_stream(ctx, &url, &part, None).await?;
            if ctx.cancelled() {
                return Ok(());
            }
            return finish(ctx);
        }
        if !done {
            return Err(first_err.unwrap_or_else(|| "Download incomplete".into()));
        }
    } else {
        // No resume possible: on a network error start over, a few times.
        let mut attempt = 0;
        let mut resp = first_resp;
        loop {
            match single_stream(ctx, &url, &part, resp.take()).await {
                Ok(()) => break,
                Err(e) if attempt < 3 && !ctx.cancelled() && e != REMOVED => {
                    attempt += 1;
                    crate::debug_log!("download", "#{} single connection failed ({e}), starting over (try {attempt})", ctx.id);
                    tokio::time::sleep(Duration::from_secs(attempt)).await;
                }
                Err(e) => return Err(e),
            }
        }
        if ctx.cancelled() {
            return Ok(());
        }
    }
    finish(ctx)
}

/// Length of one speed measurement while downloading.
const SPEED_WINDOW: Duration = Duration::from_millis(500);
/// One connection's speed is measured between these two moments, after TCP slow start.
const MEASURE_FROM: Duration = Duration::from_millis(300);
const DECIDE_AT: Duration = Duration::from_millis(700);
/// One connection counts as "filling the line" at this share of the remembered line speed.
const FILLS_LINE: f64 = 0.6;

/// Smart connections. Many connections only help when one connection can't
/// fill the internet line (a far server, or one that limits speed per
/// connection); when one connection already fills it, extra connections just
/// compete and the download gets slower (about 12% in tests on a 100 Mbit/s line).
///
/// So one connection is measured from 0.3 s to 0.7 s (after TCP slow start): if
/// it reaches 60% of the line speed remembered from earlier downloads, KilatX
/// stays with it; otherwise all connections open at once. If the speed later
/// falls below half the line speed, they open anyway. With no line speed known
/// yet (first download), all connections are used.
///
/// The line speed is learned only from whole downloads, so short bursts don't
/// inflate it.
struct Spread {
    started: Instant,
    bytes_at_start: u64,
    bytes_at_measure: Option<u64>,
    window_since: Instant,
    window_bytes: u64,
    decided: bool,
    low_windows: u32,
}

impl Spread {
    fn new(bytes: u64) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            bytes_at_start: bytes,
            bytes_at_measure: None,
            window_since: now,
            window_bytes: bytes,
            decided: false,
            low_windows: 0,
        }
    }

    /// How many connections to open now.
    fn decide(&mut self, bytes: u64, spawned: u32, max: u32, left: u64, line_peak: &AtomicU64) -> u32 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.started);
        let mut window_speed = None;
        let window = now.duration_since(self.window_since);
        if window >= SPEED_WINDOW {
            window_speed = Some(bytes.saturating_sub(self.window_bytes) as f64 / window.as_secs_f64());
            self.window_since = now;
            self.window_bytes = bytes;
        }
        if spawned >= max || left < 2 * MIN_SPLIT {
            return 0;
        }
        let peak = line_peak.load(Ordering::Relaxed) as f64;
        if !self.decided {
            if elapsed >= MEASURE_FROM && self.bytes_at_measure.is_none() {
                self.bytes_at_measure = Some(bytes);
            }
            if elapsed < DECIDE_AT {
                return 0;
            }
            self.decided = true;
            let from = self.bytes_at_measure.unwrap_or(self.bytes_at_start);
            let one = bytes.saturating_sub(from) as f64 / (DECIDE_AT - MEASURE_FROM).as_secs_f64();
            if peak > 0.0 && one >= FILLS_LINE * peak {
                return 0;
            }
            return max - spawned;
        }
        if let Some(speed) = window_speed {
            self.low_windows = if peak > 0.0 && speed < 0.5 * peak { self.low_windows + 1 } else { 0 };
            if self.low_windows >= 2 {
                return max - spawned;
            }
        }
        0
    }

    /// Learns the line speed from the average of the whole download.
    fn finish(&self, bytes: u64, line_peak: &AtomicU64) {
        let elapsed = self.started.elapsed().as_secs_f64();
        if elapsed >= 0.5 {
            line_peak.fetch_max((bytes.saturating_sub(self.bytes_at_start) as f64 / elapsed) as u64, Ordering::Relaxed);
        }
    }
}

struct Probe {
    size: Option<u64>,
    resumable: bool,
    name: Option<String>,
    /// The open response (whole file from byte 0), ready to be streamed.
    resp: Option<reqwest::Response>,
}

/// Asks for the whole file as a range. A 206 answer means the server supports
/// ranges (resume + multiple connections) and tells us the full size.
async fn probe(ctx: &Ctx, url: &str) -> Result<Probe, String> {
    // A first request that can't connect (slow mirror, flaky Wi-Fi) is retried a few times.
    let mut attempt = 0u64;
    let mut resp = loop {
        match ctx.send(url, Some("bytes=0-")).await {
            Ok(r) => break r,
            Err(e) if attempt < 3 && !ctx.cancelled() && !e.is_builder() => {
                attempt += 1;
                crate::debug_log!("download", "#{} first request failed ({}), retry {attempt}", ctx.id, ctx.net(e));
                sleep_unless_cancelled(ctx, Duration::from_secs(attempt)).await;
            }
            Err(e) => return Err(ctx.net(e)),
        }
    };
    // Busy server on the very first request: wait as asked and try again a few times.
    for attempt in 1..=4u32 {
        if !matches!(resp.status(), StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE) || ctx.cancelled() {
            break;
        }
        let asked = retry_after(&resp);
        let wait = if asked > 0 { asked } else { 1 << attempt };
        crate::debug_log!("download", "#{} server busy (HTTP {}), waiting {wait} s", ctx.id, resp.status().as_u16());
        sleep_unless_cancelled(ctx, Duration::from_secs(wait)).await;
        resp = ctx.send(url, Some("bytes=0-")).await.map_err(|e| ctx.net(e))?;
    }
    let status = resp.status();
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // Typical answer for an empty file ("bytes */0"); otherwise ask without a range.
        let total = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.trim().parse::<u64>().ok());
        let name = util::name_from_url(resp.url().as_str());
        if total == Some(0) {
            return Ok(Probe { size: Some(0), resumable: true, name, resp: None });
        }
        let resp = ctx.send(url, None).await.map_err(|e| ctx.net(e))?;
        if !resp.status().is_success() {
            return Err(http_error(resp.status()));
        }
        let size = resp.content_length();
        return Ok(Probe { size, resumable: false, name, resp: Some(resp) });
    }
    if status == StatusCode::UNAUTHORIZED && ctx.credentials(url).is_some() {
        return Err("Login failed (401 Unauthorized). Check the username and password for this site in Settings › Site logins.".into());
    }
    if !status.is_success() {
        return Err(http_error(status));
    }
    let headers = resp.headers();
    let is_html = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/html"));
    let name = util::name_from_disposition(headers.get(CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()))
        .or_else(|| util::name_from_url(resp.url().as_str()));
    let expected = util::name_from_url(url).map(|n| util::extension(&n)).unwrap_or_default();
    if is_html && !matches!(expected.as_str(), "" | "html" | "htm" | "php" | "asp" | "aspx" | "jsp" | "cgi") {
        return Err(format!(
            "The server sent a web page instead of the .{expected} file. The link may need to be opened in a browser first."
        ));
    }
    if status == StatusCode::PARTIAL_CONTENT {
        let size = headers
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.trim().parse::<u64>().ok());
        Ok(Probe { size, resumable: size.is_some(), name, resp: Some(resp) })
    } else {
        let size = resp.content_length();
        Ok(Probe { size, resumable: false, name, resp: Some(resp) })
    }
}

/// Picks work for one connection: an idle unfinished segment, else the biggest
/// active segment split in half, else (endgame) everything a stalled connection
/// still had to fetch.
fn claim(ctx: &Ctx) -> Option<usize> {
    let mut items = ctx.items.lock().unwrap();
    let it = items.iter_mut().find(|i| i.id == ctx.id)?;
    if let Some(i) = it.segments.iter().position(|s| !s.active && s.pos < s.end) {
        it.segments[i].active = true;
        it.segments[i].touched = Some(Instant::now());
        return Some(i);
    }
    let biggest = it
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.active)
        .map(|(i, s)| (i, s.remaining()))
        .max_by_key(|&(_, left)| left);
    let stalled = it
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.active && s.remaining() >= ENDGAME_SPLIT)
        .filter(|(_, s)| s.touched.is_none_or(|t| t.elapsed() >= STEAL_AFTER))
        .map(|(i, s)| (i, s.remaining()))
        .max_by_key(|&(_, left)| left);
    let (i, mid) = match (biggest, stalled) {
        (Some((i, left)), _) if left >= 2 * MIN_SPLIT => (i, it.segments[i].pos + left / 2),
        // The stalled connection keeps nothing; it finds its segment done when it wakes up.
        (_, Some((i, _))) => (i, it.segments[i].pos),
        _ => return None,
    };
    let seg = &mut it.segments[i];
    let end = seg.end;
    crate::debug_log!(
        "download",
        "#{} {} segment {i} at {mid} (was {}-{end})",
        ctx.id,
        if mid == seg.pos { "took over stalled" } else { "split" },
        seg.pos
    );
    seg.end = mid;
    let mut new = Segment::new(mid, end);
    new.active = true;
    new.touched = Some(Instant::now());
    it.segments.push(new);
    Some(it.segments.len() - 1)
}

async fn sleep_unless_cancelled(ctx: &Ctx, total: Duration) {
    let until = Instant::now() + total;
    while !ctx.cancelled() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// True while some part of the file is still missing.
fn unfinished(ctx: &Ctx) -> bool {
    ctx.item(|it| it.segments.iter().any(|s| s.pos < s.end)).unwrap_or(false)
}

async fn worker(ctx: Ctx, url: String, part: PathBuf, mut initial: Option<reqwest::Response>) -> Result<(), String> {
    loop {
        if ctx.cancelled() {
            return Ok(());
        }
        // The first connection continues the probe response on segment 0.
        let idx = if initial.is_some() { 0 } else {
            match claim(&ctx) {
                Some(i) => i,
                // Nothing to take right now: stay to help if another connection stalls.
                None if unfinished(&ctx) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                None => return Ok(()),
            }
        };
        // Counts failures in a row without progress; a connection that keeps
        // dropping but still moves forward is retried as long as it advances.
        let mut tries = 0;
        let res = loop {
            let before = ctx.item(|it| it.segments[idx].pos)?;
            let attempt = match initial.take() {
                Some(resp) => stream_segment(&ctx, idx, &part, resp).await,
                None => fetch_segment(&ctx, idx, &url, &part).await,
            };
            match attempt {
                Ok(()) => break Ok(()),
                Err(e) if e == RANGE_REFUSED || e == REMOVED => break Err(e),
                Err(e) if e.starts_with(TOO_MANY) => {
                    // The server wants fewer connections: drop this one if others are running.
                    let others = ctx.item(|it| it.segments.iter().enumerate().any(|(i, s)| i != idx && s.active))?;
                    crate::debug_log!("download", "#{} server asked for fewer connections (segment {idx})", ctx.id);
                    if others {
                        break Ok(());
                    }
                    // Last connection left: wait as long as the server asks (or back off).
                    tries += 1;
                    if ctx.cancelled() || tries > 6 {
                        break Err(http_error(StatusCode::TOO_MANY_REQUESTS));
                    }
                    let asked: u64 = e.rsplit('|').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let wait = if asked > 0 { asked } else { 1 << (tries - 1).min(5) };
                    sleep_unless_cancelled(&ctx, Duration::from_secs(wait)).await;
                }
                Err(e) => {
                    let after = ctx.item(|it| it.segments[idx].pos)?;
                    tries = if after > before { 0 } else { tries + 1 };
                    crate::debug_log!("download", "#{} segment {idx}: {e} (failures in a row: {tries})", ctx.id);
                    if ctx.cancelled() || tries >= MAX_RETRIES {
                        break Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(500 * tries.max(1) as u64)).await;
                }
            }
        };
        let _ = ctx.item(|it| it.segments[idx].active = false);
        if let Err(e) = res {
            return if ctx.cancelled() { Ok(()) } else { Err(e) };
        }
        if ctx.item(|it| it.segments[idx].remaining())? > 0 {
            // Gave this segment back (server asked for fewer connections).
            return Ok(());
        }
    }
}

async fn fetch_segment(ctx: &Ctx, idx: usize, url: &str, part: &Path) -> Result<(), String> {
    let (pos, end) = ctx.item(|it| (it.segments[idx].pos, it.segments[idx].end))?;
    if pos >= end {
        return Ok(());
    }
    let resp = ctx.send(url, Some(&format!("bytes={}-{}", pos, end - 1))).await.map_err(|e| ctx.net(e))?;
    match resp.status() {
        StatusCode::PARTIAL_CONTENT => stream_segment(ctx, idx, part, resp).await,
        StatusCode::OK => Err(RANGE_REFUSED.into()),
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => Err(format!("{TOO_MANY}|{}", retry_after(&resp))),
        other => Err(http_error(other)),
    }
}

/// Seconds from a Retry-After header (0 when absent), capped at a minute.
fn retry_after(resp: &reqwest::Response) -> u64 {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .min(60)
}

/// Writes a range response into segment `idx`, starting at the segment's position.
async fn stream_segment(ctx: &Ctx, idx: usize, part: &Path, resp: reqwest::Response) -> Result<(), String> {
    let pos = ctx.item(|it| it.segments[idx].pos)?;
    let file = OpenOptions::new().write(true).open(part).map_err(|e| e.to_string())?;
    let mut stream = resp.bytes_stream();
    let mut cur = pos;
    loop {
        let next = tokio::time::timeout(STALL_TIMEOUT, stream.next())
            .await
            .map_err(|_| "Connection stalled (no data for 10 s)".to_string())?;
        let Some(chunk) = next else { break };
        if ctx.cancelled() {
            return Ok(());
        }
        let chunk = chunk.map_err(|e| ctx.net(e))?;
        // Another connection may have taken over the tail of this segment.
        let seg_end = ctx.item(|it| it.segments[idx].end)?;
        if cur >= seg_end {
            break;
        }
        let take = (chunk.len() as u64).min(seg_end - cur) as usize;
        ctx.throttle(take).await;
        if ctx.cancelled() {
            return Ok(());
        }
        util::write_at(&file, &chunk[..take], cur).map_err(|e| format!("Write error: {e}"))?;
        cur += take as u64;
        let finished = ctx.item(|it| {
            let s = &mut it.segments[idx];
            s.pos = s.pos.max(cur.min(s.end));
            s.touched = Some(Instant::now());
            s.pos >= s.end
        })?;
        ctx.dirty.store(true, Ordering::Relaxed);
        if finished {
            return Ok(());
        }
    }
    let left = ctx.item(|it| it.segments[idx].remaining())?;
    if left > 0 && !ctx.cancelled() {
        Err("Connection closed early".into())
    } else {
        Ok(())
    }
}

/// For servers without range support: one connection, no resume.
async fn single_stream(ctx: &Ctx, url: &str, part: &Path, resp: Option<reqwest::Response>) -> Result<(), String> {
    let resp = match resp {
        Some(r) => r,
        None => ctx.send(url, None).await.map_err(|e| ctx.net(e))?,
    };
    if !resp.status().is_success() {
        return Err(http_error(resp.status()));
    }
    let length = resp.content_length();
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(part)
        .map_err(|e| e.to_string())?;
    ctx.item(|it| {
        let mut seg = Segment::new(0, u64::MAX);
        seg.active = true;
        it.segments = vec![seg];
        if it.size.is_none() {
            it.size = length;
        }
    })?;
    let mut stream = resp.bytes_stream();
    let mut cur = 0u64;
    loop {
        let next = tokio::time::timeout(STALL_TIMEOUT, stream.next())
            .await
            .map_err(|_| "Connection stalled (no data for 10 s)".to_string())?;
        let Some(chunk) = next else { break };
        if ctx.cancelled() {
            return Ok(());
        }
        let chunk = chunk.map_err(|e| ctx.net(e))?;
        ctx.throttle(chunk.len()).await;
        if ctx.cancelled() {
            return Ok(());
        }
        util::write_at(&file, &chunk, cur).map_err(|e| format!("Write error: {e}"))?;
        cur += chunk.len() as u64;
        ctx.item(|it| it.segments[0].pos = cur)?;
        ctx.dirty.store(true, Ordering::Relaxed);
    }
    if let Some(len) = length {
        if cur < len {
            return Err("Connection closed early".into());
        }
    }
    ctx.item(|it| {
        it.segments[0].end = cur;
        it.size = Some(cur);
    })?;
    Ok(())
}

/// Renames `file.ext.kilatpart` to `file.ext` and marks the download complete.
fn finish(ctx: &Ctx) -> Result<(), String> {
    let (dir, name, part, policy) = ctx.item(|it| {
        (it.save_dir.clone(), it.file_name.clone(), it.part_path(), it.on_existing.unwrap_or(ctx.options.existing_file))
    })?;
    let target = dir.join(&name);
    let final_name = if !target.exists() {
        name
    } else if policy == ExistingFile::Overwrite {
        std::fs::remove_file(&target).map_err(|e| format!("Cannot replace the existing file: {e}"))?;
        name
    } else {
        util::unique_name(&dir, &name, &[])
    };
    util::move_file(&part, &dir.join(&final_name)).map_err(|e| format!("Cannot finish file: {e}"))?;
    crate::debug_log!("download", "#{} saved as {}", ctx.id, dir.join(&final_name).display());
    ctx.item(|it| {
        it.file_name = final_name;
        it.status = Status::Complete;
        it.finished_at = Some(util::now());
        it.headers.clear(); // cookies aren't needed any more
        if it.size.is_none() {
            it.size = Some(it.downloaded());
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Needs a local range server: run with
    //! `KILAT_TEST_BASE=http://127.0.0.1:8765 KILAT_TEST_SRC=<dir with test.bin> cargo test -- --ignored`
    use super::*;

    fn env(k: &str) -> String {
        std::env::var(k).unwrap_or_else(|_| panic!("set {k}"))
    }

    fn wait(engine: &Engine, id: u64, until: impl Fn(&Item) -> bool, secs: u64) -> Item {
        let t = std::time::Instant::now();
        loop {
            let it = engine.snapshot().into_iter().find(|i| i.id == id).unwrap();
            if until(&it) || t.elapsed().as_secs() > secs {
                return it;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn same_as_source(it: &Item) -> bool {
        let src = std::fs::read(PathBuf::from(env("KILAT_TEST_SRC")).join("test.bin")).unwrap();
        std::fs::read(it.final_path()).unwrap() == src
    }

    fn out_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("kilat-test").join(name);
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    #[ignore]
    fn multi_connection_download_is_byte_exact() {
        let engine = Engine::new(vec![], vec![], 0);
        let url = format!("{}/test.bin", env("KILAT_TEST_BASE"));
        let id = engine.add(&url, Some(out_dir("multi")), 8, true, None);
        let mid = wait(&engine, id, |i| i.active_connections() >= 4, 10);
        println!("connections during download: {}", mid.active_connections());
        assert!(mid.active_connections() >= 4, "expected several connections");
        let it = wait(&engine, id, |i| matches!(i.status, Status::Complete | Status::Error), 120);
        assert_eq!(it.status, Status::Complete, "{:?}", it.error);
        println!("segments used: {}", it.segments.len());
        assert!(same_as_source(&it));
    }

    #[test]
    #[ignore]
    fn pause_and_resume_is_byte_exact() {
        let engine = Engine::new(vec![], vec![], 0);
        let url = format!("{}/test.bin", env("KILAT_TEST_BASE"));
        let id = engine.add(&url, Some(out_dir("resume")), 6, true, None);
        let before = wait(&engine, id, |i| i.downloaded() > 3_000_000, 30).downloaded();
        engine.pause(id);
        let paused = wait(&engine, id, |i| i.status == Status::Paused, 10);
        assert_eq!(paused.status, Status::Paused);
        println!("paused at {} bytes (asked at {before})", paused.downloaded());
        engine.start(id);
        let it = wait(&engine, id, |i| matches!(i.status, Status::Complete | Status::Error), 120);
        assert_eq!(it.status, Status::Complete, "{:?}", it.error);
        assert!(same_as_source(&it));
    }

    fn timed_download(engine: &Engine, id: u64) -> (Item, f64) {
        let t = std::time::Instant::now();
        let it = wait(engine, id, |i| matches!(i.status, Status::Complete | Status::Error), 60);
        (it, t.elapsed().as_secs_f64())
    }

    #[test]
    #[ignore]
    fn global_speed_limit_is_respected() {
        // 4 MB at 1 MB/s with 8 connections should still take about 4 s.
        let engine = Engine::new(vec![], vec![], 1 << 20);
        let url = format!("{}/Annual-Report-2026.pdf", env("KILAT_TEST_BASE"));
        let id = engine.add(&url, Some(out_dir("global-limit")), 8, true, None);
        let (it, secs) = timed_download(&engine, id);
        println!("global limit: {} bytes in {secs:.2}s", it.size.unwrap_or(0));
        assert_eq!(it.status, Status::Complete, "{:?}", it.error);
        let expected = it.size.unwrap() as f64 / (1 << 20) as f64;
        assert!(secs > expected * 0.8 && secs < expected * 1.4, "took {secs:.2}s, expected ~{expected:.1}s");
    }

    #[test]
    #[ignore]
    fn per_download_limit_is_respected() {
        let engine = Engine::new(vec![], vec![], 0);
        let url = format!("{}/Annual-Report-2026.pdf", env("KILAT_TEST_BASE"));
        let id = engine.add(&url, Some(out_dir("item-limit")), 8, false, None);
        engine.set_item_limit(id, 2 << 20);
        engine.start(id);
        let (it, secs) = timed_download(&engine, id);
        println!("per-download limit: {} bytes in {secs:.2}s", it.size.unwrap_or(0));
        assert_eq!(it.status, Status::Complete, "{:?}", it.error);
        let expected = it.size.unwrap() as f64 / (2 << 20) as f64;
        assert!(secs > expected * 0.8 && secs < expected * 1.4, "took {secs:.2}s, expected ~{expected:.1}s");
    }

    #[test]
    #[ignore]
    fn queue_downloads_in_order_one_at_a_time() {
        let engine = Engine::new(vec![], vec![], 0);
        let mut main = engine.queues()[0].clone();
        main.max_parallel = 1;
        main.on_done = OnDone::Shutdown; // only reported as an event here
        engine.update_queue(&main);
        let base = env("KILAT_TEST_BASE");
        let dir = out_dir("queue");
        let ids: Vec<u64> = ["Annual-Report-2026.pdf", "Podcast-Episode-42.mp3", "Annual-Report-2026.pdf"]
            .iter()
            .map(|f| engine.add(&format!("{base}/{f}"), Some(dir.clone()), 4, false, Some(MAIN_QUEUE)))
            .collect();
        engine.move_in_queue(ids[1], true); // order is now: 1, 0, 2
        engine.start_queue(MAIN_QUEUE);

        let (mut max_running, mut events) = (0, 0);
        let t = std::time::Instant::now();
        while t.elapsed().as_secs() < 60 {
            for e in engine.tick(chrono::Local::now()) {
                let QueueEvent::Finished { on_done, .. } = e;
                assert_eq!(on_done, OnDone::Shutdown);
                events += 1;
            }
            let items = engine.snapshot();
            max_running = max_running.max(items.iter().filter(|i| i.is_running()).count());
            if items.iter().all(|i| i.status == Status::Complete) && !engine.queues()[0].running {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let items = engine.snapshot();
        let done = |id: u64| items.iter().find(|i| i.id == id).unwrap().clone();
        assert!(items.iter().all(|i| i.status == Status::Complete), "not all complete");
        assert_eq!(max_running, 1, "queue ran more than one file at a time");
        assert_eq!(events, 1, "finished event should fire exactly once");
        assert!(items.iter().all(|i| i.queue_id.is_none()), "finished files should leave the queue");
        let order: Vec<i64> = [ids[1], ids[0], ids[2]].iter().map(|&id| done(id).finished_at.unwrap()).collect();
        assert!(order.windows(2).all(|w| w[0] <= w[1]), "wrong order: {order:?}");
    }

    #[test]
    fn scheduler_starts_and_stops_on_time() {
        use chrono::TimeZone;
        let engine = Engine::new(vec![], vec![], 0);
        let mut q = engine.queues()[0].clone();
        q.start_enabled = true;
        q.start_minute = 23 * 60;
        q.days = [false, true, false, false, false, false, false]; // Tuesday only
        q.stop_enabled = true;
        q.stop_minute = 23 * 60 + 30;
        engine.update_queue(&q);
        // A file that can never download keeps the queue busy (failing, then waiting to retry).
        engine.add("http://127.0.0.1:9/never.bin", Some(out_dir("sched")), 1, false, Some(MAIN_QUEUE));
        let at = |d: u32, h: u32, m: u32| chrono::Local.with_ymd_and_hms(2026, 10, d, h, m, 0).single().unwrap();
        let running = || engine.queues()[0].running;

        engine.tick(at(12, 23, 0)); // Monday: not scheduled
        assert!(!running());
        engine.tick(at(13, 22, 59)); // Tuesday, a minute early
        assert!(!running());
        engine.tick(at(13, 23, 0));
        assert!(running(), "should start at 23:00 on Tuesday");
        engine.stop_queue(MAIN_QUEUE);
        engine.tick(at(13, 23, 0)); // same minute again: must not restart
        assert!(!running());
        engine.start_queue(MAIN_QUEUE);
        engine.tick(at(13, 23, 30));
        assert!(!running(), "should stop at 23:30");
    }

    #[test]
    #[ignore]
    fn client_uses_http1_even_when_server_offers_http2() {
        // python.org is served by a CDN that supports HTTP/2.
        let url = "https://www.python.org/ftp/python/3.13.1/python-3.13.1-amd64.exe";
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let default = reqwest::Client::new().get(url).header(RANGE, "bytes=0-0").send().await.unwrap();
            let ours = build_client(&DownloadOptions::default()).unwrap().get(url).header(RANGE, "bytes=0-0").send().await.unwrap();
            println!("default reqwest client: {:?}, KilatX client: {:?}", default.version(), ours.version());
            assert_eq!(ours.version(), reqwest::Version::HTTP_11);
        });
    }

    #[test]
    #[ignore]
    fn server_without_ranges_falls_back_to_one_connection() {
        let engine = Engine::new(vec![], vec![], 0);
        let url = format!("{}/test.bin?norange", env("KILAT_TEST_BASE"));
        let id = engine.add(&url, Some(out_dir("norange")), 8, true, None);
        let it = wait(&engine, id, |i| matches!(i.status, Status::Complete | Status::Error), 120);
        assert_eq!(it.status, Status::Complete, "{:?}", it.error);
        assert!(!it.resumable);
        assert!(same_as_source(&it));
    }
}

fn net_err(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "Connection timed out".into()
    } else if e.is_connect() {
        "Could not connect to server".into()
    } else if e.is_builder() {
        "Invalid address".into()
    } else if e.is_body() || e.is_decode() {
        "Connection lost while downloading".into()
    } else {
        e.to_string()
    }
}
