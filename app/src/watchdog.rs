//! Notices when KilatX's window stops answering and writes down what it was doing.
//!
//! The UI thread beats a heartbeat twice a second (the main timer) and names what it is
//! busy with (`busy("…")`). A watcher thread checks the heartbeat; when it is more than
//! STALL old, it asks the UI thread directly (a window message, see
//! desktop::ui_thread_answers). Only when that goes unanswered too does one line go to KilatX's
//! log (logs\kilatx.log) with how long, and the name of the part of KilatX that was running,
//! and another when it answers again. A heartbeat that stops while the thread still answers is
//! Windows running its own loop (a menu open, the window being moved or resized): not a hang,
//! only noted at the Detailed level. Only names of KilatX's own code are written: no links,
//! file names or other data.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STALL: Duration = Duration::from_secs(5);
/// How long the direct question may take before the thread counts as not answering.
const ASK_TIMEOUT: Duration = Duration::from_secs(2);

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
        let mut watch = Watch::default();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = now_ms();
            let age = now.saturating_sub(BEAT_MS.load(Ordering::Relaxed));
            let what = *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(note) = watch.step(now, age, what, || crate::desktop::ui_thread_answers(ASK_TIMEOUT)) {
                match note {
                    Note::Hang(line) => {
                        crate::log::warn("hang", &line);
                        crate::util::test_event("hang", &line);
                    }
                    Note::Paused(line) => {
                        crate::log::debug("hang", &line);
                        crate::util::test_event("paused", &line);
                    }
                }
            }
        }
    });
}

enum Note {
    /// The window really isn't answering (warning).
    Hang(String),
    /// The timer paused but the window answers (Detailed level only).
    Paused(String),
}

/// What the watcher thread remembers between checks.
#[derive(Default)]
struct Watch {
    /// Not answering since (ms) and what was running then.
    stalled_since: Option<(u64, &'static str)>,
    /// The current pause was already noted.
    pause_noted: bool,
}

impl Watch {
    /// One check: `age` is how old the heartbeat is; `ask` asks the UI thread directly
    /// (None when that isn't possible yet; then the heartbeat alone decides).
    fn step(&mut self, now: u64, age: u64, what: &'static str, ask: impl FnOnce() -> Option<bool>) -> Option<Note> {
        let stalled = age > STALL.as_millis() as u64;
        match self.stalled_since {
            None if stalled => {
                if ask() == Some(true) {
                    if self.pause_noted {
                        return None;
                    }
                    self.pause_noted = true;
                    return Some(Note::Paused(format!("timer paused for {} s while the window still answers (menu open, or the window moved or resized)", age / 1000)));
                }
                self.stalled_since = Some((now - age, what));
                Some(Note::Hang(format!("window not answering for {} s, busy with: {what}", age / 1000)))
            }
            None => {
                self.pause_noted = false;
                None
            }
            Some((since, first)) if age < 1000 => {
                self.stalled_since = None;
                self.pause_noted = false;
                Some(Note::Hang(format!("answering again after {} s (was busy with: {first})", (now - since) / 1000)))
            }
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: Option<Note>) -> String {
        match n {
            Some(Note::Hang(s)) => format!("hang: {s}"),
            Some(Note::Paused(s)) => format!("paused: {s}"),
            None => String::new(),
        }
    }

    #[test]
    fn a_paused_timer_with_an_answering_window_is_not_a_hang() {
        let mut w = Watch::default();
        assert_eq!(text(w.step(10_000, 400, "idle", || Some(true))), "");
        // Menu open: the heartbeat is old, the window answers → one note, Detailed level only.
        assert!(text(w.step(20_000, 6_000, "idle", || Some(true))).starts_with("paused: timer paused for 6 s"));
        assert_eq!(text(w.step(20_500, 6_500, "idle", || Some(true))), "");
        // Menu closed, timer runs again; a later pause is noted again.
        assert_eq!(text(w.step(21_000, 300, "idle", || panic!("not asked while the timer runs"))), "");
        assert!(text(w.step(40_000, 5_500, "idle", || Some(true))).starts_with("paused:"));
    }

    #[test]
    fn a_window_that_does_not_answer_is_a_hang_until_it_answers_again() {
        let mut w = Watch::default();
        assert_eq!(text(w.step(20_000, 6_000, "ui:open_file", || Some(false))), "hang: window not answering for 6 s, busy with: ui:open_file");
        assert_eq!(text(w.step(25_000, 11_000, "ui:open_file", || panic!("asked only once"))), "");
        assert_eq!(text(w.step(26_000, 200, "idle", || None)), "hang: answering again after 12 s (was busy with: ui:open_file)");
    }

    #[test]
    fn without_a_window_to_ask_the_heartbeat_decides() {
        let mut w = Watch::default();
        assert!(text(w.step(20_000, 6_000, "starting", || None)).starts_with("hang: window not answering"));
    }

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
