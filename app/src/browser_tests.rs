//! Browser integration scenarios (49–68): the native messaging host's protocol,
//! its decisions against the scenario server, the hand-over to KilatX, downloads
//! with the browser's cookies, and the registration in the registry.
//!
//! The protocol tests run without a server. The rest need `scenario_server.py`
//! (see scenario_tests.rs) and run with `cargo test browser -- --ignored`.

use std::cell::RefCell;
use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::{self, BrowserDownload, HostEnv};
use crate::config::DownloadOptions;
use crate::engine::{AddRequest, Engine, LinkInfo, Status};
use crate::scenario_tests::{base, finished, out, verify};
use crate::store::Settings;

/// Host environment with the real engine check but a recorded hand-over.
struct TestEnv {
    settings: Settings,
    delivered: RefCell<Vec<Vec<String>>>,
    deliver_fails: bool,
    checks: RefCell<Vec<String>>,
}

impl TestEnv {
    fn new() -> Self {
        Self { settings: Settings::default(), delivered: RefCell::new(Vec::new()), deliver_fails: false, checks: RefCell::new(Vec::new()) }
    }

    fn payload(&self) -> Vec<BrowserDownload> {
        browser::from_args(self.delivered.borrow().last().expect("nothing delivered"))
    }
}

impl HostEnv for TestEnv {
    fn settings(&self) -> Settings {
        self.settings.clone()
    }

    fn check(&self, options: &DownloadOptions, url: &str, headers: Vec<(String, String)>) -> Result<LinkInfo, String> {
        self.checks.borrow_mut().push(url.to_string());
        let engine = Engine::new(Vec::new(), Vec::new(), 0);
        engine.configure(options.clone())?;
        engine.check_link(url, headers, Duration::from_secs(10))
    }

    fn deliver(&self, args: &[String]) -> Result<(), String> {
        if self.deliver_fails {
            return Err("KilatX did not start".into());
        }
        self.delivered.borrow_mut().push(args.to_vec());
        Ok(())
    }

    fn running(&self) -> bool {
        true
    }
}

fn frame(v: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(v).unwrap();
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend(body);
    out
}

fn replies(mut bytes: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    while bytes.len() >= 4 {
        let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        out.push(serde_json::from_slice(&bytes[4..4 + len]).unwrap());
        bytes = &bytes[4 + len..];
    }
    out
}

/// Sends `messages` (already framed) through the host, returns its answers.
fn talk(env: &TestEnv, input: Vec<u8>) -> Vec<Value> {
    let mut output = Vec::new();
    browser::serve(env, &mut input.as_slice(), &mut output);
    replies(&output)
}

fn ask(env: &TestEnv, msg: Value) -> Value {
    talk(env, frame(&msg)).pop().expect("no answer")
}

fn offer(url: &str) -> Value {
    json!({ "type": "offer", "url": url, "final_url": url, "referrer": "", "filename": "", "cookies": "", "user_agent": "Mozilla/5.0 Test" })
}

// ----- protocol (no server needed) -----

#[test]
fn scenario_49_hello_reports_version_and_state() {
    let mut env = TestEnv::new();
    env.settings.browser_capture = false;
    let r = ask(&env, json!({ "type": "hello" }));
    assert_eq!(r["ok"], true);
    assert_eq!(r["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(r["enabled"], false);
}

#[test]
fn scenario_50_broken_messages_never_crash_or_hang() {
    let env = TestEnv::new();
    // Garbage, an unknown type, then a valid message: each gets its own answer.
    let mut input = Vec::new();
    let junk = b"not json";
    input.extend((junk.len() as u32).to_le_bytes());
    input.extend(junk);
    input.extend(frame(&json!({ "type": "launch-missiles" })));
    input.extend(frame(&json!({ "type": "hello" })));
    let r = talk(&env, input);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0]["ok"], false);
    assert_eq!(r[1]["ok"], false);
    assert_eq!(r[2]["ok"], true);

    // Oversized: refused, then the host stops reading.
    let mut big = (64 * 1024 * 1024u32).to_le_bytes().to_vec();
    big.extend(frame(&json!({ "type": "hello" })));
    let r = talk(&env, big);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["error"], "message too large");

    // Cut off in the middle of a message: no answer, no panic.
    let mut cut = frame(&json!({ "type": "hello" }));
    cut.truncate(cut.len() - 3);
    assert!(talk(&env, cut).is_empty());
    assert!(talk(&env, Vec::new()).is_empty(), "closed pipe ends the host");
}

#[test]
fn scenario_51_header_values_cannot_inject_lines() {
    let dl = BrowserDownload {
        url: "https://example.com/a.zip".into(),
        cookies: "a=1\r\nX-Evil: 1".into(),
        referrer: "https://example.com/page".into(),
        user_agent: String::new(),
        ..Default::default()
    };
    let h = dl.headers();
    assert_eq!(h, vec![("Referer".to_string(), "https://example.com/page".to_string())]);
}

#[test]
fn scenario_52_payload_survives_the_hand_over() {
    let dl = BrowserDownload {
        url: "https://example.com/Laporan Akhir – 2026.pdf".into(),
        cookies: "x".repeat(20_000),
        filename: "Laporan Akhir – 2026.pdf".into(),
        ..Default::default()
    };
    let args = vec!["/tray".to_string(), browser::BROWSER_SWITCH.into(), serde_json::to_string(&[&dl]).unwrap()];
    // The pipe carries the args as one JSON line.
    let line = serde_json::to_string(&args).unwrap();
    let back: Vec<String> = serde_json::from_str(&line).unwrap();
    assert_eq!(browser::from_args(&back), vec![dl]);
    // Anything that isn't http(s) is dropped.
    let bad = vec![browser::BROWSER_SWITCH.to_string(), r#"[{"url":"file:///C:/Windows/win.ini"}]"#.into()];
    assert!(browser::from_args(&bad).is_empty());
}

#[test]
fn scenario_53_host_launch_is_detected() {
    assert!(browser::is_host_launch(&["chrome-extension://mbnpaegbhgmmkfddjehcbmbdofgdlkok/".into(), "--parent-window=0".into()]));
    assert!(!browser::is_host_launch(&["/d".into(), "https://example.com/a.zip".into()]));
    assert!(!browser::is_host_launch(&[]));
}

#[test]
fn scenario_54_offer_respects_switch_types_and_exceptions() {
    let mut env = TestEnv::new();
    env.settings.download.capture_exceptions = vec!["*.example.org".into()];
    // No server needed: these are decided before any request.
    let r = ask(&env, offer("https://example.com/page.html"));
    assert_eq!(r["action"], "leave", "{r}");
    let r = ask(&env, offer("https://files.example.org/setup.exe"));
    assert_eq!(r["action"], "leave", "{r}");
    assert!(r["reason"].as_str().unwrap().contains("exceptions"));
    let r = ask(&env, offer("data:application/zip;base64,AAAA"));
    assert_eq!(r["action"], "leave", "{r}");
    env.settings.browser_capture = false;
    let r = ask(&env, offer("https://example.com/setup.exe"));
    assert_eq!(r["action"], "leave", "{r}");
    assert!(env.checks.borrow().is_empty(), "no request may be made for these");
    assert!(env.delivered.borrow().is_empty());
}

// ----- against the scenario server -----

#[test]
#[ignore]
fn scenario_55_offer_takes_a_normal_file() {
    let env = TestEnv::new();
    let url = format!("{}/plain/300000/browser-take.zip", base());
    let r = ask(&env, json!({ "type": "offer", "url": url, "final_url": url, "referrer": "https://example.com/",
        "filename": "browser-take.zip", "cookies": "session=ok", "user_agent": "Mozilla/5.0 Test", "size": 300000 }));
    assert_eq!(r["action"], "take", "{r}");
    let p = env.payload();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].url, url);
    assert_eq!(p[0].cookies, "session=ok");
    assert_eq!(p[0].referrer, "https://example.com/");
    assert_eq!(p[0].user_agent, "Mozilla/5.0 Test");
    assert_eq!(p[0].filename, "browser-take.zip");
}

#[test]
#[ignore]
fn scenario_56_offer_leaves_what_kilatx_cannot_fetch() {
    let env = TestEnv::new();
    let b = base();
    for (url, why) in [
        (format!("{b}/notfound/1000/gone.zip"), "not found"),
        (format!("{b}/servererror/1000/err.zip"), "Server error"),
        (format!("{b}/htmlpage/1000/x.exe"), "web page"),
        (format!("{b}/postonly/1000/report.pdf"), "405"),
        (format!("{b}/cookie/1000/members.zip"), "Access denied"),
        (format!("{b}/needref/1000/hot.zip?ref=gallery"), "Access denied"),
    ] {
        let r = ask(&env, offer(&url));
        assert_eq!(r["action"], "leave", "{url}: {r}");
        assert!(r["reason"].as_str().unwrap().contains(why), "{url}: {r}");
    }
    assert!(env.delivered.borrow().is_empty(), "nothing may reach KilatX");
}

#[test]
#[ignore]
fn scenario_57_cookies_and_referrer_from_the_browser_open_the_door() {
    let env = TestEnv::new();
    let b = base();
    let url = format!("{b}/cookie/1000/members.zip");
    let mut m = offer(&url);
    m["cookies"] = json!("theme=dark; session=ok");
    assert_eq!(ask(&env, m)["action"], "take");
    let url = format!("{b}/needref/1000/hot.zip?ref=gallery");
    let mut m = offer(&url);
    m["referrer"] = json!("https://site.test/gallery/42");
    assert_eq!(ask(&env, m)["action"], "take");
}

#[test]
#[ignore]
fn scenario_58_redirect_target_used_when_the_original_fails() {
    let env = TestEnv::new();
    let b = base();
    // The original link only worked once (the browser used it); the final one works.
    let original = format!("{b}/notfound/1000/once.zip");
    let final_url = format!("{b}/plain/1000/once.zip");
    let r = ask(&env, json!({ "type": "offer", "url": original, "final_url": final_url, "filename": "once.zip",
        "cookies": "", "final_cookies": "cdn=1", "user_agent": "Mozilla/5.0" }));
    assert_eq!(r["action"], "take", "{r}");
    let p = env.payload();
    assert_eq!(p[0].url, final_url);
    assert_eq!(p[0].cookies, "cdn=1");
    // A working original link is preferred (fresh signed link on resume).
    let env = TestEnv::new();
    let original = format!("{b}/redirect/2/1000/hop.zip");
    let r = ask(&env, json!({ "type": "offer", "url": original, "final_url": format!("{b}/plain/1000/hop.zip"), "filename": "hop.zip" }));
    assert_eq!(r["action"], "take", "{r}");
    assert_eq!(env.payload()[0].url, original);
}

#[test]
#[ignore]
fn scenario_59_kilatx_not_starting_leaves_the_download_to_the_browser() {
    let mut env = TestEnv::new();
    env.deliver_fails = true;
    let r = ask(&env, offer(&format!("{}/plain/1000/nostart.zip", base())));
    assert_eq!(r["action"], "leave", "{r}");
    assert!(r["reason"].as_str().unwrap().contains("did not start"));
}

#[test]
#[ignore]
fn scenario_60_context_menu_sends_without_filters() {
    let env = TestEnv::new();
    // An .html link and a site that would fail the check: the user asked, so it goes to KilatX.
    let url = format!("{}/notfound/1000/page.html", base());
    let r = ask(&env, json!({ "type": "add", "url": url, "referrer": "https://site.test/", "cookies": "a=b" }));
    assert_eq!(r["action"], "take", "{r}");
    assert!(env.checks.borrow().is_empty(), "no check for an explicit request");
    assert_eq!(env.payload()[0].url, url);
}

#[test]
#[ignore]
fn scenario_61_slow_server_is_left_after_the_timeout() {
    // The check gives up after 10 s, so the browser never waits longer than that.
    let env = TestEnv::new();
    let engine = Engine::new(Vec::new(), Vec::new(), 0);
    engine.configure(DownloadOptions::default()).unwrap();
    let t = std::time::Instant::now();
    // 'stall' answers the first non-zero range only; use a black-hole address instead.
    let r = engine.check_link("http://10.255.255.1/file.zip", Vec::new(), Duration::from_secs(2));
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(4), "took {:?}", t.elapsed());
    let _ = env;
}

// ----- the download itself -----

#[test]
#[ignore]
fn scenario_62_download_with_browser_cookies_and_name() {
    let dir = out("s62");
    let engine = Engine::new(Vec::new(), Vec::new(), 0);
    let dl = BrowserDownload {
        url: format!("{}/cookie/2000000/members.zip?r=400000", base()),
        cookies: "session=ok".into(),
        referrer: "https://site.test/".into(),
        user_agent: "Mozilla/5.0 Test".into(),
        filename: "Members Pack.zip".into(),
        size: None,
    };
    let id = engine.add_request(AddRequest {
        url: dl.url.clone(),
        dir: Some(dir.clone()),
        connections: 8,
        start: true,
        headers: dl.headers(),
        file_name: Some(dl.filename.clone()),
        ..Default::default()
    });
    let it = finished(&engine, id, 60);
    verify(&it, 2_000_000);
    assert_eq!(it.file_name, "Members Pack.zip", "browser's name kept");
    assert!(it.segments.len() > 1 || it.connections > 1, "several connections with cookies");
    assert!(it.headers.is_empty(), "cookies dropped once complete");
}

#[test]
#[ignore]
fn scenario_63_cookies_are_saved_encrypted_and_resume_works() {
    let dir = out("s63");
    let engine = Engine::new(Vec::new(), Vec::new(), 0);
    let id = engine.add_request(AddRequest {
        url: format!("{}/cookie/3000000/resume.zip?r=300000", base()),
        dir: Some(dir),
        connections: 4,
        start: true,
        headers: vec![("Cookie".into(), "session=ok; secret=TOPSECRET123".into())],
        ..Default::default()
    });
    std::thread::sleep(Duration::from_millis(1500));
    engine.pause(id);
    let paused = crate::scenario_tests::wait(&engine, id, |i| i.status == Status::Paused, 10);
    let json = serde_json::to_string(&paused).unwrap();
    assert!(!json.contains("TOPSECRET123"), "cookie must not be saved in plain text");
    assert!(json.contains("dpapi:"));
    // As after a restart: load it back and resume with the cookie.
    let back: crate::engine::Item = serde_json::from_str(&json).unwrap();
    assert_eq!(back.headers[0].1, "session=ok; secret=TOPSECRET123");
    let engine2 = Engine::new(vec![back], Vec::new(), 0);
    engine2.start(id);
    let it = finished(&engine2, id, 60);
    verify(&it, 3_000_000);
}

#[test]
#[ignore]
fn scenario_64_server_name_does_not_replace_the_browser_name() {
    let dir = out("s64");
    let engine = Engine::new(Vec::new(), Vec::new(), 0);
    // 'cd' sends a Content-Disposition name; the browser already chose one.
    let id = engine.add_request(AddRequest {
        url: format!("{}/cd/1000/download.php?id=7", base()),
        dir: Some(dir),
        connections: 1,
        start: true,
        file_name: Some("Chosen by browser.pdf".into()),
        ..Default::default()
    });
    let it = finished(&engine, id, 30);
    assert_eq!(it.status, Status::Complete, "{:?}", it.error);
    assert_eq!(it.file_name, "Chosen by browser.pdf");
    // A name with characters Windows forbids is cleaned, not rejected.
    let id = engine.add_request(AddRequest {
        url: format!("{}/plain/1000/x.bin", base()),
        dir: Some(out("s64b")),
        connections: 1,
        start: true,
        file_name: Some("a:b*c?.bin".into()),
        ..Default::default()
    });
    let it = finished(&engine, id, 30);
    assert_eq!(it.file_name, "a_b_c_.bin");
}

// ----- registration (touches HKCU, under a test host name) -----

#[test]
#[ignore]
fn scenario_65_registration_and_remove_traces() {
    const TEST_HOST: &str = "app.kilatx.host.test";
    let data = std::env::temp_dir().join("kilatx-browser-reg");
    let _ = std::fs::remove_dir_all(&data);
    browser::register(&data, TEST_HOST).unwrap();
    assert!(browser::is_registered(&data, TEST_HOST));
    // Manifest points at this exe and only allows the KilatX extension.
    let m: Value = serde_json::from_str(&std::fs::read_to_string(data.join("browser").join(format!("{TEST_HOST}.json"))).unwrap()).unwrap();
    assert_eq!(m["name"], TEST_HOST);
    assert_eq!(m["type"], "stdio");
    assert_eq!(PathBuf::from(m["path"].as_str().unwrap()), std::env::current_exe().unwrap());
    assert_eq!(m["allowed_origins"], json!([format!("chrome-extension://{}/", browser::EXTENSION_ID)]));
    // Every browser key points at it.
    for key in [r"Software\Google\Chrome\NativeMessagingHosts", r"Software\Microsoft\Edge\NativeMessagingHosts", r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts"] {
        let v = windows_registry::CURRENT_USER.open(format!(r"{key}\{TEST_HOST}")).unwrap().get_string("").unwrap();
        assert_eq!(PathBuf::from(v), data.join("browser").join(format!("{TEST_HOST}.json")));
    }
    // The extension's files are there and identical to the built-in ones.
    let ext = browser::extension_dir(&data);
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(ext.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"), "extension version follows KilatX");
    assert!(ext.join("background.js").is_file() && ext.join("icons/icon128.png").is_file());

    // Moved portable copy: a stale path is reported as not registered, re-registering fixes it.
    let mpath = data.join("browser").join(format!("{TEST_HOST}.json"));
    let mut stale = m.clone();
    stale["path"] = json!(r"E:\old\KilatX.exe");
    std::fs::write(&mpath, serde_json::to_string(&stale).unwrap()).unwrap();
    assert!(!browser::is_registered(&data, TEST_HOST));
    browser::register(&data, TEST_HOST).unwrap();
    assert!(browser::is_registered(&data, TEST_HOST));

    browser::unregister(&data, TEST_HOST);
    assert!(!browser::is_registered(&data, TEST_HOST));
    assert!(windows_registry::CURRENT_USER.open(format!(r"Software\Google\Chrome\NativeMessagingHosts\{TEST_HOST}")).is_err());
    assert!(!mpath.exists());
}

use std::path::PathBuf;

#[test]
fn scenario_66_extension_id_matches_the_key() {
    // Chrome derives the ID from the public key in manifest.json.
    use sha2::Digest;
    let manifest: Value = serde_json::from_slice(include_bytes!("../extension/manifest.json")).unwrap();
    let key = manifest["key"].as_str().unwrap();
    let der = base64_decode(key);
    let hash = sha2::Sha256::digest(&der);
    let id: String = hash[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect();
    assert_eq!(id, browser::EXTENSION_ID);
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
}

fn base64_decode(s: &str) -> Vec<u8> {
    let table = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 0,
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let v: Vec<u32> = chunk.iter().map(|c| table(*c) as u32).collect();
        let n = v.iter().enumerate().fold(0u32, |acc, (i, x)| acc | (x << (18 - 6 * i)));
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

// ----- sites that need a login (Settings › Site logins) -----

fn env_with_login(pattern: &str, user: &str, pass: &str) -> TestEnv {
    let mut env = TestEnv::new();
    env.settings.download.logins = vec![crate::config::SiteLogin {
        pattern: pattern.into(),
        username: user.into(),
        password: crate::config::Secret(pass.into()),
    }];
    env
}

#[test]
#[ignore]
fn scenario_67_browser_download_from_a_basic_login_site() {
    // The browser logged in itself (its credentials are invisible to extensions);
    // KilatX can take over because the site is in Site logins.
    let env = env_with_login("127.0.0.1", "user", "pass");
    let url = format!("{}/auth/2000000/members-only.zip?r=1000000", base());
    let r = ask(&env, offer(&url));
    assert_eq!(r["action"], "take", "{r}");
    // And the download itself logs in on every connection.
    let dl = &env.payload()[0];
    let engine = Engine::new(Vec::new(), Vec::new(), 0);
    engine.configure(env.settings.download.clone()).unwrap();
    let id = engine.add_request(AddRequest {
        url: dl.url.clone(),
        dir: Some(out("s67")),
        connections: 8,
        start: true,
        headers: dl.headers(),
        file_name: Some(dl.filename.clone()),
        ..Default::default()
    });
    let it = finished(&engine, id, 60);
    verify(&it, 2_000_000);
}

#[test]
#[ignore]
fn scenario_68_login_site_without_a_saved_login_stays_in_the_browser() {
    let env = TestEnv::new();
    let r = ask(&env, offer(&format!("{}/auth/1000/members-only.zip", base())));
    assert_eq!(r["action"], "leave", "{r}");
    let reason = r["reason"].as_str().unwrap();
    assert!(reason.contains("Login required") && reason.contains("Site logins"), "{reason}");
    assert!(env.delivered.borrow().is_empty());
}

#[test]
#[ignore]
fn scenario_69_browser_download_from_a_digest_login_site() {
    let env = env_with_login("127.0.0.1", "user", "pass");
    let r = ask(&env, offer(&format!("{}/digest/1000/nas-backup.zip", base())));
    assert_eq!(r["action"], "take", "{r}");
}

#[test]
#[ignore]
fn scenario_70_wrong_saved_password_stays_in_the_browser() {
    let env = env_with_login("127.0.0.1", "user", "nope");
    let r = ask(&env, offer(&format!("{}/auth/1000/members-only.zip", base())));
    assert_eq!(r["action"], "leave", "{r}");
    assert!(r["reason"].as_str().unwrap().contains("Login failed"), "{r}");
    // A login saved for another site is never tried here.
    let env = env_with_login("files.example.com", "user", "pass");
    let r = ask(&env, offer(&format!("{}/whoauth/1000/public.zip", base())));
    assert_eq!(r["action"], "take", "{r}");
    assert_eq!(server_get("/__auth?path=/whoauth/1000/public.zip"), "null", "a password was sent where it doesn't belong");
}

/// Plain GET to the scenario server, returns the body.
fn server_get(path: &str) -> String {
    use std::io::{Read, Write};
    let host = base().trim_start_matches("http://").to_string();
    let mut s = std::net::TcpStream::connect(&host).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp.split("\r\n\r\n").nth(1).unwrap_or_default().to_string()
}

#[test]
fn scenario_71_other_download_managers_are_recognised_by_their_own_description() {
    // Nothing product-specific in KilatX: a host counts as a download manager when its own
    // description says so.
    for d in ["Example Download Accelerator helper", "XYZ DOWNLOADER native host", "Downloads helper"] {
        assert!(browser::looks_like_download_manager(d), "{d}");
    }
    for d in ["ShareX", "Password manager helper", "Cloud drive proxy", ""] {
        assert!(!browser::looks_like_download_manager(d), "{d}");
    }
}

#[test]
fn scenario_72_other_download_manager_connected_to_a_browser_is_found() {
    // A test-only registry key stands in for a browser's NativeMessagingHosts key, so programs
    // really installed on this PC don't change the result.
    use windows_registry::CURRENT_USER;
    let dir = std::env::temp_dir().join(format!("kilatx-other-hosts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = format!(r"Software\KilatX-Test-{}\NativeMessagingHosts", std::process::id());
    let add = |name: &str, desc: &str| {
        let manifest = dir.join(format!("{name}.json"));
        std::fs::write(&manifest, json!({"name": name, "description": desc, "path": "helper.exe", "type": "stdio"}).to_string()).unwrap();
        CURRENT_USER.create(format!(r"{key}\{name}")).unwrap().set_string("", manifest.display().to_string()).unwrap();
    };
    // KilatX's own host and an unrelated helper: no warning.
    add(browser::HOST_NAME, "KilatX extension helper");
    add("app.example.notes", "Example Notes sync helper");
    let quiet = browser::download_manager_host_in(CURRENT_USER, &key);
    // Another download manager joins: warning.
    add("app.example.dlaccel", "Example Download Accelerator helper");
    let warned = browser::download_manager_host_in(CURRENT_USER, &key);
    let _ = CURRENT_USER.remove_tree(key.trim_end_matches(r"\NativeMessagingHosts"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!quiet, "KilatX itself or a non-download helper counted as another download manager");
    assert!(warned, "another download manager not found");
    assert!(!browser::download_manager_host_in(CURRENT_USER, &key), "key not cleaned up");
}