//! Notices when KilatX's window stops answering and writes down what it was doing.
//!
//! The UI thread beats a heartbeat twice a second (the main timer) and names what it is
//! busy with (`busy("…")`). A watcher thread checks the heartbeat; when it is more than
//! STALL old, one line goes to KilatX's log (logs\kilatx.log) with how long, and the name of
//! the part of KilatX that was running, and another when it answers again. Only names of
//! KilatX's own code are written: no links, file names or other data.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STALL: Duration = Duration::from_secs(5);

static START: Mutex<Option<Instant>> = Mutex::new(None);
static BEAT_MS: AtomicU64 = AtomicU64::new(0);
static ACTIVITY: Mutex<&'static str> = Mutex::new("starting");

fn now_ms() -> u64 {
    let mut start = START.lock().unwrap_or_else(|e| e.into_inner());
    start.get_or_insert_with(Instant::now).elapsed().as_millis() as u64
}

/// The UI thread is alive (called from its timer).
pub fn beat() {
    BEAT_MS.store(now_ms(), Ordering::Relaxed);
}

/// Names what the UI thread does until the returned guard is dropped.
pub fn busy(what: &'static str) -> Busy {
    let mut a = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
    let before = *a;
    *a = what;
    Busy(before)
}

pub struct Busy(&'static str);

impl Drop for Busy {
    fn drop(&mut self) {
        *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) = self.0;
    }
}

/// Starts watching; writes into KilatX's log (log.rs).
pub fn start() {
    beat();
    *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) = "idle";
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(move || {
        let mut stalled_since: Option<(u64, &'static str)> = None;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = now_ms();
            let age = now.saturating_sub(BEAT_MS.load(Ordering::Relaxed));
            let what = *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
            match stalled_since {
                None if age > STALL.as_millis() as u64 => {
                    let since = now - age;
                    stalled_since = Some((since, what));
                    write(&format!("window not answering for {} s, busy with: {what}", age / 1000));
                }
                Some((since, first)) if age < 1000 => {
                    write(&format!("answering again after {} s (was busy with: {first})", (now - since) / 1000));
                    stalled_since = None;
                }
                _ => {}
            }
        }
    });
}

fn write(line: &str) {
    crate::log::warn("hang", line);
    crate::util::test_event("hang", line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_ui_thread_is_written_down_with_what_it_was_doing() {
        let dir = std::env::temp_dir().join(format!("kilatx-watchdog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::log::init(&dir);
        start();
        {
            let _b = busy("test:slow-part");
            // No heartbeat for longer than STALL
            std::thread::sleep(STALL + Duration::from_millis(1500));
        }
        beat();
        std::thread::sleep(Duration::from_millis(1200));
        let log = std::fs::read_to_string(crate::log::dir(&dir).join("kilatx.log")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(log.contains("not answering") && log.contains("test:slow-part"), "{log}");
        assert!(log.contains("answering again"), "{log}");
    }
}
