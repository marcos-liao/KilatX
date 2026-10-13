//! KilatX's own log: `<data folder>\logs\kilatx.log`, always on, for finding out what
//! happened when something goes wrong ("send me the log").
//!
//! What goes in: start and exit (version, Windows, installed or portable), downloads starting,
//! finishing and failing (with the reason), update checks, browser connections, and the
//! window not answering (with the part of KilatX that was busy, see watchdog.rs).
//! What never goes in: cookies, passwords, login headers. Addresses are written without their
//! query string (`?…`), which often carries one-time tokens.
//!
//! Level (Error, Warning, Info, Debug) and folder are set in Settings › About; Debug adds
//! what each connection does (requests, splits, retries, take-overs).
//!
//! At 1 MB the file becomes kilatx.1.log (then .2, .3); older ones are deleted.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

const MAX_SIZE: u64 = 1024 * 1024;
const KEEP: usize = 3;

/// Where the log goes (data\logs, or the folder chosen in Settings).
static LOG: OnceLock<Mutex<PathBuf>> = OnceLock::new();
/// 0 error, 1 warning, 2 info (default), 3 debug.
static LEVEL: AtomicU8 = AtomicU8::new(2);

pub const LEVELS: [&str; 4] = ["error", "warning", "info", "debug"];

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// Applies Settings: level name (see LEVELS) and folder ("" = the default in the data folder).
/// A folder that can't be written falls back to the default; returns the folder in use.
pub fn configure(level: &str, folder: &str, data_dir: &Path) -> PathBuf {
    LEVEL.store(LEVELS.iter().position(|l| *l == level).unwrap_or(2) as u8, Ordering::Relaxed);
    let wanted = if folder.trim().is_empty() { dir(data_dir) } else { PathBuf::from(folder.trim()) };
    let usable = std::fs::create_dir_all(&wanted).is_ok() && {
        let probe = wanted.join(".kilatx-write-test");
        let ok = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    };
    let folder_in_use = if usable { wanted.clone() } else { dir(data_dir) };
    let _ = std::fs::create_dir_all(&folder_in_use);
    if let Some(lock) = LOG.get() {
        if let Ok(mut p) = lock.lock() {
            *p = folder_in_use.join("kilatx.log");
        }
    }
    if !usable {
        warn("log", &format!("cannot write to {}; using {}", wanted.display(), folder_in_use.display()));
    }
    folder_in_use
}

/// The folder the log is written to now.
pub fn current_folder() -> Option<PathBuf> {
    LOG.get()?.lock().ok()?.parent().map(Path::to_path_buf)
}

pub fn debug_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= 3
}

/// Debug line, only built when the level is Debug: `debug_log!("download", "#{} split", id)`.
#[macro_export]
macro_rules! debug_log {
    ($area:expr, $($arg:tt)*) => {
        if $crate::log::debug_enabled() {
            $crate::log::debug($area, &format!($($arg)*));
        }
    };
}

/// Starts logging into `<data_dir>\logs`. Also writes panics there.
pub fn init(data_dir: &Path) {
    let dir = dir(data_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = LOG.set(Mutex::new(dir.join("kilatx.log")));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let place = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        error("panic", &format!("{what}{place}"));
        previous(info);
    }));
}

pub fn debug(area: &str, message: &str) {
    write(3, area, message);
}

pub fn info(area: &str, message: &str) {
    write(2, area, message);
}

pub fn warn(area: &str, message: &str) {
    write(1, area, message);
}

pub fn error(area: &str, message: &str) {
    write(0, area, message);
}

fn write(level: u8, area: &str, message: &str) {
    if level > LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let level = ["ERROR", "WARN ", "INFO ", "DEBUG"][level.min(3) as usize];
    let Some(lock) = LOG.get() else { return };
    let Ok(path) = lock.lock() else { return };
    if std::fs::metadata(&*path).is_ok_and(|m| m.len() > MAX_SIZE) {
        rotate(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&*path) {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let message = redact(message).replace(['\r', '\n'], " ");
        let _ = writeln!(f, "{stamp} {level} [{area}] {message}");
    }
}

fn rotate(path: &Path) {
    let numbered = |n: usize| path.with_file_name(format!("kilatx.{n}.log"));
    let _ = std::fs::remove_file(numbered(KEEP));
    for n in (1..KEEP).rev() {
        let _ = std::fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = std::fs::rename(path, numbered(1));
}

/// Addresses lose their query string and anything after `#`; `user:password@` is removed.
pub fn redact(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("ftp://")) {
                return word.to_string();
            }
            let cut = word.find(['?', '#']).unwrap_or(word.len());
            let mut url = word[..cut].to_string();
            if cut < word.len() {
                url.push_str("?(query hidden)");
            }
            // scheme://user:pass@host → scheme://host
            if let Some(scheme_end) = url.find("://") {
                let rest = &url[scheme_end + 3..];
                let host_end = rest.find('/').unwrap_or(rest.len());
                if let Some(at) = rest[..host_end].rfind('@') {
                    url = format!("{}{}", &url[..scheme_end + 3], &rest[at + 1..]);
                }
            }
            url
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_lose_tokens_and_logins() {
        assert_eq!(redact("start https://cdn.example.com/f.zip?token=abc&sig=1 now"), "start https://cdn.example.com/f.zip?(query hidden) now");
        assert_eq!(redact("https://user:secret@files.example.com/a.zip"), "https://files.example.com/a.zip");
        assert_eq!(redact("http://example.com/page#frag"), "http://example.com/page?(query hidden)");
        assert_eq!(redact("no address here"), "no address here");
        assert_eq!(redact("HTTPS://Example.com/x?y"), "HTTPS://Example.com/x?(query hidden)");
    }

    #[test]
    fn log_rotates_and_keeps_three() {
        let dir = std::env::temp_dir().join(format!("kilatx-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kilatx.log");
        for n in 1..=5 {
            std::fs::write(&path, format!("generation {n}")).unwrap();
            rotate(&path);
        }
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        assert_eq!(names, ["kilatx.1.log", "kilatx.2.log", "kilatx.3.log"]);
        assert_eq!(std::fs::read_to_string(dir.join("kilatx.1.log")).unwrap(), "generation 5");
        assert_eq!(std::fs::read_to_string(dir.join("kilatx.3.log")).unwrap(), "generation 3");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
