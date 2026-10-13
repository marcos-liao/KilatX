//! Switches KilatX Setup (Inno Setup, `setup/KilatX.iss`) runs while installing,
//! updating and uninstalling. Each does its job and exits; no window opens.
//!
//! `KilatX.exe /close`                       asks the running KilatX of this Windows user to
//!                                           save and exit, waits until it is gone (exit 0) or
//!                                           gives up after 20 seconds (exit 1)
//! `KilatX.exe /register-user`               after install: notification name, browser
//!                                           connection, start with Windows (as set in KilatX)
//! `KilatX.exe /cleanup-user [/removedata]`  on uninstall: removes what KilatX registered for
//!                                           this Windows user, and its data if asked
//! `KilatX.exe /register-machine`            as administrator: browsers of all users reach this
//!                                           KilatX.exe (manifest next to it, HKLM keys)
//! `KilatX.exe /unregister-machine`          removes that again (uninstall)
//! `KilatX.exe /uninstall`                   opens the uninstaller next to KilatX.exe

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::HSTRING;

/// Switches handled here, before KilatX's single-instance check.
pub const SWITCHES: &[&str] = &["/close", "/register-user", "/cleanup-user", "/register-machine", "/unregister-machine", "/uninstall"];

pub fn run(args: &[String]) -> i32 {
    let exe = std::env::current_exe().unwrap_or_default();
    if has(args, "/close") {
        let closed = close_kilatx(&exe, Duration::from_secs(20));
        // `/close <installed KilatX.exe>`: also end what else runs from that file (a browser's
        // connection to KilatX, or a KilatX that didn't close), so setup can replace it.
        let ended = args.get(1).map(PathBuf::from).is_none_or(|target| end_processes_of(&target));
        crate::util::event_log("close", &format!("closed={closed} ended={ended}"));
        return if closed && ended { 0 } else { 1 };
    }
    if has(args, "/register-user") {
        register_user();
        return 0;
    }
    // As administrator, from setup: browsers of every user of this PC reach this KilatX.exe.
    if has(args, "/register-machine") {
        return match crate::browser::register_machine(&exe, crate::browser::HOST_NAME) {
            Ok(()) => 0,
            Err(e) => {
                crate::util::event_log("register-machine", &e);
                1
            }
        };
    }
    if has(args, "/unregister-machine") {
        crate::browser::unregister_machine(&exe, crate::browser::HOST_NAME);
        return 0;
    }
    if has(args, "/cleanup-user") {
        cleanup_user(&exe, has(args, "/removedata"));
        return 0;
    }
    // "/uninstall": from an old shortcut or a script.
    let uninstaller = exe.parent().map(|d| d.join("unins000.exe")).filter(|p| p.is_file());
    match uninstaller {
        Some(u) => {
            let _ = std::process::Command::new(u).spawn();
            0
        }
        None => {
            message("This copy of KilatX is not installed (portable copy): there is nothing to uninstall. To remove what it registered in Windows, use Settings › Browser › Remove traces and exit.");
            2
        }
    }
}

fn register_user() {
    let data_dir = crate::store::data_dir();
    let _ = std::fs::create_dir_all(&data_dir);
    let settings = crate::store::load(&data_dir).settings;
    crate::desktop::register_app(&crate::notification_icon(&data_dir));
    if settings.browser_capture {
        if let Err(e) = crate::browser::register(&data_dir, crate::browser::HOST_NAME) {
            crate::util::event_log("register-user", &format!("browser: {e}"));
        }
    }
    if settings.start_with_windows {
        let _ = crate::desktop::set_autostart(true);
    }
    crate::util::event_log("register-user", "");
}

fn cleanup_user(exe: &Path, remove_data: bool) {
    let data_dir = crate::store::data_dir();
    crate::desktop::remove_autostart_if(exe);
    crate::browser::unregister_if_ours(&data_dir, crate::browser::HOST_NAME);
    crate::desktop::remove_app_registration();
    if remove_data && is_installed_data_dir(&data_dir) {
        let _ = std::fs::remove_dir_all(&data_dir);
    }
    crate::util::event_log("cleanup-user", if remove_data { "removedata" } else { "" });
}

/// The installed copy's data folder (%APPDATA%\KilatX, or the test override): never a
/// portable `data` folder or anything that isn't clearly KilatX's.
fn is_installed_data_dir(dir: &Path) -> bool {
    if crate::store::is_portable() {
        return false;
    }
    let test_override = cfg!(debug_assertions) && std::env::var_os("KILATX_DATA_DIR").is_some();
    let named = test_override || dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("KilatX"));
    named && (dir.join("state.json").exists() || dir.read_dir().is_ok_and(|mut d| d.next().is_none()))
}

/// Mutex name of the running KilatX (see desktop::instance_key).
fn instance_mutex() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let extra = if cfg!(debug_assertions) { std::env::var("KILATX_INSTANCE").unwrap_or_default() } else { String::new() };
    format!(r"Local\KilatX-{user}{extra}")
}

pub fn kilatx_running() -> bool {
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(instance_mutex())) } {
        Ok(h) => {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

/// Asks the running KilatX to save and exit (`<exe> /quit` hands the request over),
/// then waits until it is gone. False if it is still running after `timeout`.
pub fn close_kilatx(exe: &Path, timeout: Duration) -> bool {
    if !kilatx_running() {
        return true;
    }
    if exe.is_file() {
        let _ = std::process::Command::new(exe).arg("/quit").spawn().and_then(|mut c| c.wait());
    }
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !kilatx_running() {
            // The mutex goes when the process ends; give Windows a moment to release the exe file.
            std::thread::sleep(Duration::from_millis(300));
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Ends every process (of this Windows user) running from `exe`, except this one, and
/// waits for them. False if one is still there after a few seconds.
pub fn end_processes_of(exe: &Path) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    let want = normalize(exe);
    let me = std::process::id();
    let mut all_gone = true;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return true };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more {
            let pid = entry.th32ProcessID;
            if pid != me && pid != 0 {
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) {
                    let mut buf = [0u16; 1024];
                    let mut len = buf.len() as u32;
                    let path = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len)
                        .ok()
                        .map(|_| String::from_utf16_lossy(&buf[..len as usize]));
                    if path.is_some_and(|p| normalize(Path::new(&p)) == want) {
                        let _ = TerminateProcess(h, 1);
                        if WaitForSingleObject(h, 5000) != WAIT_OBJECT_0 {
                            all_gone = false;
                        }
                    }
                    let _ = CloseHandle(h);
                }
            }
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    all_gone
}

fn normalize(p: &Path) -> String {
    p.to_string_lossy().trim_start_matches(r"\\?\").trim_end_matches('\\').to_lowercase()
}

/// True when this process runs with administrator rights (elevated by UAC).
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Lets Windows load DLLs only from System32, never from the folder KilatX runs in
/// (where another program could have put a fake one). Static imports are covered by
/// the /DEPENDENTLOADFLAG linker option (build.rs).
pub fn harden_dll_search() {
    use windows::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

pub fn has(args: &[String], switch: &str) -> bool {
    args.iter().any(|a| a.eq_ignore_ascii_case(switch))
}

pub fn is_setup_switch(args: &[String]) -> bool {
    args.first().is_some_and(|a| SWITCHES.iter().any(|s| a.eq_ignore_ascii_case(s)))
}

fn message(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    crate::util::event_log("uninstall", text);
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("KilatX"), MB_OK | MB_ICONINFORMATION);
    }
}
