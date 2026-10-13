#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
#[cfg(test)]
mod browser_tests;
mod config;
mod desktop;
mod digest;
mod engine;
mod limiter;
mod setup_hooks;
mod update;
#[cfg(test)]
mod scenario_tests;
#[cfg(test)]
mod options_tests;
mod store;
mod util;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{Color, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use chrono::{Datelike, Timelike};

use config::{DownloadOptions, DuplicateLink, ExistingFile, ProxyMode};
use engine::{AddRequest, Engine, Item, OnDone, QueueEvent, Status, MAIN_QUEUE};

/// Speed limit choices (bytes per second) shown in the menus.
const LIMITS: [(u64, &str); 8] = [
    (0, "Unlimited"),
    (128 << 10, "128 KB/s"),
    (256 << 10, "256 KB/s"),
    (512 << 10, "512 KB/s"),
    (1 << 20, "1 MB/s"),
    (2 << 20, "2 MB/s"),
    (5 << 20, "5 MB/s"),
    (10 << 20, "10 MB/s"),
];

/// KilatX's source code (GNU GPL v3).
const SOURCE_URL: &str = "https://github.com/marcos-liao/KilatX";

/// Seconds between "queue finished" and the actual shutdown, to allow cancelling.
const SHUTDOWN_DELAY: u64 = 60;

slint::include_modules!();

struct UiState {
    speeds: HashMap<u64, f64>,
    last_bytes: HashMap<u64, u64>,
    last_tick: Instant,
    last_save: Instant,
    rows: Rc<VecModel<RowData>>,
    filters: Rc<VecModel<CategoryData>>,
    categories: Rc<VecModel<CategoryData>>,
    queues: Rc<VecModel<QueueData>>,
    queue_names: Rc<VecModel<SharedString>>,
    /// Queue currently open in the scheduler dialog.
    sched_queue: Option<u64>,
    shutdown_at: Option<Instant>,
    /// Last seen status per download, to notify when one finishes or fails.
    last_status: HashMap<u64, Status>,
    clip_seq: u32,
    /// Last link offered from the clipboard, so it isn't offered twice.
    last_offered: String,
    /// Set when KilatX itself copies a link ("Copy link"), so it isn't offered back.
    ignore_clip: bool,
    /// Download options being edited in Settings (applied on Save).
    pending: Option<DownloadOptions>,
    /// Add-dialog text the user was warned about (already in the list); a second Start adds it.
    confirm_dup: Option<String>,
    /// Download whose "file already exists" question is on screen.
    conflict_id: Option<u64>,
    /// Cookies, referrer and name of links the browser sent, until the Add dialog submits them.
    browser_pending: HashMap<String, browser::BrowserDownload>,
    /// The newer version shown in the update dialog.
    update_release: Option<update::Release>,
}

fn main() -> Result<(), slint::PlatformError> {
    setup_hooks::harden_dll_search();
    let args = absolutize_args(std::env::args().skip(1).collect());
    // Started by a browser as its native messaging host: answer and exit, no window.
    if browser::is_host_launch(&args) {
        browser::run_host_process(store::data_dir());
        return Ok(());
    }
    // Run by KilatX Setup and its uninstaller.
    if setup_hooks::is_setup_switch(&args) {
        std::process::exit(setup_hooks::run(&args));
    }
    // Single instance: decided before any window exists. A second launch only
    // hands its links to the running KilatX and quits.
    let instance = match desktop::claim_instance() {
        // "/quit" (from setup or the uninstaller) with no KilatX running: nothing to do.
        desktop::Claim::First(_) if has_switch(&args, "/quit") => return Ok(()),
        desktop::Claim::First(instance) => instance,
        desktop::Claim::AlreadyRunning => {
            if !desktop::hand_over(&args) {
                desktop::show_not_responding();
            }
            return Ok(());
        }
    };
    let data_dir = store::data_dir();
    let _ = std::fs::create_dir_all(&data_dir);
    let saved = store::load(&data_dir);
    util::event_log("start", if setup_hooks::is_elevated() { "elevated=1" } else { "elevated=0" });

    // Software rendering unless GPU rendering was turned on. If the window didn't
    // come up last time with the GPU (no usable OpenGL, as in some VMs or over
    // Remote Desktop), go back to software rendering this time and from now on.
    let marker = data_dir.join("starting.lock");
    let failed_last_start = marker.exists() && saved.settings.gpu_render;
    let gpu = saved.settings.gpu_render && !failed_last_start && !has_switch(&args, "/software");
    if !gpu {
        let _ = slint::BackendSelector::new().renderer_name("software".into()).select();
    }

    let engine = Arc::new(Engine::new(saved.items, saved.queues, saved.settings.global_limit));
    engine.set_line_peak(saved.settings.line_peak);
    if let Err(e) = engine.configure(saved.settings.download.clone()) {
        // e.g. a proxy without address: fall back to defaults rather than not starting
        eprintln!("download options not applied: {e}");
    }
    let settings = Rc::new(RefCell::new(saved.settings));
    if failed_last_start {
        settings.borrow_mut().gpu_render = false;
    }

    let ui = AppWindow::new()?;
    let tray = KilatTray::new()?;
    {
        // Later launches send their arguments here.
        let ui = ui.as_weak();
        let listening = desktop::listen(&instance, move |args| {
            let json = serde_json::to_string(&args).unwrap_or_default();
            let _ = ui.upgrade_in_event_loop(move |ui| ui.invoke_external_args(json.into()));
        });
        if listening.is_err() {
            // An older KilatX (without the mutex) owns the pipe: hand over to it.
            if !desktop::hand_over(&args) {
                desktop::show_not_responding();
            }
            return Ok(());
        }
    }
    desktop::register_app(&notification_icon(&data_dir));
    if settings.borrow().browser_capture {
        // Every start: keeps the path right when a portable copy was moved.
        if let Err(e) = browser::register(&data_dir, browser::HOST_NAME) {
            eprintln!("browser integration not registered: {e}");
        }
    }
    if settings.borrow().start_with_windows {
        // Re-registering fixes the path when a portable copy was moved.
        let _ = desktop::set_autostart(true);
    }
    if failed_last_start {
        ui.set_notice("GPU rendering failed last time: switched back to software rendering".into());
    }
    let state = Rc::new(RefCell::new(UiState {
        speeds: HashMap::new(),
        last_bytes: HashMap::new(),
        last_tick: Instant::now(),
        last_save: Instant::now(),
        rows: Rc::new(VecModel::default()),
        filters: Rc::new(VecModel::default()),
        categories: Rc::new(VecModel::default()),
        queues: Rc::new(VecModel::default()),
        queue_names: Rc::new(VecModel::default()),
        sched_queue: None,
        shutdown_at: None,
        last_status: engine.snapshot().iter().map(|i| (i.id, i.status)).collect(),
        clip_seq: desktop::clipboard_sequence(),
        last_offered: String::new(),
        ignore_clip: false,
        pending: None,
        confirm_dup: None,
        conflict_id: None,
        browser_pending: HashMap::new(),
        update_release: None,
    }));
    {
        let st = state.borrow();
        ui.set_rows(ModelRc::from(st.rows.clone()));
        ui.set_filters(ModelRc::from(st.filters.clone()));
        ui.set_categories(ModelRc::from(st.categories.clone()));
        ui.set_queues(ModelRc::from(st.queues.clone()));
        ui.set_queue_names(ModelRc::from(st.queue_names.clone()));
    }
    let limit_names: Vec<SharedString> = LIMITS.iter().map(|l| l.1.into()).collect();
    ui.set_limit_names(ModelRc::new(VecModel::from(limit_names)));
    ui.set_add_conns(settings.borrow().connections as i32);

    let save_all = {
        let engine = engine.clone();
        let settings = settings.clone();
        let data_dir = data_dir.clone();
        Rc::new(move || {
            settings.borrow_mut().line_peak = engine.line_peak();
            store::save(&data_dir, &settings.borrow(), &engine.queues(), &engine.snapshot())
        })
    };

    if failed_last_start {
        save_all(); // remember software rendering right away
    }

    let refresh = {
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        Rc::new(move || {
            if let Some(ui) = ui.upgrade() {
                refresh_ui(&ui, &engine, &state.borrow());
            }
        })
    };

    ui.on_refresh({
        let refresh = refresh.clone();
        move || refresh()
    });

    ui.on_add_clicked({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            // Pre-fill with a link from the clipboard.
            let clip = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
            let clip = clip.trim();
            let looks_like_url = clip.starts_with("http://") || clip.starts_with("https://");
            ui.set_add_url(if looks_like_url { clip.into() } else { SharedString::new() });
            ui.set_add_name(SharedString::new());
            ui.set_add_folder(SharedString::new());
            ui.set_add_error(SharedString::new());
            ui.set_show_add(true);
        }
    });

    ui.on_browse_folder({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(dir) = rfd::FileDialog::new().set_title("Save downloads to").pick_folder() {
                ui.set_add_folder(dir.to_string_lossy().to_string().into());
            }
        }
    });

    ui.on_start_new({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let settings = settings.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        let save_all = save_all.clone();
        move |start_now| {
            let Some(ui) = ui.upgrade() else { return };
            let text = ui.get_add_url().to_string();
            let mut urls: Vec<&str> = text
                .split_whitespace()
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                .collect();
            if urls.is_empty() {
                ui.set_add_error("Enter a link that starts with http:// or https://".into());
                return;
            }
            // Links already in the list (Settings › Downloads).
            let items = engine.snapshot();
            let existing = |u: &str| items.iter().find(|i| i.url == u).cloned();
            if let Some(first) = urls.iter().find_map(|u| existing(u)) {
                match engine.options().duplicate_link {
                    DuplicateLink::AddAgain => {}
                    DuplicateLink::Skip => {
                        urls.retain(|u| existing(u).is_none());
                        if urls.is_empty() {
                            ui.set_show_add(false);
                            ui.set_selected_id(first.id as i32);
                            refresh();
                            return;
                        }
                    }
                    DuplicateLink::Ask => {
                        let confirmed = state.borrow().confirm_dup.as_deref() == Some(text.as_str());
                        if !confirmed {
                            state.borrow_mut().confirm_dup = Some(text.clone());
                            ui.set_add_error(
                                format!("Already in the list: {} ({}). Press the button again to download it once more.", first.file_name, status_text(&first))
                                    .into(),
                            );
                            return;
                        }
                    }
                }
            }
            state.borrow_mut().confirm_dup = None;
            let folder = ui.get_add_folder().to_string();
            let dir = (!folder.is_empty()).then(|| PathBuf::from(folder));
            let conns = ui.get_add_conns().clamp(1, 32) as u32;
            // A typed name applies when there is exactly one link.
            let typed = ui.get_add_name().trim().to_string();
            let single = urls.len() == 1;
            let mut last = 0;
            for url in urls {
                let from_browser = state.borrow_mut().browser_pending.remove(url);
                let name = if single && !typed.is_empty() {
                    Some(typed.clone())
                } else {
                    from_browser.as_ref().map(|b| b.filename.clone()).filter(|n| !n.is_empty())
                };
                last = engine.add_request(AddRequest {
                    url: url.to_string(),
                    dir: dir.clone(),
                    connections: conns,
                    start: start_now,
                    // "Add to main queue" waits there until the queue runs.
                    queue: (!start_now).then_some(MAIN_QUEUE),
                    headers: from_browser.map(|b| b.headers()).unwrap_or_default(),
                    file_name: name,
                });
            }
            settings.borrow_mut().connections = conns;
            save_all();
            ui.set_show_add(false);
            ui.set_selected_id(last as i32);
            refresh();
        }
    });

    ui.on_resume({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move |id| {
            if id >= 0 {
                engine.start(id as u64);
            }
            refresh();
        }
    });

    ui.on_pause({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move |id| {
            if id >= 0 {
                engine.pause(id as u64);
            }
            refresh();
        }
    });

    ui.on_pause_all({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            engine.pause_all();
            refresh();
        }
    });

    ui.on_confirm_delete({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let id = ui.get_selected_id();
            if id >= 0 {
                engine.remove(id as u64, ui.get_delete_file());
            }
            ui.set_show_delete(false);
            ui.set_selected_id(-1);
            refresh();
        }
    });

    ui.on_delete_completed({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            engine.clear_completed();
            refresh();
        }
    });

    ui.on_open_file({
        let engine = engine.clone();
        move |id| {
            let Some(item) = find(&engine, id) else { return };
            if item.status == Status::Complete && item.final_path().exists() {
                let _ = open::that_detached(item.final_path());
            } else {
                reveal(&item);
            }
        }
    });

    ui.on_open_folder({
        let engine = engine.clone();
        move |id| {
            if let Some(item) = find(&engine, id) {
                reveal(&item);
            }
        }
    });

    ui.on_copy_url({
        let engine = engine.clone();
        let state = state.clone();
        move |id| {
            if let Some(item) = find(&engine, id) {
                state.borrow_mut().ignore_clip = true;
                if let Ok(mut c) = arboard::Clipboard::new() {
                    let _ = c.set_text(item.url);
                }
            }
        }
    });

    ui.on_select({
        let ui = ui.as_weak();
        let refresh = refresh.clone();
        move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.set_selected_id(id);
            }
            refresh();
        }
    });

    ui.on_start_queue({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            engine.start_queue(active_queue(&ui));
            refresh();
        }
    });

    ui.on_stop_queue({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            engine.stop_queue(active_queue(&ui));
            refresh();
        }
    });

    ui.on_open_scheduler({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let active = active_queue(&ui);
            let index = engine.queues().iter().position(|q| q.id == active).unwrap_or(0);
            load_sched(&ui, &engine, &mut state.borrow_mut(), index);
            ui.set_show_scheduler(true);
            refresh();
        }
    });

    ui.on_new_queue({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let n = engine.queues().len() + 1;
            engine.create_queue(&format!("Queue {n}"));
            load_sched(&ui, &engine, &mut state.borrow_mut(), usize::MAX);
            ui.set_show_scheduler(true);
            refresh();
        }
    });

    ui.on_sched_select({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move |index| {
            let Some(ui) = ui.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                save_sched(&ui, &engine, &st);
                load_sched(&ui, &engine, &mut st, index.max(0) as usize);
            }
            refresh();
        }
    });

    ui.on_sched_new({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                save_sched(&ui, &engine, &st);
                let n = engine.queues().len() + 1;
                engine.create_queue(&format!("Queue {n}"));
                load_sched(&ui, &engine, &mut st, usize::MAX);
            }
            refresh();
        }
    });

    ui.on_sched_delete({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                if let Some(id) = st.sched_queue {
                    engine.delete_queue(id);
                    if ui.get_current_category() == format!("q:{id}").as_str() {
                        ui.set_current_category("all".into());
                    }
                }
                load_sched(&ui, &engine, &mut st, 0);
            }
            refresh();
        }
    });

    ui.on_sched_start_now({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            {
                let st = state.borrow();
                save_sched(&ui, &engine, &st);
                if let Some(id) = st.sched_queue {
                    engine.start_queue(id);
                }
            }
            refresh();
        }
    });

    ui.on_sched_stop_now({
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move || {
            if let Some(id) = state.borrow().sched_queue {
                engine.stop_queue(id);
            }
            refresh();
        }
    });

    ui.on_sched_close({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        let save_all = save_all.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            save_sched(&ui, &engine, &state.borrow());
            ui.set_show_scheduler(false);
            save_all();
            refresh();
        }
    });

    ui.on_set_global_limit({
        let engine = engine.clone();
        let settings = settings.clone();
        let refresh = refresh.clone();
        let save_all = save_all.clone();
        move |index| {
            let limit = limit_at(index);
            engine.set_global_limit(limit);
            settings.borrow_mut().global_limit = limit;
            save_all();
            refresh();
        }
    });

    ui.on_set_item_limit({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move |id, index| {
            if id >= 0 {
                engine.set_item_limit(id as u64, limit_at(index));
            }
            refresh();
        }
    });

    ui.on_set_item_queue({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move |id, index| {
            if id >= 0 {
                // Index 0 is "No queue"; the rest follow the queue list.
                let queue = (index > 0).then(|| engine.queues().get(index as usize - 1).map(|q| q.id)).flatten();
                engine.set_item_queue(id as u64, queue);
            }
            refresh();
        }
    });

    ui.on_move_item({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move |id, earlier| {
            if id >= 0 {
                engine.move_in_queue(id as u64, earlier);
            }
            refresh();
        }
    });

    ui.on_change_address({
        let ui = ui.as_weak();
        let engine = engine.clone();
        move |id| {
            let Some(ui) = ui.upgrade() else { return };
            let Some(item) = find(&engine, id) else { return };
            // A running download has to stop before its address can change.
            engine.pause(id as u64);
            ui.set_change_url_text(item.url.into());
            ui.set_change_url_error(SharedString::new());
            ui.set_show_change_url(true);
        }
    });

    ui.on_confirm_change_url({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let id = ui.get_selected_id();
            if id < 0 {
                return;
            }
            // The pause may still be settling; give it a moment.
            let mut result = engine.set_url(id as u64, &ui.get_change_url_text());
            for _ in 0..20 {
                if result.as_ref().is_err_and(|e| e.contains("Pause")) {
                    std::thread::sleep(Duration::from_millis(100));
                    result = engine.set_url(id as u64, &ui.get_change_url_text());
                }
            }
            match result {
                Ok(()) => {
                    ui.set_show_change_url(false);
                    engine.start(id as u64);
                }
                Err(e) => ui.set_change_url_error(e.into()),
            }
            refresh();
        }
    });

    ui.on_cancel_shutdown({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            state.borrow_mut().shutdown_at = None;
            if let Some(ui) = ui.upgrade() {
                ui.set_show_shutdown(false);
            }
        }
    });


    // Shows the window (from the tray, a second launch, or a copied link). While it
    // comes up, a marker file records the attempt: if the renderer crashes, the next
    // start finds the marker and switches to software rendering.
    let marker_timer = Rc::new(Timer::default());
    let shown_once = Rc::new(std::cell::Cell::new(false));
    let show_window = {
        let ui = ui.as_weak();
        let shown_once = shown_once.clone();
        let marker = marker.clone();
        let marker_timer = marker_timer.clone();
        Rc::new(move || {
            let Some(ui) = ui.upgrade() else { return };
            // Only the first show creates the renderer; later shows (from the tray) can't crash it.
            let first_time = !shown_once.replace(true);
            if first_time {
                let _ = std::fs::write(&marker, b"");
            }
            let _ = ui.show();
            ui.window().set_minimized(false);
            desktop::bring_to_front();
            if first_time {
                let marker = marker.clone();
                marker_timer.start(TimerMode::SingleShot, Duration::from_secs(3), move || {
                    let _ = std::fs::remove_file(&marker);
                });
            }
        })
    };

    // Closing the window keeps KilatX running in the tray (unless turned off).
    ui.window().on_close_requested({
        let settings = settings.clone();
        let save_all = save_all.clone();
        move || {
            if settings.borrow().close_to_tray {
                if !settings.borrow().tray_hint_shown {
                    settings.borrow_mut().tray_hint_shown = true;
                    save_all();
                    let _ = desktop::notify(
                        "KilatX is still running",
                        "Downloads continue in the notification area. Right-click the KilatX icon to exit.",
                    );
                }
            } else {
                let _ = slint::quit_event_loop();
            }
            slint::CloseRequestResponse::HideWindow
        }
    });

    tray.on_icon_clicked({
        let show_window = show_window.clone();
        move || show_window()
    });
    tray.on_open_window({
        let show_window = show_window.clone();
        move || show_window()
    });
    tray.on_add_url({
        let ui = ui.as_weak();
        let show_window = show_window.clone();
        move || {
            show_window();
            if let Some(ui) = ui.upgrade() {
                ui.invoke_add_clicked();
            }
        }
    });
    tray.on_start_queue({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            engine.start_queue(MAIN_QUEUE);
            refresh();
        }
    });
    tray.on_pause_all({
        let engine = engine.clone();
        let refresh = refresh.clone();
        move || {
            engine.pause_all();
            refresh();
        }
    });
    tray.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });

    // A second launch (or the browser later) passed its arguments to this instance.
    ui.on_external_args({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let settings = settings.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        let show_window = show_window.clone();
        move |json| {
            let Some(ui) = ui.upgrade() else { return };
            let args: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
            // Setup or the uninstaller asks KilatX to exit (progress is saved on the way out).
            if has_switch(&args, "/quit") {
                util::event_log("quit", "");
                let _ = slint::quit_event_loop();
                return;
            }
            let links = parse_links(&args);
            let policy = engine.options().duplicate_link;
            let items = engine.snapshot();
            let (mut last, mut ask) = (None, None);

            // Downloads the browser handed over (with its cookies, referrer and name).
            let mut for_dialog = Vec::new();
            for dl in browser::from_args(&args) {
                if let Some(old) = items.iter().find(|i| i.url == dl.url) {
                    match policy {
                        DuplicateLink::AddAgain => {}
                        DuplicateLink::Skip => {
                            last = Some(old.id);
                            continue;
                        }
                        DuplicateLink::Ask => {
                            ask.get_or_insert((dl.url.clone(), old.clone()));
                            state.borrow_mut().browser_pending.insert(dl.url.clone(), dl);
                            continue;
                        }
                    }
                }
                if settings.borrow().browser_dialog {
                    for_dialog.push(dl);
                } else {
                    let id = engine.add_request(AddRequest {
                        url: dl.url.clone(),
                        connections: settings.borrow().connections,
                        start: true,
                        headers: dl.headers(),
                        file_name: Some(dl.filename.clone()).filter(|n| !n.is_empty()),
                        ..Default::default()
                    });
                    util::event_log("browser-start", &dl.url);
                    last = Some(id);
                }
            }
            if !for_dialog.is_empty() && ask.is_none() {
                let urls: Vec<String> = for_dialog.iter().map(|d| d.url.clone()).collect();
                ui.set_add_url(urls.join(" ").into());
                ui.set_add_name(if for_dialog.len() == 1 { for_dialog[0].filename.as_str().into() } else { SharedString::new() });
                ui.set_add_folder(SharedString::new());
                ui.set_add_error(SharedString::new());
                {
                    let mut st = state.borrow_mut();
                    st.confirm_dup = None;
                    for dl in for_dialog {
                        st.browser_pending.insert(dl.url.clone(), dl);
                    }
                }
                ui.set_show_add(true);
                util::event_log("add-dialog", &urls.join(" "));
                show_window();
            }
            for url in &links.urls {
                if let Some(old) = items.iter().find(|i| i.url == *url) {
                    match policy {
                        DuplicateLink::AddAgain => {}
                        DuplicateLink::Skip => {
                            last = Some(old.id);
                            continue;
                        }
                        DuplicateLink::Ask => {
                            ask.get_or_insert((url.clone(), old.clone()));
                            continue;
                        }
                    }
                }
                let queue = (!links.start).then_some(MAIN_QUEUE);
                last = Some(engine.add(url, links.folder.clone(), settings.borrow().connections, links.start, queue));
            }
            // Debug builds only: store screenshots without the detail panel (it shows local paths).
            let keep_unselected = cfg!(debug_assertions) && std::env::var_os("KILATX_PREVIEW_NOSELECT").is_some();
            if let Some(id) = last.filter(|_| !keep_unselected) {
                ui.set_selected_id(id as i32);
            }
            if let Some((url, old)) = &ask {
                // Let the user decide in the Add dialog.
                state.borrow_mut().confirm_dup = Some(url.clone());
                ui.set_add_url(url.as_str().into());
                let name = state.borrow().browser_pending.get(url).map(|b| b.filename.clone()).unwrap_or_default();
                ui.set_add_name(name.into());
                ui.set_add_folder(links.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default().into());
                ui.set_add_error(
                    format!("Already in the list: {} ({}). Press the button again to download it once more.", old.file_name, status_text(old)).into(),
                );
                ui.set_show_add(true);
            }
            if !has_switch(&args, "/tray") || ask.is_some() {
                show_window();
            }
            refresh();
        }
    });

    ui.on_open_settings({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let settings = settings.clone();
        let state = state.clone();
        let data_dir = data_dir.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let s = settings.borrow();
            ui.set_set_close_to_tray(s.close_to_tray);
            ui.set_set_start_with_windows(s.start_with_windows);
            ui.set_set_watch_clipboard(s.watch_clipboard);
            ui.set_set_notify(s.notify);
            ui.set_set_gpu_render(s.gpu_render);
            ui.set_set_connections(s.connections as i32);
            let opts = (*engine.options()).clone();
            options_to_ui(&ui, &opts);
            state.borrow_mut().pending = Some(opts);
            ui.set_settings_error(SharedString::new());
            ui.set_connection_test(SharedString::new());
            ui.set_about_version(env!("CARGO_PKG_VERSION").into());
            ui.set_about_build(format!("{}  ·  built {}", std::env::consts::ARCH, env!("KILATX_BUILD_DATE")).into());
            ui.set_about_renderer(if gpu { "GPU (OpenGL)" } else { "Software" }.into());
            ui.set_about_data(data_dir.display().to_string().into());
            ui.set_about_license("Free and open source: GNU General Public License v3".into());
            ui.set_set_check_updates(s.check_updates);
            if !ui.get_update_checking() {
                ui.set_update_check_text(SharedString::new());
            }
            ui.set_set_browser_capture(s.browser_capture);
            ui.set_set_browser_dialog(s.browser_dialog);
            sync_browser_rows(&ui, &data_dir);
            ui.set_browser_message(SharedString::new());
            ui.set_show_settings(true);
        }
    });

    // Category list edits go to the pending copy; the list isn't rebuilt while typing.
    ui.on_cat_edit_ext({
        let state = state.clone();
        move |i, text| {
            if let Some(c) = state.borrow_mut().pending.as_mut().and_then(|p| p.categories.get_mut(i as usize)) {
                c.extensions = config::parse_list(&text);
            }
        }
    });
    ui.on_cat_edit_folder({
        let state = state.clone();
        move |i, text| {
            if let Some(c) = state.borrow_mut().pending.as_mut().and_then(|p| p.categories.get_mut(i as usize)) {
                let t = text.trim();
                c.folder = (!t.is_empty()).then(|| PathBuf::from(t));
            }
        }
    });
    ui.on_cat_browse({
        let ui = ui.as_weak();
        let state = state.clone();
        move |i| {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(dir) = rfd::FileDialog::new().set_title("Folder for this category").pick_folder() {
                if let Some(p) = state.borrow_mut().pending.as_mut() {
                    if let Some(c) = p.categories.get_mut(i as usize) {
                        c.folder = Some(dir);
                    }
                    sync_category_rows(&ui, p);
                }
            }
        }
    });
    ui.on_cat_delete({
        let ui = ui.as_weak();
        let state = state.clone();
        move |i| {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(p) = state.borrow_mut().pending.as_mut() {
                if (i as usize) < p.categories.len() && !is_builtin_category(&p.categories[i as usize].name) {
                    p.categories.remove(i as usize);
                }
                sync_category_rows(&ui, p);
            }
        }
    });
    ui.on_cat_add({
        let ui = ui.as_weak();
        let state = state.clone();
        move |name, exts| {
            let Some(ui) = ui.upgrade() else { return };
            let name = name.trim().to_string();
            let mut st = state.borrow_mut();
            let Some(p) = st.pending.as_mut() else { return };
            let reserved = ["general", "other", "all"].contains(&name.to_lowercase().as_str());
            if name.is_empty() || reserved || p.categories.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
                ui.set_settings_error("Give the new category a name that isn't used yet.".into());
                return;
            }
            p.categories.push(config::CategoryDef { name, extensions: config::parse_list(&exts), folder: None });
            ui.set_settings_error(SharedString::new());
            sync_category_rows(&ui, p);
        }
    });
    ui.on_browse_base({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(dir) = rfd::FileDialog::new().set_title("Downloads folder").pick_folder() {
                ui.set_base_folder(dir.display().to_string().into());
            }
        }
    });
    ui.on_browse_temp({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(dir) = rfd::FileDialog::new().set_title("Temporary folder").pick_folder() {
                ui.set_temp_folder(dir.display().to_string().into());
            }
        }
    });
    ui.on_login_add({
        let ui = ui.as_weak();
        let state = state.clone();
        move |site, user, pass| {
            let Some(ui) = ui.upgrade() else { return };
            let site = site.trim().to_string();
            if site.is_empty() || user.trim().is_empty() {
                ui.set_settings_error("Enter the site and the username.".into());
                return;
            }
            if let Some(p) = state.borrow_mut().pending.as_mut() {
                p.logins.retain(|l| !l.pattern.eq_ignore_ascii_case(&site));
                p.logins.push(config::SiteLogin { pattern: site, username: user.trim().to_string(), password: config::Secret(pass.to_string()) });
                sync_login_rows(&ui, p);
            }
            ui.set_settings_error(SharedString::new());
        }
    });
    ui.on_login_delete({
        let ui = ui.as_weak();
        let state = state.clone();
        move |i| {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(p) = state.borrow_mut().pending.as_mut() {
                if (i as usize) < p.logins.len() {
                    p.logins.remove(i as usize);
                }
                sync_login_rows(&ui, p);
            }
        }
    });

    // Tries the connection with the proxy settings as they are in the form right now.
    ui.on_test_connection({
        let ui = ui.as_weak();
        let state = state.clone();
        move || {
            let Some(ui_strong) = ui.upgrade() else { return };
            let base = state.borrow().pending.clone().unwrap_or_default();
            let opts = match options_from_ui(&ui_strong, &base) {
                Ok(o) => o,
                Err(e) => {
                    ui_strong.set_connection_test(e.into());
                    return;
                }
            };
            ui_strong.set_connection_test("Testing…".into());
            let ui = ui.clone();
            std::thread::spawn(move || {
                let result = test_connection(&opts);
                let _ = ui.upgrade_in_event_loop(move |ui| ui.set_connection_test(result.into()));
            });
        }
    });

    ui.on_show_document({
        let ui = ui.as_weak();
        let data_dir = data_dir.clone();
        move |which| {
            let Some(ui) = ui.upgrade() else { return };
            let (title, text) = match which {
                0 => ("License: GNU GPL v3", include_str!("../../LICENSE")),
                1 => ("Privacy policy", include_str!("../docs/PRIVACY.txt")),
                3 => {
                    let _ = open::that_detached(SOURCE_URL);
                    return;
                }
                _ => {
                    // Large (hundreds of licenses): open it in the text editor instead.
                    let path = data_dir.join("THIRD-PARTY-NOTICES.txt");
                    let _ = std::fs::write(&path, include_str!("../docs/THIRD-PARTY-NOTICES.txt"));
                    let _ = open::that_detached(&path);
                    return;
                }
            };
            ui.set_doc_title(title.into());
            ui.set_doc_text(text.into());
            ui.set_show_doc(true);
        }
    });

    ui.on_resolve_conflict({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let refresh = refresh.clone();
        move |choice| {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(id) = state.borrow_mut().conflict_id.take() {
                let choice = match choice {
                    1 => ExistingFile::Overwrite,
                    2 => ExistingFile::Skip,
                    _ => ExistingFile::Rename,
                };
                engine.resolve_conflict(id, choice);
            }
            ui.set_show_conflict(false);
            refresh();
        }
    });

    ui.on_browser_install({
        let ui = ui.as_weak();
        let data_dir = data_dir.clone();
        move |index| {
            let Some(ui) = ui.upgrade() else { return };
            let Some(b) = browser::BROWSERS.get(index as usize) else { return };
            let message = match browser::write_extension(&data_dir) {
                Ok(dir) => {
                    // Path ready to paste into "Load unpacked"; the folder opens next to it.
                    if let Ok(mut c) = arboard::Clipboard::new() {
                        let _ = c.set_text(dir.display().to_string());
                    }
                    let _ = open::that_detached(&dir);
                    match b.open_extensions_page() {
                        Ok(()) => format!(
                            "{}: turn on \"Developer mode\", click \"Load unpacked\" and paste the folder path (already copied). The folder is also open in Explorer.",
                            b.name
                        ),
                        Err(e) => e,
                    }
                }
                Err(e) => e,
            };
            ui.set_browser_message(message.into());
        }
    });
    ui.on_browser_open_folder({
        let data_dir = data_dir.clone();
        move || {
            if let Ok(dir) = browser::write_extension(&data_dir) {
                let _ = open::that_detached(dir);
            }
        }
    });
    ui.on_remove_traces({
        let settings = settings.clone();
        let save_all = save_all.clone();
        let data_dir = data_dir.clone();
        move || {
            browser::unregister(&data_dir, browser::HOST_NAME);
            desktop::remove_traces();
            {
                // Stay off until turned on again, so the next start doesn't re-register.
                let mut s = settings.borrow_mut();
                s.browser_capture = false;
                s.start_with_windows = false;
            }
            save_all();
            util::event_log("traces-removed", "");
            let _ = slint::quit_event_loop();
        }
    });

    ui.on_save_settings({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let settings = settings.clone();
        let data_dir = data_dir.clone();
        let state = state.clone();
        let save_all = save_all.clone();
        let refresh = refresh.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let base = state.borrow().pending.clone().unwrap_or_default();
            let opts = match options_from_ui(&ui, &base) {
                Ok(o) => o,
                Err(e) => {
                    ui.set_settings_error(e.into());
                    return;
                }
            };
            if let Err(e) = engine.configure(opts.clone()) {
                ui.set_settings_error(e.into());
                return;
            }
            settings.borrow_mut().download = opts;
            state.borrow_mut().pending = None;
            {
                let mut s = settings.borrow_mut();
                s.close_to_tray = ui.get_set_close_to_tray();
                s.watch_clipboard = ui.get_set_watch_clipboard();
                s.notify = ui.get_set_notify();
                s.gpu_render = ui.get_set_gpu_render();
                s.connections = ui.get_set_connections().clamp(1, 32) as u32;
                let autostart = ui.get_set_start_with_windows();
                if autostart != s.start_with_windows {
                    let _ = desktop::set_autostart(autostart);
                }
                s.start_with_windows = autostart;
                s.browser_dialog = ui.get_set_browser_dialog();
                let capture = ui.get_set_browser_capture();
                if capture != s.browser_capture {
                    if capture {
                        let _ = browser::register(&data_dir, browser::HOST_NAME);
                    } else {
                        browser::unregister(&data_dir, browser::HOST_NAME);
                    }
                }
                s.browser_capture = capture;
                s.check_updates = ui.get_set_check_updates();
            }
            ui.set_add_conns(settings.borrow().connections as i32);
            ui.set_show_settings(false);
            save_all();
            refresh();
        }
    });

    // Updates: KilatX looks for a new version (once a day, or when asked) and asks first.
    ui.on_check_updates({
        let ui = ui.as_weak();
        let engine = engine.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            ui.set_update_checking(true);
            ui.set_update_check_text(SharedString::new());
            spawn_update_check(ui.as_weak(), engine.options().as_ref().clone(), true);
        }
    });
    ui.on_update_event({
        let ui = ui.as_weak();
        let settings = settings.clone();
        let state = state.clone();
        let save_all = save_all.clone();
        move |json| {
            let Some(ui) = ui.upgrade() else { return };
            let ev: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
            let text = |k: &str| ev[k].as_str().unwrap_or_default().to_string();
            match ev["kind"].as_str().unwrap_or_default() {
                "checked" => {
                    let manual = ev["manual"].as_bool().unwrap_or(false);
                    ui.set_update_checking(false);
                    let release: Option<update::Release> = serde_json::from_value(ev["release"].clone()).ok();
                    let Some(release) = release else {
                        util::event_log("update-check-failed", &text("error"));
                        if manual {
                            ui.set_update_check_text(text("error").into());
                        }
                        return;
                    };
                    settings.borrow_mut().last_update_check = chrono::Utc::now().timestamp();
                    save_all();
                    let skipped = settings.borrow().skipped_version.clone();
                    match update::evaluate(release, env!("CARGO_PKG_VERSION"), &skipped, manual) {
                        update::Found::UpToDate => {
                            util::event_log("update-none", "");
                            if manual {
                                ui.set_update_check_text(format!("KilatX is up to date (version {}).", env!("CARGO_PKG_VERSION")).into());
                            }
                        }
                        update::Found::Newer(r) => {
                            util::event_log("update-available", &r.version);
                            ui.set_update_check_text(format!("Version {} is available.", r.version).into());
                            ui.set_update_version(r.version.as_str().into());
                            ui.set_update_current(env!("CARGO_PKG_VERSION").into());
                            ui.set_update_notes(r.notes.as_str().into());
                            ui.set_update_status(SharedString::new());
                            ui.set_update_busy(false);
                            ui.set_update_portable(store::is_portable());
                            ui.set_show_update(true);
                            if !manual && settings.borrow().notify {
                                let _ = desktop::notify(&format!("KilatX {} is available", r.version), "Open KilatX to update.");
                            }
                            state.borrow_mut().update_release = Some(r);
                            // Debug builds only: end-to-end tests accept the update without a click.
                            #[cfg(debug_assertions)]
                            if std::env::var_os("KILATX_UPDATE_AUTO").is_some() {
                                ui.invoke_update_now();
                            }
                        }
                    }
                }
                "progress" => ui.set_update_status(format!("Downloading… {}%", ev["percent"].as_u64().unwrap_or(0)).into()),
                "started" => {
                    util::event_log("update-started", &text("version"));
                    ui.set_update_status("Installing… KilatX closes and starts again when it is done.".into());
                }
                _ => {
                    util::event_log("update-failed", &text("error"));
                    ui.set_update_busy(false);
                    ui.set_update_status(text("error").into());
                }
            }
        }
    });
    ui.on_update_now({
        let ui = ui.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        move || {
            let Some(ui_strong) = ui.upgrade() else { return };
            let Some(release) = state.borrow().update_release.clone() else { return };
            if store::is_portable() {
                // A portable copy isn't installed: the user replaces KilatX.exe from the page.
                let page = if release.page.starts_with("https://") { release.page.clone() } else { String::new() };
                if !page.is_empty() {
                    let _ = open::that_detached(&page);
                }
                ui_strong.set_show_update(false);
                return;
            }
            let Some(file) = release.file().cloned() else { return };
            ui_strong.set_update_busy(true);
            ui_strong.set_update_status("Downloading…".into());
            let options = engine.options().as_ref().clone();
            let ui = ui.clone();
            std::thread::spawn(move || {
                let post = |v: serde_json::Value| {
                    let json = v.to_string();
                    let _ = ui.upgrade_in_event_loop(move |ui| ui.invoke_update_event(json.into()));
                };
                let result = engine::build_client(&options).and_then(|client| {
                    let dir = std::env::temp_dir().join("KilatX-update");
                    let path = update::download(&client, &file, &release.version, &dir, &|p| {
                        post(serde_json::json!({"kind": "progress", "percent": p}));
                    })?;
                    update::run_setup(&path, &file.sha256)
                });
                match result {
                    Ok(()) => post(serde_json::json!({"kind": "started", "version": release.version})),
                    Err(e) => post(serde_json::json!({"kind": "failed", "error": e})),
                }
            });
        }
    });
    ui.on_update_later({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_show_update(false);
            }
        }
    });
    ui.on_update_skip({
        let ui = ui.as_weak();
        let settings = settings.clone();
        let save_all = save_all.clone();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            settings.borrow_mut().skipped_version = ui.get_update_version().to_string();
            save_all();
            ui.set_show_update(false);
        }
    });
    {
        let s = settings.borrow();
        if s.check_updates && update::check_due(s.last_update_check, chrono::Utc::now().timestamp()) {
            // A few seconds after start, so it never slows down opening KilatX.
            let ui = ui.as_weak();
            let options = engine.options().as_ref().clone();
            Timer::single_shot(Duration::from_secs(5), move || spawn_update_check(ui, options, false));
        }
    }


    // Twice a second: run queues and the scheduler, update speeds, redraw,
    // and save progress every few seconds.
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(500), {
        let ui = ui.as_weak();
        let tray = tray.as_weak();
        let engine = engine.clone();
        let state = state.clone();
        let settings = settings.clone();
        let refresh = refresh.clone();
        let save_all = save_all.clone();
        let show_window = show_window.clone();
        let repaint_hooked = std::cell::Cell::new(false);
        move || {
            let Some(ui) = ui.upgrade() else { return };
            // Once the window exists: draw all of it again whenever Windows asks for that.
            if !repaint_hooked.get() {
                let weak = ui.as_weak();
                repaint_hooked.set(desktop::watch_os_repaints(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_repaint_nonce(ui.get_repaint_nonce().wrapping_add(1));
                    }
                }));
            }
            for event in engine.tick(chrono::Local::now()) {
                let QueueEvent::Finished { name, on_done } = event;
                match on_done {
                    OnDone::Nothing => {}
                    OnDone::ExitApp => {
                        let _ = slint::quit_event_loop();
                    }
                    OnDone::Shutdown => {
                        state.borrow_mut().shutdown_at = Some(Instant::now() + Duration::from_secs(SHUTDOWN_DELAY));
                        ui.set_shutdown_title(format!("\"{name}\" has finished").into());
                        ui.set_show_shutdown(true);
                    }
                }
            }
            let shutdown_at = state.borrow().shutdown_at;
            if let Some(at) = shutdown_at {
                let left = at.saturating_duration_since(Instant::now()).as_secs();
                ui.set_shutdown_left(left as i32);
                if left == 0 {
                    state.borrow_mut().shutdown_at = None;
                    engine.shutdown();
                    save_all();
                    #[cfg(windows)]
                    let _ = std::process::Command::new("shutdown").args(["/s", "/t", "5"]).spawn();
                    let _ = slint::quit_event_loop();
                    return;
                }
            }
            let items = engine.snapshot();

            // Notify when a download finishes or fails.
            {
                let notify = settings.borrow().notify;
                let mut st = state.borrow_mut();
                for it in &items {
                    // Downloads present at start-up are pre-filled; anything new counts as
                    // just queued, so one that finished within half a second still notifies.
                    let before = st.last_status.insert(it.id, it.status).unwrap_or(Status::Queued);
                    if !notify || before == it.status {
                        continue;
                    }
                    let size = it.size.map(util::fmt_size).unwrap_or_default();
                    util::event_log("notify", &format!("{:?}	{}", it.status, it.file_name));
                    let _ = match it.status {
                        Status::Complete => desktop::notify("Download complete", &format!("{}  ·  {size}", it.file_name)),
                        Status::Error => desktop::notify(
                            "Download failed",
                            &format!("{}: {}", it.file_name, it.error.as_deref().unwrap_or("unknown error")),
                        ),
                        _ => Ok(()),
                    };
                }
            }

            // Clipboard: a copied link to a file opens the Add dialog.
            let offer = {
                let mut st = state.borrow_mut();
                let seq = desktop::clipboard_sequence();
                let changed = seq != st.clip_seq;
                st.clip_seq = seq;
                let ours = std::mem::take(&mut st.ignore_clip) && changed;
                if changed && !ours && settings.borrow().watch_clipboard && !ui.get_show_add() {
                    let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
                    let text = text.trim().to_string();
                    let new_link = looks_like_download(&text, &engine.options()) && text != st.last_offered && !items.iter().any(|i| i.url == text);
                    new_link.then(|| {
                        st.last_offered = text.clone();
                        text
                    })
                } else {
                    None
                }
            };
            if let Some(url) = offer {
                util::event_log("clipboard-offer", &url);
                ui.set_add_url(url.into());
                ui.set_add_folder(SharedString::new());
                ui.set_add_error(SharedString::new());
                ui.set_show_add(true);
                show_window();
            }

            if let Some(tray) = tray.upgrade() {
                tray.set_tip(tray_tip(&items, &state.borrow()).into());
            }

            // A download waits because its file exists and the policy is "Ask me".
            if !ui.get_show_conflict() {
                if let Some(it) = items.iter().find(|i| i.conflict.is_some()) {
                    state.borrow_mut().conflict_id = Some(it.id);
                    ui.set_conflict_name(it.conflict.clone().unwrap_or_default().into());
                    ui.set_conflict_folder(it.save_dir.display().to_string().into());
                    ui.set_show_conflict(true);
                    show_window();
                }
            }

            let save_now = {
                let mut st = state.borrow_mut();
                update_speeds(&mut st, &items);
                let due = engine.dirty.load(Ordering::Relaxed) && st.last_save.elapsed() > Duration::from_secs(3);
                if due {
                    engine.dirty.store(false, Ordering::Relaxed);
                    st.last_save = Instant::now();
                }
                due
            };
            if save_now {
                save_all();
            }
            refresh();
        }
    });

    if args.iter().any(|a| a.eq_ignore_ascii_case("/d") || a.eq_ignore_ascii_case("-d")) {
        ui.invoke_external_args(serde_json::to_string(&args).unwrap_or_default().into());
    }
    refresh();
    let _ = tray.show();
    if !has_switch(&args, "/tray") {
        show_window();
    }
    // Debug builds only: open a Settings page directly (used for screenshots).
    #[cfg(debug_assertions)]
    if let Ok(page) = std::env::var("KILATX_PREVIEW_PAGE") {
        ui.invoke_open_settings();
        ui.set_settings_page(page.parse().unwrap_or(0));
    }
    // Keeps running while the window is hidden in the tray, until "Exit".
    slint::run_event_loop_until_quit()?;
    drop(instance);

    let _ = tray.hide();
    engine.shutdown();
    save_all();
    let _ = std::fs::remove_file(&marker);
    Ok(())
}

/// Makes a `/p` folder absolute, so it still means the same folder when the
/// arguments are handed to an already running KilatX.
fn absolutize_args(mut args: Vec<String>) -> Vec<String> {
    for i in 1..args.len() {
        if args[i - 1].eq_ignore_ascii_case("/p") || args[i - 1].eq_ignore_ascii_case("-p") {
            if let Ok(abs) = std::path::absolute(&args[i]) {
                args[i] = abs.to_string_lossy().to_string();
            }
        }
    }
    args
}

/// Checks for an update in the background; the answer comes back as an `update-event`.
fn spawn_update_check(ui: slint::Weak<AppWindow>, options: DownloadOptions, manual: bool) {
    std::thread::spawn(move || {
        let result = engine::build_client(&options).and_then(|client| update::check(&client, &update::update_url()));
        let ev = match result {
            Ok(release) => serde_json::json!({"kind": "checked", "manual": manual, "release": release}),
            Err(e) => serde_json::json!({"kind": "checked", "manual": manual, "error": e}),
        };
        let json = ev.to_string();
        let _ = ui.upgrade_in_event_loop(move |ui| ui.invoke_update_event(json.into()));
    });
}

fn has_switch(args: &[String], switch: &str) -> bool {
    let alt = switch.replacen('/', "-", 1);
    args.iter().any(|a| a.eq_ignore_ascii_case(switch) || a.eq_ignore_ascii_case(&alt))
}

/// A copied link worth offering: http(s), one line, one of the captured file
/// types, and not from an excepted site (Settings › File types).
fn looks_like_download(text: &str, options: &config::DownloadOptions) -> bool {
    (text.starts_with("http://") || text.starts_with("https://"))
        && text.len() < 2048
        && !text.chars().any(char::is_whitespace)
        && options.wants(text)
}

fn tray_tip(items: &[Item], st: &UiState) -> String {
    let active = items.iter().filter(|i| i.is_running()).count();
    if active == 0 {
        return "KilatX".into();
    }
    let speed: f64 = st.speeds.values().sum();
    format!("KilatX  ·  {active} downloading  ·  {}", util::fmt_speed(speed))
}

/// The icon file notifications show (Windows needs a path, not embedded data).
fn notification_icon(data_dir: &Path) -> PathBuf {
    let path = data_dir.join("kilatx.png");
    if !path.exists() {
        let _ = std::fs::write(&path, include_bytes!("../ui/icons/app-256.png"));
    }
    path
}

/// Links on the command line: `KilatX.exe /d URL [/d URL2] [/p folder] [/a]`
/// (`/a` adds them to the main queue without starting).
struct Links {
    urls: Vec<String>,
    folder: Option<PathBuf>,
    start: bool,
}

fn parse_links(args: &[String]) -> Links {
    let mut links = Links { urls: Vec::new(), folder: None, start: true };
    let mut i = 0;
    while i < args.len() {
        match args[i].to_lowercase().as_str() {
            "/d" | "-d" if i + 1 < args.len() => {
                let url = args[i + 1].trim().to_string();
                if url.starts_with("http://") || url.starts_with("https://") {
                    links.urls.push(url);
                }
                i += 1;
            }
            "/p" | "-p" if i + 1 < args.len() => {
                links.folder = Some(PathBuf::from(&args[i + 1]));
                i += 1;
            }
            "/a" | "-a" => links.start = false,
            _ => {}
        }
        i += 1;
    }
    links
}

fn sync_browser_rows(ui: &AppWindow, data_dir: &Path) {
    let rows: Vec<BrowserRow> = browser::BROWSERS
        .iter()
        .map(|b| BrowserRow { name: b.name.into(), installed: b.exe_path().is_some() })
        .collect();
    ui.set_browser_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_browser_registered(browser::is_registered(data_dir, browser::HOST_NAME) || browser::is_registered_for_machine(browser::HOST_NAME));
    ui.set_browser_other(browser::other_download_manager_present());
    ui.set_browser_folder(browser::extension_dir(data_dir).display().to_string().into());
}

fn is_builtin_category(name: &str) -> bool {
    config::default_categories().iter().any(|c| c.name == name)
}

fn sync_category_rows(ui: &AppWindow, opts: &DownloadOptions) {
    let rows: Vec<CategoryRow> = opts
        .categories
        .iter()
        .map(|c| CategoryRow {
            name: c.name.clone().into(),
            extensions: c.extensions.join(" ").into(),
            folder: c.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default().into(),
            builtin: is_builtin_category(&c.name),
        })
        .collect();
    ui.set_cat_rows(ModelRc::new(VecModel::from(rows)));
}

fn sync_login_rows(ui: &AppWindow, opts: &DownloadOptions) {
    let rows: Vec<LoginRow> =
        opts.logins.iter().map(|l| LoginRow { pattern: l.pattern.clone().into(), username: l.username.clone().into() }).collect();
    ui.set_login_rows(ModelRc::new(VecModel::from(rows)));
}

/// Fills the Settings pages from the options.
fn options_to_ui(ui: &AppWindow, o: &DownloadOptions) {
    sync_category_rows(ui, o);
    sync_login_rows(ui, o);
    ui.set_capture_types(o.capture_types.join(" ").into());
    ui.set_capture_exceptions(o.capture_exceptions.join(", ").into());
    ui.set_base_folder(o.base_folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default().into());
    ui.set_default_base_folder(DownloadOptions::default().base().display().to_string().into());
    ui.set_temp_enabled(o.temp_folder.is_some());
    ui.set_temp_folder(o.temp_folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default().into());
    ui.set_existing_index(match o.existing_file {
        ExistingFile::Rename => 0,
        ExistingFile::Overwrite => 1,
        ExistingFile::Skip => 2,
        ExistingFile::Ask => 3,
    });
    ui.set_duplicate_index(match o.duplicate_link {
        DuplicateLink::Ask => 0,
        DuplicateLink::AddAgain => 1,
        DuplicateLink::Skip => 2,
    });
    let agent = o.user_agent.trim();
    let preset = config::AGENT_PRESETS.iter().position(|(_, a)| *a == agent);
    ui.set_agent_index(match (agent.is_empty(), preset) {
        (true, _) => 0,
        (false, Some(i)) => i as i32 + 1,
        (false, None) => 4,
    });
    ui.set_agent_custom(if preset.is_none() { agent.to_string() } else { String::new() }.into());
    let p = &o.proxy;
    ui.set_proxy_index(match p.mode {
        ProxyMode::System => 0,
        ProxyMode::None => 1,
        ProxyMode::Http => 2,
        ProxyMode::Socks5 => 3,
    });
    ui.set_proxy_host(p.host.clone().into());
    ui.set_proxy_port(if p.port == 0 { String::new() } else { p.port.to_string() }.into());
    ui.set_proxy_user(p.username.clone().into());
    ui.set_proxy_pass(p.password.0.clone().into());
    ui.set_proxy_bypass(p.bypass.join(", ").into());
}

/// Reads the Settings pages; lists (categories, logins) come from `base`.
fn options_from_ui(ui: &AppWindow, base: &DownloadOptions) -> Result<DownloadOptions, String> {
    let mut o = base.clone();
    o.capture_types = config::parse_list(&ui.get_capture_types());
    o.capture_exceptions = config::parse_list(&ui.get_capture_exceptions());
    let folder = |s: SharedString| {
        let t = s.trim().to_string();
        (!t.is_empty()).then(|| PathBuf::from(t))
    };
    o.base_folder = folder(ui.get_base_folder());
    o.temp_folder = if ui.get_temp_enabled() {
        Some(folder(ui.get_temp_folder()).ok_or("Choose the temporary folder, or turn it off.")?)
    } else {
        None
    };
    o.existing_file = match ui.get_existing_index() {
        1 => ExistingFile::Overwrite,
        2 => ExistingFile::Skip,
        3 => ExistingFile::Ask,
        _ => ExistingFile::Rename,
    };
    o.duplicate_link = match ui.get_duplicate_index() {
        1 => DuplicateLink::AddAgain,
        2 => DuplicateLink::Skip,
        _ => DuplicateLink::Ask,
    };
    o.user_agent = match ui.get_agent_index() {
        0 => String::new(),
        i @ 1..=3 => config::AGENT_PRESETS[i as usize - 1].1.to_string(),
        _ => ui.get_agent_custom().trim().to_string(),
    };
    o.proxy.mode = match ui.get_proxy_index() {
        1 => ProxyMode::None,
        2 => ProxyMode::Http,
        3 => ProxyMode::Socks5,
        _ => ProxyMode::System,
    };
    o.proxy.host = ui.get_proxy_host().trim().to_string();
    let port = ui.get_proxy_port().trim().to_string();
    o.proxy.port = if port.is_empty() { 0 } else { port.parse().map_err(|_| "The proxy port must be a number from 1 to 65535.")? };
    o.proxy.username = ui.get_proxy_user().trim().to_string();
    o.proxy.password = config::Secret(ui.get_proxy_pass().to_string());
    o.proxy.bypass = config::parse_list(&ui.get_proxy_bypass());
    if matches!(o.proxy.mode, ProxyMode::Http | ProxyMode::Socks5) && (o.proxy.host.is_empty() || o.proxy.port == 0) {
        return Err("Enter the proxy address and port.".into());
    }
    Ok(o)
}

/// Fetches Microsoft's connectivity-check page through the given settings.
fn test_connection(opts: &DownloadOptions) -> String {
    let client = match engine::build_client(opts) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return e.to_string(),
    };
    let started = Instant::now();
    rt.block_on(async {
        match client.get("http://www.msftconnecttest.com/connecttest.txt").send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(t) if t.contains("Microsoft Connect Test") => {
                    format!("Connected ({} ms).", started.elapsed().as_millis())
                }
                Ok(_) => "Connected, but something in between changed the page (a login page or filter?).".into(),
                Err(e) => format!("Connection broke: {e}"),
            },
            Ok(r) if r.status().as_u16() == 407 => "The proxy needs a login: enter the username and password.".into(),
            Ok(r) => format!("Unexpected answer: {}", r.status()),
            Err(e) if e.is_connect() && matches!(opts.proxy.mode, ProxyMode::Http | ProxyMode::Socks5) => {
                "Could not connect to the proxy. Check its address, port and login.".into()
            }
            Err(e) if e.is_timeout() => "No answer (timed out).".into(),
            Err(e) => format!("Failed: {e}"),
        }
    })
}

fn find(engine: &Engine, id: i32) -> Option<Item> {
    engine.snapshot().into_iter().find(|i| i.id as i32 == id)
}

/// Opens Explorer with the file selected (or just the folder if the file isn't there yet).
fn reveal(item: &Item) {
    let path = if item.final_path().exists() {
        item.final_path()
    } else if item.part_path().exists() {
        item.part_path()
    } else {
        let _ = open::that_detached(&item.save_dir);
        return;
    };
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = open::that_detached(&item.save_dir);
    }
}

fn update_speeds(st: &mut UiState, items: &[Item]) {
    let dt = st.last_tick.elapsed().as_secs_f64().max(0.05);
    st.last_tick = Instant::now();
    for it in items {
        if it.status == Status::Downloading {
            let now = it.downloaded();
            let prev = st.last_bytes.get(&it.id).copied().unwrap_or(now);
            let inst = now.saturating_sub(prev) as f64 / dt;
            let speed = st.speeds.entry(it.id).or_insert(inst);
            *speed = *speed * 0.7 + inst * 0.3; // smooth out jitter
            st.last_bytes.insert(it.id, now);
        } else {
            st.speeds.remove(&it.id);
            st.last_bytes.remove(&it.id);
        }
    }
}

fn state_code(s: Status) -> i32 {
    match s {
        Status::Queued => 0,
        Status::Connecting => 1,
        Status::Downloading => 2,
        Status::Paused => 3,
        Status::Complete => 4,
        Status::Error => 5,
    }
}

fn progress(it: &Item) -> f32 {
    match (it.status, it.size) {
        (Status::Complete, _) => 1.0,
        (_, Some(size)) if size > 0 => (it.downloaded() as f64 / size as f64).min(1.0) as f32,
        _ => 0.0,
    }
}

fn ext_label(name: &str) -> String {
    let ext = util::extension(name).to_uppercase();
    if ext.is_empty() || ext.len() > 4 {
        "FILE".into()
    } else {
        ext
    }
}

fn status_text(it: &Item) -> String {
    let pct = (progress(it) * 100.0) as u32;
    let done = util::fmt_size(it.downloaded());
    match it.status {
        Status::Queued => "Queued".into(),
        Status::Connecting => "Connecting…".into(),
        Status::Downloading => match it.size {
            Some(size) => format!("{pct}%  ·  {done} of {}", util::fmt_size(size)),
            None => format!("{done} downloaded"),
        },
        Status::Paused if it.size.is_some() => format!("Paused  ·  {pct}%"),
        Status::Paused => "Paused".into(),
        Status::Complete => "Complete".into(),
        Status::Error => format!("Error: {}", it.error.as_deref().unwrap_or("unknown")),
    }
}

fn eta(it: &Item, speed: f64) -> String {
    match it.size {
        Some(size) if it.status == Status::Downloading && speed > 1.0 => {
            util::fmt_eta(size.saturating_sub(it.downloaded()) as f64 / speed)
        }
        _ => String::new(),
    }
}

fn to_row(it: &Item, st: &UiState) -> RowData {
    let speed = st.speeds.get(&it.id).copied().unwrap_or(0.0);
    RowData {
        id: it.id as i32,
        name: it.file_name.clone().into(),
        ext: ext_label(&it.file_name).into(),
        category: it.category.clone().into(),
        size: it.size.map(util::fmt_size).unwrap_or_else(|| "—".into()).into(),
        status: status_text(it).into(),
        state: state_code(it.status),
        progress: progress(it),
        speed: if it.status == Status::Downloading { util::fmt_speed(speed) } else { String::new() }.into(),
        eta: eta(it, speed).into(),
        added: util::fmt_time(it.added_at).into(),
    }
}

fn to_detail(it: &Item, st: &UiState) -> DetailData {
    let speed = st.speeds.get(&it.id).copied().unwrap_or(0.0);
    let segments: Vec<SegBar> = match it.size {
        Some(size) if size > 0 && it.resumable => it
            .segments
            .iter()
            .map(|s| {
                let len = s.end.saturating_sub(s.start).max(1) as f32;
                SegBar {
                    x: s.start as f32 / size as f32,
                    w: len / size as f32,
                    done: (s.pos.saturating_sub(s.start) as f32 / len).min(1.0),
                }
            })
            .collect(),
        _ => vec![SegBar { x: 0.0, w: 1.0, done: progress(it) }],
    };
    let resumable = if it.segments.is_empty() {
        "Not checked yet"
    } else if it.resumable {
        "Supported"
    } else {
        "Not supported by server"
    };
    let connections = if it.status == Status::Downloading {
        format!("{} active  ·  {} max", it.active_connections(), it.connections)
    } else {
        format!("{} max", it.connections)
    };
    DetailData {
        visible: true,
        id: it.id as i32,
        name: it.file_name.clone().into(),
        ext: ext_label(&it.file_name).into(),
        category: it.category.clone().into(),
        url: it.url.clone().into(),
        path: it.final_path().to_string_lossy().to_string().into(),
        size: it.size.map(util::fmt_size).unwrap_or_default().into(),
        downloaded: util::fmt_size(it.downloaded()).into(),
        speed: if it.status == Status::Downloading { util::fmt_speed(speed) } else { String::new() }.into(),
        eta: eta(it, speed).into(),
        status: if it.status == Status::Error { "Error".to_string() } else { status_text(it) }.into(),
        state: state_code(it.status),
        resumable: resumable.into(),
        connections: connections.into(),
        progress: progress(it),
        in_queue: it.queue_id.is_some(),
        error: if it.status == Status::Error { it.error.clone().unwrap_or_default() } else { String::new() }.into(),
        segments: ModelRc::new(VecModel::from(segments)),
    }
}

fn matches_filter(it: &Item, key: &str) -> bool {
    match key {
        "all" => true,
        "downloading" => it.is_running(),
        "unfinished" => it.status != Status::Complete,
        "complete" => it.status == Status::Complete,
        key if key.starts_with("q:") => it.queue_id.is_some_and(|q| key[2..].parse() == Ok(q)),
        cat => it.category == cat,
    }
}

fn cat_color(cat: &str) -> Color {
    let rgb = match cat {
        "Video" => 0x8b5cf6,
        "Music" => 0xec4899,
        "Compressed" => 0xf59e0b,
        "Documents" => 0x3b82f6,
        "Programs" => 0x10b981,
        _ => 0x64748b,
    };
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Replaces the rows in place so the list keeps its scroll position.
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() == rows.len() {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
    } else {
        model.set_vec(rows);
    }
}

fn refresh_ui(ui: &AppWindow, engine: &Engine, st: &UiState) {
    let items = engine.snapshot();
    let key = ui.get_current_category().to_string();
    let query = ui.get_search_text().to_lowercase();
    let count = |f: &dyn Fn(&Item) -> bool| items.iter().filter(|i| f(i)).count() as i32;

    let entry = |key: &str, label: &str, count: i32, color: Color| CategoryData {
        key: key.into(),
        label: label.into(),
        count,
        color,
    };
    let accent = Color::from_rgb_u8(0x38, 0x8b, 0xfd);
    sync_model(
        &st.filters,
        vec![
            entry("all", "All downloads", items.len() as i32, Color::from_rgb_u8(0x94, 0xa3, 0xb8)),
            entry("downloading", "Downloading", count(&|i| i.is_running()), accent),
            entry("unfinished", "Unfinished", count(&|i| i.status != Status::Complete), Color::from_rgb_u8(0xf5, 0x9e, 0x0b)),
            entry("complete", "Completed", count(&|i| i.status == Status::Complete), Color::from_rgb_u8(0x10, 0xb9, 0x81)),
        ],
    );
    let options = engine.options();
    let names = options.categories.iter().map(|c| c.name.as_str()).chain([config::OTHER]);
    sync_model(
        &st.categories,
        names
            .map(|c| {
                let label = if c == config::OTHER { "Other" } else { c };
                entry(c, label, count(&|i| i.category == c), cat_color(c))
            })
            .collect(),
    );

    let mut visible: Vec<&Item> = items
        .iter()
        .filter(|i| matches_filter(i, &key))
        .filter(|i| query.is_empty() || i.file_name.to_lowercase().contains(&query))
        .collect();
    visible.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(b.id.cmp(&a.id)));
    sync_model(&st.rows, visible.iter().map(|i| to_row(i, st)).collect());

    let queues = engine.queues();
    let in_queue = |q: u64| items.iter().filter(|i| i.queue_id == Some(q) && i.status != Status::Complete).count();
    sync_model(
        &st.queues,
        queues
            .iter()
            .map(|q| QueueData { id: q.id as i32, name: q.name.clone().into(), count: in_queue(q.id) as i32, running: q.running })
            .collect(),
    );
    let mut names: Vec<SharedString> = vec!["No queue".into()];
    names.extend(queues.iter().map(|q| SharedString::from(q.name.as_str())));
    sync_model(&st.queue_names, names);

    let active = active_queue(ui);
    ui.set_active_queue_running(queues.iter().any(|q| q.id == active && q.running));
    let global = engine.global_limit();
    ui.set_global_limit_index(limit_index(global));
    ui.set_global_limit_text(if global == 0 { "No limit".into() } else { util::fmt_speed(global as f64).into() });

    if ui.get_show_scheduler() {
        if let Some(q) = st.sched_queue.and_then(|id| queues.iter().find(|q| q.id == id)) {
            ui.set_sched_running(q.running);
            ui.set_sched_files(in_queue(q.id) as i32);
        }
        let days = [
            ui.get_sched_d0(),
            ui.get_sched_d1(),
            ui.get_sched_d2(),
            ui.get_sched_d3(),
            ui.get_sched_d4(),
            ui.get_sched_d5(),
            ui.get_sched_d6(),
        ];
        let next = next_start(ui.get_sched_start_on(), ui.get_sched_start_h(), ui.get_sched_start_m(), days);
        ui.set_sched_next(next.into());
    }

    let selected = items.iter().find(|i| i.id as i32 == ui.get_selected_id());
    ui.set_detail(selected.map(|i| to_detail(i, st)).unwrap_or_default());
    if let Some(it) = selected {
        let qi = it.queue_id.and_then(|q| queues.iter().position(|x| x.id == q)).map_or(0, |i| i + 1);
        ui.set_detail_queue_index(qi as i32);
        ui.set_detail_limit_index(limit_index(it.speed_limit));
    }
    let window = ui.window();
    let width = window.size().width as f32 / window.scale_factor();
    ui.set_compact(selected.is_some() && width < 1500.0);
    ui.set_has_selection(selected.is_some());
    ui.set_can_resume(selected.is_some_and(|i| matches!(i.status, Status::Queued | Status::Paused | Status::Error)));
    ui.set_can_pause(selected.is_some_and(|i| i.is_running()));
    ui.set_has_completed(items.iter().any(|i| i.status == Status::Complete));

    let active = items.iter().filter(|i| i.is_running()).count();
    let total: f64 = st.speeds.values().sum();
    let mut status = format!("{} downloads", items.len());
    if active > 0 {
        status += &format!("   ·   {active} active   ·   {}", util::fmt_speed(total));
    }
    ui.set_status_text(status.into());
}

fn limit_index(bytes_per_sec: u64) -> i32 {
    LIMITS.iter().position(|l| l.0 == bytes_per_sec).unwrap_or(0) as i32
}

fn limit_at(index: i32) -> u64 {
    LIMITS.get(index.max(0) as usize).map_or(0, |l| l.0)
}

/// The queue the toolbar acts on: the one selected in the sidebar, else the main queue.
fn active_queue(ui: &AppWindow) -> u64 {
    ui.get_current_category()
        .strip_prefix("q:")
        .and_then(|id| id.parse().ok())
        .unwrap_or(MAIN_QUEUE)
}

/// Fills the scheduler form with the queue at `index`.
fn load_sched(ui: &AppWindow, engine: &Engine, st: &mut UiState, index: usize) {
    let queues = engine.queues();
    let index = index.min(queues.len().saturating_sub(1));
    let Some(q) = queues.get(index) else { return };
    st.sched_queue = Some(q.id);
    ui.set_sched_index(index as i32);
    ui.set_sched_name(q.name.clone().into());
    ui.set_sched_parallel(q.max_parallel as i32);
    ui.set_sched_start_on(q.start_enabled);
    ui.set_sched_start_h((q.start_minute / 60) as i32);
    ui.set_sched_start_m((q.start_minute % 60) as i32);
    let d = q.days;
    ui.set_sched_d0(d[0]);
    ui.set_sched_d1(d[1]);
    ui.set_sched_d2(d[2]);
    ui.set_sched_d3(d[3]);
    ui.set_sched_d4(d[4]);
    ui.set_sched_d5(d[5]);
    ui.set_sched_d6(d[6]);
    ui.set_sched_stop_on(q.stop_enabled);
    ui.set_sched_stop_h((q.stop_minute / 60) as i32);
    ui.set_sched_stop_m((q.stop_minute % 60) as i32);
    ui.set_sched_retries(q.retries as i32);
    ui.set_sched_done(match q.on_done {
        OnDone::Nothing => 0,
        OnDone::ExitApp => 1,
        OnDone::Shutdown => 2,
    });
    ui.set_sched_is_main(q.id == MAIN_QUEUE);
    ui.set_sched_running(q.running);
}

/// Writes the scheduler form back to its queue.
fn save_sched(ui: &AppWindow, engine: &Engine, st: &UiState) {
    let Some(mut q) = st.sched_queue.and_then(|id| engine.queues().into_iter().find(|q| q.id == id)) else {
        return;
    };
    q.name = ui.get_sched_name().to_string();
    q.max_parallel = ui.get_sched_parallel().max(1) as u32;
    q.start_enabled = ui.get_sched_start_on();
    q.start_minute = (ui.get_sched_start_h() * 60 + ui.get_sched_start_m()) as u32;
    q.days = [
        ui.get_sched_d0(),
        ui.get_sched_d1(),
        ui.get_sched_d2(),
        ui.get_sched_d3(),
        ui.get_sched_d4(),
        ui.get_sched_d5(),
        ui.get_sched_d6(),
    ];
    q.stop_enabled = ui.get_sched_stop_on();
    q.stop_minute = (ui.get_sched_stop_h() * 60 + ui.get_sched_stop_m()) as u32;
    q.retries = ui.get_sched_retries().max(0) as u32;
    q.on_done = match ui.get_sched_done() {
        1 => OnDone::ExitApp,
        2 => OnDone::Shutdown,
        _ => OnDone::Nothing,
    };
    engine.update_queue(&q);
}

/// "today 23:00", "tomorrow 23:00" or "Friday 23:00"; empty when nothing is scheduled.
fn next_start(on: bool, hour: i32, minute: i32, days: [bool; 7]) -> String {
    if !on || !days.contains(&true) {
        return String::new();
    }
    let now = chrono::Local::now();
    let target = (hour * 60 + minute) as u32;
    let current = now.hour() * 60 + now.minute();
    for offset in 0..8u64 {
        let Some(day) = now.date_naive().checked_add_days(chrono::Days::new(offset)) else { break };
        if days[day.weekday().num_days_from_monday() as usize] && (offset > 0 || target > current) {
            let when = match offset {
                0 => "today".to_string(),
                1 => "tomorrow".to_string(),
                _ => day.format("%A").to_string(),
            };
            return format!("{when} {hour:02}:{minute:02}");
        }
    }
    String::new()
}
