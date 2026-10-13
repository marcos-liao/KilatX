use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::DownloadOptions;
use crate::engine::{Item, Queue};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub connections: u32,
    /// Global speed limit in bytes per second, 0 = none.
    #[serde(default)]
    pub global_limit: u64,
    /// Remembered internet line speed (bytes/s) for smart connections.
    #[serde(default)]
    pub line_peak: u64,
    #[serde(default = "yes")]
    pub close_to_tray: bool,
    #[serde(default = "yes")]
    pub watch_clipboard: bool,
    #[serde(default = "yes")]
    pub notify: bool,
    #[serde(default)]
    pub start_with_windows: bool,
    /// GPU (OpenGL) rendering. Off by default: the software renderer never shows a
    /// blank window after a GPU driver reset, sleep or Remote Desktop, and uses less memory.
    #[serde(default)]
    pub gpu_render: bool,
    /// The "still running in the tray" notification was shown once.
    #[serde(default)]
    pub tray_hint_shown: bool,
    /// File types, save-to folders, existing files, user agent, proxy, site logins.
    #[serde(default)]
    pub download: DownloadOptions,
    /// Browser integration: browsers may hand their downloads to KilatX. Turned off by
    /// "Remove traces", so a portable copy doesn't register itself again.
    #[serde(default = "yes")]
    pub browser_capture: bool,
    /// Show the Add dialog for downloads from the browser (otherwise they start at once).
    #[serde(default = "yes")]
    pub browser_dialog: bool,
    /// Look for a new version once a day (KilatX asks before updating).
    #[serde(default = "yes")]
    pub check_updates: bool,
    /// When the last update check succeeded (Unix seconds).
    #[serde(default)]
    pub last_update_check: i64,
    /// "Skip this version": not offered again (a later version is).
    #[serde(default)]
    pub skipped_version: String,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            connections: 8,
            global_limit: 0,
            line_peak: 0,
            close_to_tray: true,
            watch_clipboard: true,
            notify: true,
            start_with_windows: false,
            gpu_render: false,
            tray_hint_shown: false,
            download: DownloadOptions::default(),
            browser_capture: true,
            browser_dialog: true,
            check_updates: true,
            last_update_check: 0,
            skipped_version: String::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub queues: Vec<Queue>,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// Portable mode: if a `portable.txt` file or a `data` folder sits next to the
/// exe, everything is stored in `<exe dir>\data`. Otherwise `%APPDATA%\KilatX`.
pub fn is_portable() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .is_some_and(|d| d.join("portable.txt").exists() || d.join("data").is_dir())
}

pub fn data_dir() -> PathBuf {
    // Debug builds only: tests point an installed copy at a temporary data folder.
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("KILATX_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        if exe_dir.join("portable.txt").exists() || exe_dir.join("data").is_dir() {
            return exe_dir.join("data");
        }
    }
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("KilatX");
    // Before the rename the app was "Kilat Download Manager": move its data over once.
    let old = base.join("KilatDM");
    if !dir.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

pub fn load(dir: &Path) -> State {
    std::fs::read_to_string(dir.join("state.json"))
        .ok()
        // Editors like Notepad may add a byte-order mark, which JSON doesn't allow.
        .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

pub fn save(dir: &Path, settings: &Settings, queues: &[Queue], items: &[Item]) {
    #[derive(Serialize)]
    struct Out<'a> {
        settings: &'a Settings,
        queues: &'a [Queue],
        items: &'a [Item],
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&Out { settings, queues, items }) else {
        return;
    };
    // Write to a temp file first so a crash mid-save never corrupts the list.
    let tmp = dir.join("state.json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("state.json"));
    }
}
