//! Windows desktop integration: single instance, clipboard watching,
//! start with Windows, and toast notifications.

use std::io::Write;
use std::path::Path;

use windows::core::{HSTRING, PCWSTR};

/// AppUserModelID: groups the taskbar button and names the sender of notifications.
pub const APP_ID: &str = "KilatX.KilatX";
const WINDOW_TITLE: &str = "KilatX";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "KilatX";

/// Exactly one KilatX per Windows user, whichever copy or folder it runs from:
/// two instances would fight over downloads, settings and the clipboard.
/// Debug builds accept KILATX_INSTANCE so tests can run beside the user's own copy.
fn instance_key() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let extra = if cfg!(debug_assertions) { std::env::var("KILATX_INSTANCE").unwrap_or_default() } else { String::new() };
    format!("KilatX-{user}{extra}")
}

fn pipe_name() -> String {
    format!(r"\\.\pipe\{}", instance_key())
}

/// Held for the life of the process. Windows releases it when the process ends,
/// even after a crash, so a dead KilatX never blocks the next start.
pub struct Instance(#[allow(dead_code)] windows::Win32::Foundation::HANDLE);

pub enum Claim {
    First(Instance),
    AlreadyRunning,
}

/// Decides, before any window exists, whether this process may run.
pub fn claim_instance() -> Claim {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    let name = HSTRING::from(format!(r"Local\{}", instance_key()));
    match unsafe { CreateMutexW(None, true, &name) } {
        Ok(handle) if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS => Claim::First(Instance(handle)),
        Ok(handle) => {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            Claim::AlreadyRunning
        }
        // Can't tell: refuse rather than risk two instances.
        Err(_) => Claim::AlreadyRunning,
    }
}

/// Second launch: give the links to the running KilatX. It may still be starting
/// (pipe not open yet), so keep trying for a few seconds. False if it never answers.
pub fn hand_over(args: &[String]) -> bool {
    for _ in 0..30 {
        if forward_to_running(args) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// True while a KilatX owns the instance (checked without taking it).
pub fn is_running() -> bool {
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name = HSTRING::from(format!(r"Local\{}", instance_key()));
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) } {
        Ok(h) => {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

/// Gives `args` to KilatX, starting it (in the tray) first if it isn't running.
/// Used by the browser host, which must return quickly and must not become KilatX itself.
pub fn deliver(args: &[String]) -> Result<(), String> {
    if is_running() {
        return if hand_over(args) { Ok(()) } else { Err("KilatX is running but not responding".into()) };
    }
    launch_detached(&["/tray"])?;
    // Startup takes a moment (settings, window); wait up to 15 s for it to listen.
    for _ in 0..150 {
        if forward_to_running(args) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err("KilatX did not start".into())
}

/// Starts KilatX.exe as an independent process: not tied to the browser that
/// launched the host (outside its job, no shared console or pipes).
fn launch_detached(args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // The browser's pipes are our stdin/stdout. If KilatX inherited them, the browser
    // would never see the pipe close and would wait for the host forever.
    unsafe {
        use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
        use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if let Ok(h) = GetStdHandle(which) {
                if !h.is_invalid() {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
                }
            }
        }
    }
    let spawn = |flags: u32| {
        Command::new(&exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // Breaking away fails when the browser's job doesn't allow it; then start inside it.
    spawn(base | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(base)).map(|_| ()).map_err(|e| e.to_string())
}

/// Tells the user why nothing opened when the running KilatX doesn't respond.
pub fn show_not_responding() {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(
                "KilatX is already running but is not responding.\n\n\
                 Close it from the tray icon (or Task Manager) and start KilatX again.",
            ),
            &HSTRING::from(WINDOW_TITLE),
            MB_OK | MB_ICONWARNING,
        );
    }
}

/// Hands `args` to a KilatX that is already running. Returns false if there is none.
fn forward_to_running(args: &[String]) -> bool {
    let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(pipe_name()) else {
        return false;
    };
    // Let the running instance bring its window to the front.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    let line = serde_json::to_string(args).unwrap_or_else(|_| "[]".into());
    pipe.write_all(line.as_bytes()).and_then(|_| pipe.write_all(b"\n")).is_ok()
}

/// Receives the arguments of later launches (call after `claim_instance` succeeded).
pub fn listen(_instance: &Instance, on_args: impl Fn(Vec<String>) + Send + 'static) -> std::io::Result<()> {
    use tokio::io::AsyncBufReadExt;
    use tokio::net::windows::named_pipe::ServerOptions;

    let name = pipe_name();
    let rt = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build()?;
    let first = rt.block_on(async { ServerOptions::new().first_pipe_instance(true).create(&name) })?;
    std::thread::spawn(move || {
        rt.block_on(async move {
            use tokio::io::AsyncReadExt;
            let pause = std::time::Duration::from_millis(250);
            let mut server = first;
            loop {
                let connected = server.connect().await.is_ok();
                // Open the next pipe before reading, so launches never find no pipe.
                let next = loop {
                    match ServerOptions::new().create(&name) {
                        Ok(s) => break s,
                        // Never spin: wait and try again.
                        Err(_) => tokio::time::sleep(pause).await,
                    }
                };
                let current = std::mem::replace(&mut server, next);
                if !connected {
                    tokio::time::sleep(pause).await;
                    continue;
                }
                // A client that connects and sends nothing (or garbage) must not stall
                // the listener or grow memory: cap time and size.
                let mut line = String::new();
                let mut reader = tokio::io::BufReader::new(current).take(64 * 1024);
                let read = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_line(&mut line)).await;
                if let Ok(Ok(_)) = read {
                    if let Ok(args) = serde_json::from_str::<Vec<String>>(line.trim()) {
                        on_args(args);
                    }
                }
            }
        });
    });
    Ok(())
}

/// Puts the main window in front (allowed because the sender called AllowSetForegroundWindow).
pub fn bring_to_front() {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE};
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), &HSTRING::from(WINDOW_TITLE)) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

thread_local! {
    static ON_OS_REPAINT: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

/// Windows sometimes throws away what the window shows (screen off or sleep, lock, Remote
/// Desktop, display changes) and asks for all of it again. The software renderer only copies
/// what changed to the screen, so the rest would stay blank or show what was behind the
/// window. Calls `on_repaint` (on the UI thread) when such a request comes from Windows, so
/// the whole window can be marked for drawing. False until KilatX's window exists.
pub fn watch_os_repaints(on_repaint: impl Fn() + 'static) -> bool {
    use windows::Win32::UI::Shell::SetWindowSubclass;
    let Some(hwnd) = own_window() else { return false };
    ON_OS_REPAINT.with(|c| *c.borrow_mut() = Some(Box::new(on_repaint)));
    unsafe { SetWindowSubclass(hwnd, Some(repaint_subclass), 1, 0).as_bool() }
}

unsafe extern "system" fn repaint_subclass(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Graphics::Gdi::GetUpdateRect;
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::WM_PAINT;
    // KilatX's own redraws arrive as WM_PAINT without an update region; only Windows sets one.
    if msg == WM_PAINT && GetUpdateRect(hwnd, None, false).as_bool() {
        ON_OS_REPAINT.with(|c| {
            if let Some(f) = c.borrow().as_ref() {
                f();
            }
        });
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// KilatX's main window (this process, title "KilatX").
fn own_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId};
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> windows::core::BOOL {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            let mut buf = [0u16; 64];
            let n = GetWindowTextW(hwnd, &mut buf) as usize;
            if String::from_utf16_lossy(&buf[..n]) == WINDOW_TITLE {
                *(found.0 as *mut Option<HWND>) = Some(hwnd);
                return false.into();
            }
        }
        true.into()
    }
    let mut found: Option<HWND> = None;
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    }
    found
}

/// When Explorer restarts (crash, Task Manager › Restart, some updates) the taskbar is new and
/// every app has to add its tray icon again; Windows announces this with a "TaskbarCreated"
/// broadcast. The tray icon's window is message-only, and those never get broadcasts, so the
/// icon would stay gone while KilatX keeps running. This hidden window (top-level, so it does
/// get the broadcast) passes the message on to the tray icon's window, which adds the icon back.
/// Call on the UI thread after the tray icon exists.
pub fn keep_tray_icon_after_explorer_restart() {
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, CreateWindowExW, RegisterClassW, MSGFLT_ALLOW, WINDOW_EX_STYLE, WNDCLASSW, WS_POPUP,
    };
    let message = taskbar_created_message();
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(taskbar_watcher_proc),
            hInstance: instance.into(),
            lpszClassName: w!("KilatXTaskbarWatcher"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // Never shown; no parent and no WS_CHILD, so it is top-level and gets broadcasts.
        match CreateWindowExW(WINDOW_EX_STYLE(0), w!("KilatXTaskbarWatcher"), PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None) {
            // Lets Explorer's message through even if KilatX was started as administrator.
            Ok(hwnd) => {
                let _ = ChangeWindowMessageFilterEx(hwnd, message, MSGFLT_ALLOW, None);
            }
            Err(e) => crate::log::warn("tray", &format!("cannot watch for taskbar restarts: {e}")),
        }
    }
}

fn taskbar_created_message() -> u32 {
    unsafe { windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) }
}

unsafe extern "system" fn taskbar_watcher_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, FindWindowExW, GetWindowThreadProcessId, SendMessageW, HWND_MESSAGE};
    if msg != 0 && msg == taskbar_created_message() {
        // The tray icon's message-only window (created by the UI library), in this process only.
        let mut after = None;
        let mut passed = 0;
        while let Ok(tray) = FindWindowExW(Some(HWND_MESSAGE), after, windows::core::w!("SlintSystemTrayWindow"), PCWSTR::null()) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(tray, Some(&mut pid));
            if pid == std::process::id() {
                SendMessageW(tray, msg, Some(WPARAM(0)), Some(LPARAM(0)));
                passed += 1;
            }
            after = Some(tray);
        }
        crate::log::info("tray", &format!("taskbar restarted; tray icon added again ({passed})"));
        crate::util::test_event("tray-readded", &passed.to_string());
        return windows::Win32::Foundation::LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Changes whenever anything is copied; cheap to poll.
pub fn clipboard_sequence() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Adds or removes KilatX from the programs started at sign-in (no admin needed).
/// Calling it again after the folder moved fixes the path, which keeps portable copies working.
pub fn set_autostart(on: bool) -> Result<(), String> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY).map_err(|e| e.to_string())?;
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_string(RUN_VALUE, format!("\"{}\" /tray", exe.display())).map_err(|e| e.to_string())
    } else {
        let _ = key.remove_value(RUN_VALUE);
        Ok(())
    }
}

/// Registers the name and icon shown on notifications (per user, no admin).
pub fn register_app(icon_png: &Path) {
    if let Ok(key) = windows_registry::CURRENT_USER.create(format!(r"Software\Classes\AppUserModelId\{APP_ID}")) {
        let _ = key.set_string("DisplayName", WINDOW_TITLE);
        let _ = key.set_string("IconUri", icon_png.display().to_string());
    }
    unsafe {
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
    }
}

/// Removes everything KilatX wrote to the registry (for portable use on someone else's PC).
pub fn remove_traces() {
    let _ = set_autostart(false);
    remove_app_registration();
}

/// The notification name and icon (written again by any KilatX that starts).
pub fn remove_app_registration() {
    let _ = windows_registry::CURRENT_USER.remove_tree(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
}

/// Turns off start with Windows only if it starts this exe (not another copy).
pub fn remove_autostart_if(exe: &Path) {
    let Ok(key) = windows_registry::CURRENT_USER.open(RUN_KEY) else { return };
    let Ok(value) = key.get_string(RUN_VALUE) else { return };
    if value.to_lowercase().contains(&exe.display().to_string().to_lowercase()) {
        let _ = set_autostart(false);
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Shows a Windows notification from a helper thread: the notification service runs in
/// another process, and KilatX's window must never wait for it.
pub fn notify_soon(title: String, body: String) {
    let _ = std::thread::Builder::new().name("notify".into()).spawn(move || {
        let _ = notify(&title, &body);
    });
}

/// Shows a Windows notification.
pub fn notify(title: &str, body: &str) -> windows::core::Result<()> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    );
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real Run key; removes it again at the end.
    #[test]
    #[ignore]
    fn autostart_is_registered_and_removed() {
        set_autostart(true).unwrap();
        let key = windows_registry::CURRENT_USER.open(RUN_KEY).unwrap();
        let value = key.get_string(RUN_VALUE).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert!(value.contains(&exe.display().to_string()) && value.ends_with("/tray"), "{value}");
        set_autostart(false).unwrap();
        assert!(key.get_string(RUN_VALUE).is_err(), "Run entry should be gone");
    }

    #[test]
    #[ignore]
    fn single_instance_is_enforced_and_carries_arguments() {
        // Own instance name so the user's running KilatX is not touched.
        std::env::set_var("KILATX_INSTANCE", "-unit-test");
        let Claim::First(instance) = claim_instance() else { panic!("first claim must succeed") };
        assert!(matches!(claim_instance(), Claim::AlreadyRunning), "second claim must be refused");
        let (tx, rx) = std::sync::mpsc::channel();
        listen(&instance, move |args| tx.send(args).unwrap()).unwrap();

        // A silent client and a garbage client must not block later launches.
        let open = || {
            for _ in 0..50 {
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open(pipe_name()) {
                    return f;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            panic!("pipe never became free");
        };
        let silent = open();
        {
            let mut junk = open();
            let _ = junk.write_all(b"not json\n");
        }
        let sent = vec!["/d".to_string(), "https://example.com/a b.zip".to_string()];
        assert!(hand_over(&sent));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), sent);
        drop(silent);
        for i in 0..20 {
            assert!(hand_over(&[format!("n{i}")]));
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), vec![format!("n{i}")]);
        }
    }
}

/// How many KilatX notifications are in the Action Center (used by tests).
#[allow(dead_code)]
pub fn notifications_in_action_center() -> u32 {
    use windows::UI::Notifications::ToastNotificationManager;
    ToastNotificationManager::History()
        .and_then(|h| h.GetHistoryWithId(&HSTRING::from(APP_ID)))
        .and_then(|list| list.Size())
        .unwrap_or(0)
}
