//! One test per real-world "download model", against `testkit/scenario_server.py`.
//!
//! ```text
//! python testkit/scenario_server.py 8766 8767
//! set KILAT_SCEN=http://127.0.0.1:8766  &  set KILAT_SCEN_PROXY=http://127.0.0.1:8767
//! cargo test scenario -- --ignored --test-threads=1
//! ```
//! Generated files have byte i == i % 251, so every byte's position is checked.

use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::engine::{Engine, Item, Status};

pub(crate) fn base() -> String {
    std::env::var("KILAT_SCEN").expect("set KILAT_SCEN to the scenario server")
}

pub(crate) fn out(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("kilat-scenarios").join(name);
    let _ = std::fs::remove_dir_all(&d);
    d
}

pub(crate) fn item(engine: &Engine, id: u64) -> Item {
    engine.snapshot().into_iter().find(|i| i.id == id).unwrap()
}

pub(crate) fn wait(engine: &Engine, id: u64, until: impl Fn(&Item) -> bool, secs: u64) -> Item {
    let t = Instant::now();
    loop {
        let it = item(engine, id);
        if until(&it) || t.elapsed().as_secs() > secs {
            return it;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

pub(crate) fn finished(engine: &Engine, id: u64, secs: u64) -> Item {
    wait(engine, id, |i| matches!(i.status, Status::Complete | Status::Error), secs)
}

/// Checks size and that every byte sits at the right position.
pub(crate) fn verify(it: &Item, size: u64) {
    assert_eq!(it.status, Status::Complete, "download failed: {:?}", it.error);
    let path = it.final_path();
    let mut f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(f.metadata().unwrap().len(), size, "wrong size");
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0u64;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for (i, b) in buf[..n].iter().enumerate() {
            let at = pos + i as u64;
            assert_eq!(*b as u64, at % 251, "wrong byte at offset {at}");
        }
        pos += n as u64;
    }
    assert_eq!(pos, size);
}

fn download(path: &str, dir: &str, conns: u32) -> (Engine, u64) {
    let engine = Engine::new(vec![], vec![], 0);
    let id = engine.add(&format!("{}{path}", base()), Some(out(dir)), conns, true, None);
    (engine, id)
}

#[test]
#[ignore]
fn scenario_01_plain_multi_connection() {
    // (An unthrottled local server is saturated by one connection, so smart
    // connections rightly stay at one there; see scenario 25.)
    // Each connection limited to 2 MB/s, so more connections really help.
    let (e, id) = download("/plain/20000000/a.bin?r=2000000", "plain", 8);
    let it = finished(&e, id, 60);
    verify(&it, 20_000_000);
    assert!(it.resumable && it.segments.len() > 1, "should use several connections");
}

#[test]
#[ignore]
fn scenario_02_server_without_ranges() {
    let (e, id) = download("/norange/5000000/b.bin", "norange", 8);
    let it = finished(&e, id, 60);
    verify(&it, 5_000_000);
    assert!(!it.resumable);
}

#[test]
#[ignore]
fn scenario_03_per_connection_throttle_is_beaten_by_connections() {
    // 6 MB at 300 KB/s per connection: 20 s with one connection, ~2.5 s with 8.
    let t = Instant::now();
    let (e, id) = download("/throttle/6000000/c.bin?r=300000", "throttle", 8);
    let it = finished(&e, id, 60);
    let secs = t.elapsed().as_secs_f64();
    println!("throttled server: {secs:.1}s with 8 connections (1 connection would need 20 s)");
    verify(&it, 6_000_000);
    assert!(secs < 6.0, "took {secs:.1}s");
}

#[test]
#[ignore]
fn scenario_04_chunked_unknown_size() {
    let (e, id) = download("/chunked/3000000/d.bin", "chunked", 8);
    let it = finished(&e, id, 60);
    verify(&it, 3_000_000);
    assert_eq!(it.size, Some(3_000_000));
}

#[test]
#[ignore]
fn scenario_05_redirect_chain() {
    let (e, id) = download("/redirect/3/4000000/e.bin", "redirect", 8);
    let it = finished(&e, id, 60);
    verify(&it, 4_000_000);
    assert_eq!(it.file_name, "e.bin");
    assert!(it.resumable);
}

#[test]
#[ignore]
fn scenario_06_expired_link_can_be_refreshed() {
    let size = 6_000_000;
    let link = |valid_for: u64| {
        let expiry = chrono::Local::now().timestamp() as u64 + valid_for;
        format!("{}/expiring/{expiry}/{size}/f.bin", base())
    };
    let e = Engine::new(vec![], vec![], 0);
    let id = e.add(&link(3), Some(out("expiring")), 4, false, None);
    e.set_item_limit(id, 1 << 20); // slow enough to pause half way
    e.start(id);
    wait(&e, id, |i| i.downloaded() > 1_500_000, 10);
    e.pause(id);
    wait(&e, id, |i| i.status == Status::Paused, 5);
    std::thread::sleep(Duration::from_secs(4)); // the link expires meanwhile
    e.start(id);
    let failed = finished(&e, id, 20);
    assert_eq!(failed.status, Status::Error, "expired link should fail");
    let msg = failed.error.clone().unwrap_or_default();
    println!("error shown: {msg}");
    assert!(msg.contains("403") && msg.to_lowercase().contains("expired"), "unhelpful message: {msg}");

    let kept = failed.downloaded();
    e.set_url(id, &link(600)).expect("change address");
    e.set_item_limit(id, 0);
    e.start(id);
    let it = finished(&e, id, 30);
    verify(&it, size);
    assert!(kept > 0, "progress before the refresh should be kept");
}

#[test]
#[ignore]
fn scenario_07_name_from_content_disposition() {
    let (e, id) = download("/cd/2000000/download.php?id=7", "cd", 4);
    let it = finished(&e, id, 60);
    verify(&it, 2_000_000);
    assert_eq!(it.file_name, "Laporan Akhir – 2026.pdf");
    assert_eq!(it.category, "Documents");
}

#[test]
#[ignore]
fn scenario_08_server_limits_connections() {
    // Server allows 3 connections; KilatX asks for 8 and must adapt, not fail.
    let (e, id) = download("/connlimit/12000000/g.bin?n=3&r=1000000", "connlimit", 8);
    let it = finished(&e, id, 60);
    verify(&it, 12_000_000);
}

#[test]
#[ignore]
fn scenario_09_cookie_required() {
    let e = Engine::new(vec![], vec![], 0);
    let url = format!("{}/cookie/3000000/h.bin", base());
    let without = e.add(&url, Some(out("cookie-no")), 4, true, None);
    let it = finished(&e, without, 20);
    assert_eq!(it.status, Status::Error);
    println!("error shown: {:?}", it.error);
    assert!(it.error.unwrap_or_default().contains("403"));

    let with = e.add(&url, Some(out("cookie-yes")), 4, false, None);
    e.set_headers(with, vec![("Cookie".into(), "session=ok".into())]);
    e.start(with);
    verify(&finished(&e, with, 30), 3_000_000);
}

#[test]
#[ignore]
fn scenario_10_basic_auth() {
    let e = Engine::new(vec![], vec![], 0);
    let host = base().replacen("http://", "", 1);
    let no = e.add(&format!("http://{host}/auth/2000000/i.bin"), Some(out("auth-no")), 4, true, None);
    let it = finished(&e, no, 20);
    assert_eq!(it.status, Status::Error);
    println!("error shown: {:?}", it.error);
    assert!(it.error.unwrap_or_default().contains("401"));

    let yes = e.add(&format!("http://user:pass@{host}/auth/2000000/i.bin"), Some(out("auth-yes")), 4, true, None);
    verify(&finished(&e, yes, 30), 2_000_000);
}

#[test]
#[ignore]
fn scenario_11_proxy() {
    let proxy = std::env::var("KILAT_SCEN_PROXY").expect("set KILAT_SCEN_PROXY");
    let count = || -> u64 {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            reqwest::Client::builder().no_proxy().build().unwrap()
                .get(format!("{}/__count", base())).send().await.unwrap().text().await.unwrap().parse().unwrap()
        })
    };
    let before = count();
    std::env::set_var("HTTP_PROXY", &proxy);
    std::env::set_var("NO_PROXY", "");
    let (e, id) = download("/plain/3000000/j.bin", "proxy", 4);
    std::env::remove_var("HTTP_PROXY");
    let it = finished(&e, id, 60);
    verify(&it, 3_000_000);
    assert!(count() > before, "download did not go through the proxy");
}

#[test]
#[ignore]
fn scenario_12_connection_dropped_mid_transfer() {
    let (e, id) = download("/drop/8000000/k.bin?k=300000", "drop", 6);
    let it = finished(&e, id, 60);
    verify(&it, 8_000_000);
}

#[test]
#[ignore]
fn scenario_13_empty_file() {
    let (e, id) = download("/empty/0/l.txt", "empty", 8);
    let it = finished(&e, id, 20);
    verify(&it, 0);
}

#[test]
#[ignore]
fn scenario_14_server_stops_honouring_ranges() {
    let (e, id) = download("/liar/6000000/m.bin?r=1000000", "liar", 8);
    let it = finished(&e, id, 60);
    verify(&it, 6_000_000);
}

#[test]
#[ignore]
fn scenario_15_http_errors_are_explained() {
    for (path, code) in [("/notfound/1/x.bin", "404"), ("/servererror/1/y.bin", "500")] {
        let (e, id) = download(path, "errors", 4);
        let it = finished(&e, id, 20);
        println!("{code}: {:?}", it.error);
        assert_eq!(it.status, Status::Error);
        assert!(it.error.unwrap_or_default().contains(code));
    }
}

#[test]
#[ignore]
fn scenario_16_tiny_file() {
    let (e, id) = download("/plain/1000/n.bin", "tiny", 8);
    verify(&finished(&e, id, 20), 1000);
}

#[test]
#[ignore]
fn scenario_17_same_file_twice_at_once() {
    let e = Engine::new(vec![], vec![], 0);
    let dir = out("twice");
    let url = format!("{}/plain/3000000/o.bin", base());
    let a = e.add(&url, Some(dir.clone()), 4, true, None);
    let b = e.add(&url, Some(dir.clone()), 4, true, None);
    let (ia, ib) = (finished(&e, a, 30), finished(&e, b, 30));
    verify(&ia, 3_000_000);
    verify(&ib, 3_000_000);
    assert_ne!(ia.file_name, ib.file_name, "second copy must get its own name");
}

#[test]
#[ignore]
fn scenario_18_unwritable_folder() {
    // A folder inside a file can never be created.
    let blocker = std::env::temp_dir().join("kilat-scenarios-blocker.txt");
    std::fs::write(&blocker, "x").unwrap();
    let e = Engine::new(vec![], vec![], 0);
    let id = e.add(&format!("{}/plain/1000/p.bin", base()), Some(blocker.join("sub")), 4, true, None);
    let it = finished(&e, id, 20);
    println!("error shown: {:?}", it.error);
    assert_eq!(it.status, Status::Error);
    assert!(it.error.unwrap_or_default().contains("folder"));
}

#[test]
#[ignore]
fn scenario_19_larger_than_4_gb() {
    let size: u64 = 4_600_000_000;
    let dir = out("huge");
    std::fs::create_dir_all(&dir).unwrap();
    let t = Instant::now();
    let (e, id) = download(&format!("/plain/{size}/huge.bin"), "huge", 8);
    let it = finished(&e, id, 900);
    println!("4.6 GB in {:.0}s", t.elapsed().as_secs_f64());
    verify(&it, size);
    let _ = std::fs::remove_file(it.final_path());
}

#[test]
#[ignore]
fn scenario_20_site_that_serves_browsers_a_web_page() {
    // Like SourceForge: browser user agents get an HTML page instead of the file.
    let (e, id) = download("/uasniff/2000000/setup.exe", "uasniff", 4);
    verify(&finished(&e, id, 30), 2_000_000);
}

#[test]
#[ignore]
fn scenario_21_web_page_instead_of_file_is_reported() {
    let (e, id) = download("/htmlpage/1/setup.exe", "htmlpage", 4);
    let it = finished(&e, id, 20);
    println!("error shown: {:?}", it.error);
    assert_eq!(it.status, Status::Error, "an HTML page must not be saved as setup.exe");
    assert!(it.error.unwrap_or_default().contains("web page"));
}

#[test]
#[ignore]
fn scenario_22_one_connection_stalls() {
    // One connection stops receiving data without closing. Idle connections must
    // take over its part instead of waiting for a timeout.
    let t = Instant::now();
    let (e, id) = download("/stall/8000000/q.bin?r=1000000", "stall", 8);
    let it = finished(&e, id, 90);
    let secs = t.elapsed().as_secs_f64();
    println!("with one stalled connection: {secs:.1}s, {} segments", it.segments.len());
    verify(&it, 8_000_000);
    assert!(it.segments.len() > 2, "test needs several connections to be meaningful");
    assert!(secs < 9.0, "waited for the stalled connection ({secs:.1}s)");
}

/// Not a pass/fail test: measures one real URL with 1, 2, 4 and 8 connections.
/// `set KILAT_PERF_URL=https://...` then `cargo test --release perf_connections -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_connections() {
    let url = std::env::var("KILAT_PERF_URL").expect("set KILAT_PERF_URL");
    let rounds: u32 = std::env::var("KILAT_PERF_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    let set: Vec<u32> = std::env::var("KILAT_PERF_CONNS")
        .ok()
        .map(|v| v.split(',').filter_map(|c| c.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 2, 4, 8]);
    // One engine per setting, so the line speed is remembered between rounds like in the app.
    let mut engines = std::collections::HashMap::new();
    for round in 1..=rounds {
        // alternate the order so neither setting always runs first
        let mut order = set.clone();
        if round % 2 == 0 {
            order.reverse();
        }
        for conns in order {
            let e = &*engines.entry(conns).or_insert_with(|| Engine::new(vec![], vec![], 0));
            let t = Instant::now();
            let id = e.add(&url, Some(out(&format!("perf-{conns}"))), conns, true, None);
            let it = finished(&e, id, 120);
            let secs = t.elapsed().as_secs_f64();
            let mbit = it.downloaded() as f64 * 8.0 / secs / 1e6;
            println!(
                "round {round}  {conns} connection(s): {secs:.2}s  {mbit:.0} Mbit/s  {:?}  segments={}  line={:.0} Mbit/s",
                it.status,
                it.segments.len(),
                e.line_peak() as f64 * 8.0 / 1e6
            );
        }
    }
}

#[test]
#[ignore]
fn scenario_23_busy_server_with_retry_after() {
    // The first two requests are answered 429 "Retry-After: 1"; KilatX must wait and go on.
    let (e, id) = download("/busy/3000000/r.bin", "busy", 4);
    verify(&finished(&e, id, 30), 3_000_000);
}

#[test]
#[ignore]
fn scenario_24_same_name_in_another_folder_keeps_its_name() {
    let e = Engine::new(vec![], vec![], 0);
    let url = format!("{}/plain/1000000/same.bin", base());
    // An unfinished download called same.bin in another folder...
    let waiting = e.add(&url, Some(out("name-a")), 2, false, None);
    // ...must not push this one to "same (1).bin".
    let id = e.add(&url, Some(out("name-b")), 2, true, None);
    let it = finished(&e, id, 20);
    verify(&it, 1_000_000);
    assert_eq!(it.file_name, "same.bin");
    assert_eq!(item(&e, waiting).status, Status::Queued);
}

#[test]
#[ignore]
fn scenario_25_fast_server_keeps_one_connection() {
    // The global limit plays the internet line (4 MB/s). The first download learns
    // the line speed; the second sees one connection fill the line and stays with it.
    let e = Engine::new(vec![], vec![], 4 << 20);
    let url = format!("{}/plain/6000000/s.bin", base());
    let first = e.add(&url, Some(out("fast-1")), 8, true, None);
    verify(&finished(&e, first, 30), 6_000_000);
    let second = e.add(&url, Some(out("fast-2")), 8, true, None);
    let it = finished(&e, second, 30);
    verify(&it, 6_000_000);
    println!("line speed learned: {:.1} MB/s, segments on the second download: {}", e.line_peak() as f64 / 1048576.0, it.segments.len());
    assert!(it.segments.len() <= 2, "opened {} connections on a full line", it.segments.len());
}

#[test]
#[ignore]
fn scenario_26_first_request_fails_to_connect() {
    // The very first request is cut before any answer (slow mirror, flaky Wi-Fi).
    let (e, id) = download("/flaky/2000000/t.bin", "flaky", 4);
    verify(&finished(&e, id, 30), 2_000_000);
}
