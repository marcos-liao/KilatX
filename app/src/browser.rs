//! Browser integration. The KilatX extension (Chrome, Edge, Brave and other
//! Chromium browsers) talks to KilatX through native messaging: the browser starts
//! `KilatX.exe chrome-extension://<id>/` with a pipe on stdin/stdout. In that mode
//! KilatX shows no window and never takes the single instance; it answers, hands
//! downloads to the running KilatX (starting it in the tray if needed) and exits.
//!
//! The host is registered per user under HKCU (no admin), pointing at the exe that
//! is running now, so a portable copy registers itself and fixes the path when moved.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::DownloadOptions;
use crate::engine::LinkInfo;
use crate::store::Settings;

pub const HOST_NAME: &str = "app.kilatx.host";
/// Fixed by the public key in extension/manifest.json.
pub const EXTENSION_ID: &str = "mbnpaegbhgmmkfddjehcbmbdofgdlkok";
/// Every KilatX extension that may talk to KilatX: the one installed from KilatX's own folder
/// (EXTENSION_ID), then the Chrome Web Store and Edge Add-ons versions once they are published
/// (the stores give their copies their own IDs).
pub const EXTENSION_IDS: &[&str] = &[EXTENSION_ID];
/// Messages from the browser larger than this are refused (cookies are a few KB).
const MAX_MESSAGE: usize = 1024 * 1024;
/// How long the check of a link may take before the browser keeps the download.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// A download handed over by the browser; also the payload given to the running KilatX.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserDownload {
    pub url: String,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub size: Option<u64>,
}

impl BrowserDownload {
    /// Sent with every request of this download, so the site sees the same session.
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut h = Vec::new();
        let mut push = |k: &str, v: &str| {
            // A header value can't hold line breaks; drop anything malformed.
            if !v.is_empty() && !v.contains(['\r', '\n']) {
                h.push((k.to_string(), v.to_string()));
            }
        };
        push("Cookie", &self.cookies);
        push("Referer", &self.referrer);
        push("User-Agent", &self.user_agent);
        h
    }
}

/// Switch that carries a JSON list of `BrowserDownload` to the running KilatX.
pub const BROWSER_SWITCH: &str = "/browser";

pub fn is_host_launch(args: &[String]) -> bool {
    args.first().is_some_and(|a| a.starts_with("chrome-extension://") || a == "--native-host")
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Request {
    #[serde(rename = "type")]
    kind: String,
    url: String,
    final_url: String,
    referrer: String,
    filename: String,
    size: Option<u64>,
    cookies: String,
    final_cookies: String,
    user_agent: String,
}

/// What the host needs from the outside world (replaced in tests).
pub trait HostEnv {
    fn settings(&self) -> Settings;
    fn check(&self, options: &DownloadOptions, url: &str, headers: Vec<(String, String)>) -> Result<LinkInfo, String>;
    fn deliver(&self, args: &[String]) -> Result<(), String>;
    fn running(&self) -> bool;
}

/// The real environment: settings from the data folder, the real engine and KilatX.
pub struct RealEnv {
    pub data_dir: PathBuf,
}

impl HostEnv for RealEnv {
    fn settings(&self) -> Settings {
        crate::store::load(&self.data_dir).settings
    }

    fn check(&self, options: &DownloadOptions, url: &str, headers: Vec<(String, String)>) -> Result<LinkInfo, String> {
        let engine = crate::engine::Engine::new(Vec::new(), Vec::new(), 0);
        engine.configure(options.clone())?;
        engine.check_link(url, headers, CHECK_TIMEOUT)
    }

    fn deliver(&self, args: &[String]) -> Result<(), String> {
        crate::desktop::deliver(args)
    }

    fn running(&self) -> bool {
        crate::desktop::is_running()
    }
}

/// Native-messaging entry point (the browser started KilatX as its host).
pub fn run_host_process(data_dir: PathBuf) {
    // Never linger: the browser closes the pipe after the answer; this is the safety net.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
    });
    let env = RealEnv { data_dir };
    serve(&env, &mut std::io::stdin().lock(), &mut std::io::stdout().lock());
}

/// Answers messages until the browser closes the pipe (or sends something broken).
pub fn serve(env: &dyn HostEnv, input: &mut dyn Read, output: &mut dyn Write) {
    loop {
        let mut len = [0u8; 4];
        if input.read_exact(&mut len).is_err() {
            return; // pipe closed
        }
        let len = u32::from_le_bytes(len) as usize;
        if len > MAX_MESSAGE {
            let _ = write_message(output, &json!({ "ok": false, "error": "message too large" }));
            return; // can't trust the rest of the stream
        }
        let mut body = vec![0u8; len];
        if input.read_exact(&mut body).is_err() {
            return;
        }
        let reply = match serde_json::from_slice::<Request>(&body) {
            Ok(req) => handle(env, req),
            Err(e) => json!({ "ok": false, "error": format!("bad message: {e}") }),
        };
        if write_message(output, &reply).is_err() {
            return;
        }
    }
}

fn write_message(output: &mut dyn Write, value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)?;
    output.write_all(&(body.len() as u32).to_le_bytes())?;
    output.write_all(&body)?;
    output.flush()
}

fn leave(reason: impl Into<String>) -> Value {
    json!({ "ok": true, "action": "leave", "reason": reason.into() })
}

fn handle(env: &dyn HostEnv, req: Request) -> Value {
    match req.kind.as_str() {
        "hello" => {
            let s = env.settings();
            json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "running": env.running(), "enabled": s.browser_capture })
        }
        "show" => match env.deliver(&[]) {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": e }),
        },
        // "Download with KilatX" from the context menu: the user asked, so no filters.
        "add" => {
            if !is_http(&req.url) {
                return json!({ "ok": false, "error": "not an http(s) link" });
            }
            let dl = BrowserDownload {
                url: req.url,
                referrer: req.referrer,
                filename: req.filename,
                cookies: req.cookies,
                user_agent: req.user_agent,
                size: req.size,
            };
            match send(env, &dl) {
                Ok(()) => json!({ "ok": true, "action": "take" }),
                Err(e) => json!({ "ok": false, "error": e }),
            }
        }
        "offer" => offer(env, req),
        other => json!({ "ok": false, "error": format!("unknown message type \"{other}\"") }),
    }
}

fn is_http(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// A download the browser just started: take it only when KilatX can really get it.
fn offer(env: &dyn HostEnv, req: Request) -> Value {
    let settings = env.settings();
    if !settings.browser_capture {
        return leave("browser integration is off in KilatX");
    }
    if !is_http(&req.url) {
        return leave("not an http(s) link");
    }
    let opts = &settings.download;
    let final_url = if is_http(&req.final_url) { req.final_url.clone() } else { req.url.clone() };
    let name = Some(crate::util::sanitize(&req.filename))
        .filter(|n| !n.is_empty())
        .or_else(|| crate::util::name_from_url(&final_url))
        .or_else(|| crate::util::name_from_url(&req.url))
        .unwrap_or_default();
    if opts.is_excepted(&req.url) || opts.is_excepted(&final_url) {
        return leave("site is in KilatX's exceptions");
    }
    if !opts.wants_file(&name, &final_url) {
        return leave(format!("\"{name}\" is not a file type KilatX takes (Settings › File types)"));
    }
    let mut dl = BrowserDownload {
        url: req.url.clone(),
        referrer: req.referrer.clone(),
        filename: name,
        cookies: req.cookies.clone(),
        user_agent: req.user_agent.clone(),
        size: req.size,
    };
    // Prefer the original link (a redirect target may be a short-lived signed link),
    // fall back to where the browser actually ended up.
    let mut result = env.check(opts, &dl.url, dl.headers());
    if result.is_err() && final_url != req.url {
        let alt = BrowserDownload { url: final_url, cookies: req.final_cookies.clone(), ..dl.clone() };
        if let Ok(info) = env.check(opts, &alt.url, alt.headers()) {
            dl = alt;
            result = Ok(info);
        }
    }
    let info = match result {
        Ok(info) => info,
        Err(reason) => return leave(reason),
    };
    // Fill in what the browser didn't know yet.
    if dl.filename.is_empty() {
        dl.filename = info.name.unwrap_or_default();
    }
    if dl.size.is_none() {
        dl.size = info.size;
    }
    match send(env, &dl) {
        Ok(()) => json!({ "ok": true, "action": "take" }),
        Err(e) => leave(e),
    }
}

fn send(env: &dyn HostEnv, dl: &BrowserDownload) -> Result<(), String> {
    let payload = serde_json::to_string(&[dl]).map_err(|e| e.to_string())?;
    env.deliver(&[BROWSER_SWITCH.to_string(), payload])
}

/// Downloads carried by a `/browser` switch in KilatX's arguments.
pub fn from_args(args: &[String]) -> Vec<BrowserDownload> {
    let mut out = Vec::new();
    for w in args.windows(2) {
        if w[0].eq_ignore_ascii_case(BROWSER_SWITCH) {
            if let Ok(list) = serde_json::from_str::<Vec<BrowserDownload>>(&w[1]) {
                out.extend(list.into_iter().filter(|d| is_http(&d.url)));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Registration

pub struct Browser {
    pub name: &'static str,
    /// Where the browser looks for native messaging hosts (under HKCU).
    key: &'static str,
    /// Executable, relative to Program Files / Program Files (x86) / Local AppData.
    exe: &'static str,
    /// Page to open for "Load unpacked".
    extensions_page: &'static str,
}

pub const BROWSERS: &[Browser] = &[
    Browser { name: "Google Chrome", key: r"Software\Google\Chrome\NativeMessagingHosts", exe: r"Google\Chrome\Application\chrome.exe", extensions_page: "chrome://extensions/" },
    Browser { name: "Microsoft Edge", key: r"Software\Microsoft\Edge\NativeMessagingHosts", exe: r"Microsoft\Edge\Application\msedge.exe", extensions_page: "edge://extensions/" },
    Browser { name: "Brave", key: r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts", exe: r"BraveSoftware\Brave-Browser\Application\brave.exe", extensions_page: "brave://extensions/" },
    Browser { name: "Vivaldi", key: r"Software\Vivaldi\NativeMessagingHosts", exe: r"Vivaldi\Application\vivaldi.exe", extensions_page: "vivaldi://extensions/" },
    Browser { name: "Chromium", key: r"Software\Chromium\NativeMessagingHosts", exe: r"Chromium\Application\chrome.exe", extensions_page: "chrome://extensions/" },
];

impl Browser {
    pub fn exe_path(&self) -> Option<PathBuf> {
        let roots = [std::env::var_os("ProgramFiles"), std::env::var_os("ProgramFiles(x86)"), std::env::var_os("LOCALAPPDATA")];
        roots.into_iter().flatten().map(|r| PathBuf::from(r).join(self.exe)).find(|p| p.is_file())
    }

    /// Opens the browser's extensions page (for "Load unpacked").
    pub fn open_extensions_page(&self) -> Result<(), String> {
        let exe = self.exe_path().ok_or_else(|| format!("{} is not installed", self.name))?;
        std::process::Command::new(exe).arg(self.extensions_page).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

/// The extension's files, built into KilatX.exe so the portable exe is all you need.
const EXTENSION_FILES: &[(&str, &[u8])] = &[
    ("manifest.json", include_bytes!("../extension/manifest.json")),
    ("background.js", include_bytes!("../extension/background.js")),
    ("popup.html", include_bytes!("../extension/popup.html")),
    ("popup.js", include_bytes!("../extension/popup.js")),
    ("icons/icon16.png", include_bytes!("../extension/icons/icon16.png")),
    ("icons/icon32.png", include_bytes!("../extension/icons/icon32.png")),
    ("icons/icon48.png", include_bytes!("../extension/icons/icon48.png")),
    ("icons/icon128.png", include_bytes!("../extension/icons/icon128.png")),
    ("icons/off16.png", include_bytes!("../extension/icons/off16.png")),
    ("icons/off32.png", include_bytes!("../extension/icons/off32.png")),
];

pub fn extension_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("browser").join("extension")
}

fn manifest_path(data_dir: &Path, host_name: &str) -> PathBuf {
    data_dir.join("browser").join(format!("{host_name}.json"))
}

/// Writes the extension folder (only files that changed, so the browser doesn't
/// see a modified extension for nothing).
pub fn write_extension(data_dir: &Path) -> Result<PathBuf, String> {
    let dir = extension_dir(data_dir);
    for (name, bytes) in EXTENSION_FILES {
        let path = dir.join(name);
        if std::fs::read(&path).is_ok_and(|old| old == *bytes) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    }
    Ok(dir)
}

/// The native messaging host manifest: which program the browsers start, and which
/// extensions may talk to it (the self-installed one and the store versions).
fn host_manifest(exe: &Path, host_name: &str) -> String {
    let origins: Vec<String> = EXTENSION_IDS.iter().map(|id| format!("chrome-extension://{id}/")).collect();
    let manifest = json!({
        "name": host_name,
        "description": "KilatX extension helper",
        "path": exe,
        "type": "stdio",
        "allowed_origins": origins,
    });
    serde_json::to_string_pretty(&manifest).unwrap_or_default()
}

fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::write(path, text).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Registers this exe as the browsers' native messaging host (per user, no admin).
pub fn register(data_dir: &Path, host_name: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = manifest_path(data_dir, host_name);
    write_if_changed(&path, &host_manifest(&exe, host_name))?;
    write_extension(data_dir)?;
    for b in BROWSERS {
        let key = windows_registry::CURRENT_USER.create(format!(r"{}\{host_name}", b.key)).map_err(|e| e.to_string())?;
        key.set_string("", path.display().to_string()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Removes the registration (registry keys and the host manifest).
pub fn unregister(data_dir: &Path, host_name: &str) {
    for b in BROWSERS {
        let _ = windows_registry::CURRENT_USER.remove_tree(format!(r"{}\{host_name}", b.key));
    }
    let _ = std::fs::remove_file(manifest_path(data_dir, host_name));
}

/// Removes only the keys that point at this data folder's manifest (uninstall: a
/// portable copy registered elsewhere keeps its connection).
pub fn unregister_if_ours(data_dir: &Path, host_name: &str) {
    let path = manifest_path(data_dir, host_name);
    for b in BROWSERS {
        let key = format!(r"{}\{host_name}", b.key);
        let ours = windows_registry::CURRENT_USER
            .open(&key)
            .and_then(|k| k.get_string(""))
            .is_ok_and(|v| PathBuf::from(v) == path);
        if ours {
            let _ = windows_registry::CURRENT_USER.remove_tree(&key);
        }
    }
    let _ = std::fs::remove_file(path);
}

/// True when every browser key points at this data folder's manifest, and the
/// manifest points at this exe.
pub fn is_registered(data_dir: &Path, host_name: &str) -> bool {
    let path = manifest_path(data_dir, host_name);
    let exe = std::env::current_exe().ok();
    let manifest_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("path").and_then(Value::as_str).map(PathBuf::from))
        .is_some_and(|p| Some(p) == exe);
    manifest_ok
        && BROWSERS.iter().all(|b| {
            windows_registry::CURRENT_USER
                .open(format!(r"{}\{host_name}", b.key))
                .and_then(|k| k.get_string(""))
                .is_ok_and(|v| PathBuf::from(v) == path)
        })
}

/// Machine-wide registration, for all users of the PC (setup, as administrator). Companies
/// often allow only machine-wide hosts (browser policy NativeMessagingUserLevelHosts = off).
/// The manifest goes next to the installed exe; both registry views are written, as browsers
/// look in both.
fn machine_keys(host_name: &str) -> Vec<String> {
    BROWSERS
        .iter()
        .flat_map(|b| [format!(r"{}\{host_name}", b.key), format!(r"{}\{host_name}", b.key.replacen(r"Software\", r"Software\WOW6432Node\", 1))])
        .collect()
}

pub fn machine_manifest_path(exe: &Path, host_name: &str) -> PathBuf {
    exe.with_file_name(format!("{host_name}.json"))
}

pub fn register_machine(exe: &Path, host_name: &str) -> Result<(), String> {
    let path = machine_manifest_path(exe, host_name);
    write_if_changed(&path, &host_manifest(exe, host_name))?;
    for key in machine_keys(host_name) {
        windows_registry::LOCAL_MACHINE
            .create(&key)
            .and_then(|k| k.set_string("", path.display().to_string()))
            .map_err(|e| format!("Cannot register for all users ({key}): {e}"))?;
    }
    Ok(())
}

/// Removes the machine-wide registration, where it points at this exe's manifest.
pub fn unregister_machine(exe: &Path, host_name: &str) {
    let path = machine_manifest_path(exe, host_name);
    for key in machine_keys(host_name) {
        let ours = windows_registry::LOCAL_MACHINE.open(&key).and_then(|k| k.get_string("")).is_ok_and(|v| PathBuf::from(v) == path);
        if ours {
            let _ = windows_registry::LOCAL_MACHINE.remove_tree(&key);
        }
    }
    let _ = std::fs::remove_file(path);
}

/// True when all browsers reach this exe through the machine-wide registration.
pub fn is_registered_for_machine(host_name: &str) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let path = machine_manifest_path(&exe, host_name);
    let manifest_ok = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("path").and_then(Value::as_str).map(PathBuf::from))
        .is_some_and(|p| p == exe);
    manifest_ok
        && BROWSERS.iter().all(|b| {
            windows_registry::LOCAL_MACHINE
                .open(format!(r"{}\{host_name}", b.key))
                .and_then(|k| k.get_string(""))
                .is_ok_and(|v| PathBuf::from(v) == path)
        })
}

/// True if another program connected to the browsers describes itself as a download manager
/// (its own description mentions downloads): its browser integration may take a download
/// inside the browser before KilatX's extension sees it. Only yes or no; no names are kept.
pub fn other_download_manager_present() -> bool {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE};
    BROWSERS.iter().any(|b| {
        let wow = b.key.replacen(r"Software\", r"Software\WOW6432Node\", 1);
        download_manager_host_in(CURRENT_USER, b.key) || download_manager_host_in(LOCAL_MACHINE, b.key) || download_manager_host_in(LOCAL_MACHINE, &wow)
    })
}

/// True if a host registered under `root\path` (a browser's NativeMessagingHosts key), other
/// than KilatX's own, describes itself as a download manager.
pub fn download_manager_host_in(root: &windows_registry::Key, path: &str) -> bool {
    let Ok(key) = root.open(path) else { return false };
    let Ok(names) = key.keys() else { return false };
    names.filter(|name| !name.to_ascii_lowercase().starts_with("app.kilatx")).any(|name| {
        key.open(&name)
            .and_then(|k| k.get_string(""))
            .ok()
            .and_then(|m| host_description(Path::new(&m)))
            .is_some_and(|d| looks_like_download_manager(&d))
    })
}

/// The "description" in a native messaging host manifest.
fn host_description(manifest: &Path) -> Option<String> {
    let bytes = std::fs::read(manifest).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("description")?.as_str().map(str::to_string)
}

pub fn looks_like_download_manager(description: &str) -> bool {
    description.to_lowercase().contains("download")
}
