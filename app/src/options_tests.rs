//! Scenarios 27-48: file types, save to, existing files, user agent, proxy, site logins.
//!
//! Needs `testkit/scenario_server.py 8766 8767`; the proxy port P starts
//! P = HTTP proxy, P+1 = HTTP proxy with login pu:pp, P+2 = SOCKS5, P+3 = SOCKS5 with su:sp.
//! `cargo test scenario_ -- --ignored --test-threads=1`

use std::path::PathBuf;

use crate::config::{CategoryDef, DownloadOptions, ExistingFile, ProxyMode, Secret, SiteLogin};
use crate::engine::{Engine, Item, Status};
use crate::scenario_tests::{base, finished, out, verify, wait};

fn configured(opts: DownloadOptions) -> Engine {
    let e = Engine::new(vec![], vec![], 0);
    e.configure(opts).expect("options rejected");
    e
}

fn proxy_port(offset: u16) -> u16 {
    let p = std::env::var("KILAT_SCEN_PROXY").expect("set KILAT_SCEN_PROXY");
    p.rsplit(':').next().unwrap().parse::<u16>().unwrap() + offset
}

fn server_get(path: &str) -> String {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        reqwest::Client::builder().no_proxy().build().unwrap()
            .get(format!("{}{path}", base())).send().await.unwrap().text().await.unwrap()
    })
}

fn server_counter(name: &str) -> u64 {
    server_get(&format!("/__{name}")).parse().unwrap()
}

fn proxy_opts(mode: ProxyMode, offset: u16, user: &str, pass: &str) -> DownloadOptions {
    let mut o = DownloadOptions::default();
    o.proxy.mode = mode;
    o.proxy.host = "127.0.0.1".into();
    o.proxy.port = proxy_port(offset);
    o.proxy.username = user.into();
    o.proxy.password = Secret(pass.into());
    o
}

/// Lets the engine choose the folder (no explicit folder), like the Add dialog's "Automatic".
fn add_auto(e: &Engine, path: &str) -> u64 {
    e.add(&format!("{}{path}", base()), None, 4, true, None)
}

#[test]
#[ignore]
fn scenario_27_extra_extension_goes_to_its_category() {
    let dir = out("types");
    let mut o = DownloadOptions { base_folder: Some(dir.clone()), ..Default::default() };
    o.categories.iter_mut().find(|c| c.name == "Compressed").unwrap().extensions.push("kxz".into());
    let e = configured(o);
    let it = finished(&e, add_auto(&e, "/plain/100000/archive.kxz"), 20);
    verify(&it, 100_000);
    assert_eq!(it.category, "Compressed");
    assert_eq!(it.save_dir, dir.join("Compressed"));
}

#[test]
#[ignore]
fn scenario_28_own_category_with_own_folder() {
    let dir = out("own-category");
    let mut o = DownloadOptions { base_folder: Some(dir.join("base")), ..Default::default() };
    o.categories.push(CategoryDef { name: "Images".into(), extensions: vec!["png".into()], folder: Some(dir.join("pics")) });
    let e = configured(o);
    let it = finished(&e, add_auto(&e, "/plain/50000/photo.png"), 20);
    verify(&it, 50_000);
    assert_eq!(it.category, "Images");
    assert_eq!(it.save_dir, dir.join("pics"));
}

#[test]
#[ignore]
fn scenario_29_unknown_type_goes_to_base_folder() {
    let dir = out("base");
    let e = configured(DownloadOptions { base_folder: Some(dir.clone()), ..Default::default() });
    let it = finished(&e, add_auto(&e, "/plain/1000/data.xyz"), 20);
    verify(&it, 1000);
    assert_eq!(it.save_dir, dir);
}

fn temp_folder_test(temp: PathBuf, name: &str) {
    let dir = out(&format!("temp-target-{name}"));
    let _ = std::fs::remove_dir_all(&temp);
    let e = configured(DownloadOptions { temp_folder: Some(temp.clone()), ..Default::default() });
    let id = e.add(&format!("{}/plain/3000000/{name}.bin?r=1000000", base()), Some(dir.clone()), 2, true, None);
    let during = wait(&e, id, |i| i.downloaded() > 500_000, 10);
    assert!(during.part_path().starts_with(&temp), "partial file should be in the temporary folder");
    assert!(during.part_path().exists());
    let it = finished(&e, id, 30);
    verify(&it, 3_000_000);
    assert!(it.final_path().starts_with(&dir));
    assert!(!it.part_path().exists(), "partial file left behind");
}

#[test]
#[ignore]
fn scenario_30_temporary_folder_same_drive() {
    temp_folder_test(std::env::temp_dir().join("kilat-scenarios").join("tmp-same"), "same-drive");
}

#[test]
#[ignore]
fn scenario_31_temporary_folder_on_another_drive() {
    // The project lives on D:, %TEMP% on C:: the finished file has to be copied across.
    let other = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("kilat-temp-other-drive");
    let drive = |p: &PathBuf| p.components().next().map(|c| c.as_os_str().to_owned());
    assert_ne!(drive(&std::env::temp_dir()), drive(&other), "test needs two different drives");
    temp_folder_test(other, "other-drive");
}

fn existing_file_case(policy: ExistingFile, tag: &str) -> (Engine, u64, PathBuf) {
    let dir = out(&format!("existing-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.bin"), b"OLD CONTENT").unwrap();
    let e = configured(DownloadOptions { existing_file: policy, ..Default::default() });
    let id = e.add(&format!("{}/plain/200000/report.bin", base()), Some(dir.clone()), 2, true, None);
    (e, id, dir)
}

#[test]
#[ignore]
fn scenario_32_existing_file_rename() {
    let (e, id, dir) = existing_file_case(ExistingFile::Rename, "rename");
    let it = finished(&e, id, 20);
    verify(&it, 200_000);
    assert_eq!(it.file_name, "report (1).bin");
    assert_eq!(std::fs::read(dir.join("report.bin")).unwrap(), b"OLD CONTENT");
}

#[test]
#[ignore]
fn scenario_33_existing_file_overwrite() {
    let (e, id, _) = existing_file_case(ExistingFile::Overwrite, "overwrite");
    let it = finished(&e, id, 20);
    assert_eq!(it.file_name, "report.bin");
    verify(&it, 200_000);
}

#[test]
#[ignore]
fn scenario_34_existing_file_skip() {
    let (e, id, dir) = existing_file_case(ExistingFile::Skip, "skip");
    let it = finished(&e, id, 20);
    println!("message: {:?}", it.error);
    assert_eq!(it.status, Status::Error);
    assert!(it.error.unwrap_or_default().starts_with("Skipped"));
    assert_eq!(std::fs::read(dir.join("report.bin")).unwrap(), b"OLD CONTENT");
}

#[test]
#[ignore]
fn scenario_35_existing_file_ask() {
    let (e, id, dir) = existing_file_case(ExistingFile::Ask, "ask");
    let waiting = wait(&e, id, |i| i.conflict.is_some(), 20);
    assert_eq!(waiting.status, Status::Paused, "should wait for the user");
    assert_eq!(waiting.conflict.as_deref(), Some("report.bin"));
    e.resolve_conflict(id, ExistingFile::Rename);
    let it = finished(&e, id, 20);
    verify(&it, 200_000);
    assert_eq!(it.file_name, "report (1).bin");
    assert_eq!(std::fs::read(dir.join("report.bin")).unwrap(), b"OLD CONTENT");
}

#[test]
#[ignore]
fn scenario_36_user_agent_setting() {
    let url = format!("{}/needua/1000/agent.bin?ua=MyAgent/7", base());
    let default = configured(DownloadOptions::default());
    let it = finished(&default, default.add(&url, Some(out("ua-default")), 1, true, None), 20);
    assert_eq!(it.status, Status::Error, "server only accepts MyAgent/7");
    let custom = configured(DownloadOptions { user_agent: "MyAgent/7 (test)".into(), ..Default::default() });
    let it = finished(&custom, custom.add(&url, Some(out("ua-custom")), 1, true, None), 20);
    verify(&it, 1000);
}

fn through(opts: DownloadOptions, counter: &str, tag: &str) -> (Item, u64) {
    let before = server_counter(counter);
    let e = configured(opts);
    let it = finished(&e, e.add(&format!("{}/plain/300000/{tag}.bin", base()), Some(out(tag)), 2, true, None), 30);
    (it, server_counter(counter) - before)
}

#[test]
#[ignore]
fn scenario_37_http_proxy() {
    let (it, used) = through(proxy_opts(ProxyMode::Http, 0, "", ""), "count", "proxy-http");
    verify(&it, 300_000);
    assert!(used > 0, "did not go through the proxy");
}

#[test]
#[ignore]
fn scenario_38_http_proxy_with_login() {
    let (it, _) = through(proxy_opts(ProxyMode::Http, 1, "", ""), "count", "proxy-auth-none");
    println!("without login: {:?}", it.error);
    assert_eq!(it.status, Status::Error);
    assert!(it.error.unwrap_or_default().contains("Settings › Proxy"));
    let (it, used) = through(proxy_opts(ProxyMode::Http, 1, "pu", "pp"), "count", "proxy-auth-ok");
    verify(&it, 300_000);
    assert!(used > 0);
}

#[test]
#[ignore]
fn scenario_39_socks5_proxy() {
    let (it, used) = through(proxy_opts(ProxyMode::Socks5, 2, "", ""), "socks_count", "socks");
    verify(&it, 300_000);
    assert!(used > 0, "did not go through SOCKS5");
}

#[test]
#[ignore]
fn scenario_40_socks5_proxy_with_login() {
    let (it, _) = through(proxy_opts(ProxyMode::Socks5, 3, "su", "wrong"), "socks_count", "socks-bad");
    println!("wrong login: {:?}", it.error);
    assert_eq!(it.status, Status::Error);
    assert!(it.error.unwrap_or_default().contains("proxy"));
    let (it, used) = through(proxy_opts(ProxyMode::Socks5, 3, "su", "sp"), "socks_count", "socks-ok");
    verify(&it, 300_000);
    assert!(used > 0);
}

#[test]
#[ignore]
fn scenario_41_no_proxy_ignores_system_proxy() {
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{}", proxy_port(0)));
    std::env::set_var("NO_PROXY", "");
    let mut o = DownloadOptions::default();
    o.proxy.mode = ProxyMode::None;
    let (it, used) = through(o, "count", "no-proxy");
    std::env::remove_var("HTTP_PROXY");
    verify(&it, 300_000);
    assert_eq!(used, 0, "\"No proxy\" still used the system proxy");
}

#[test]
#[ignore]
fn scenario_42_proxy_bypass_list() {
    let mut o = proxy_opts(ProxyMode::Http, 0, "", "");
    o.proxy.bypass = vec!["127.0.0.1".into()];
    let (it, used) = through(o, "count", "bypass");
    verify(&it, 300_000);
    assert_eq!(used, 0, "bypassed host went through the proxy");
}

#[test]
#[ignore]
fn scenario_43_https_through_http_proxy() {
    // Real internet: an HTTPS file through the local proxy (CONNECT tunnel).
    let before = server_counter("count");
    let e = configured(proxy_opts(ProxyMode::Http, 0, "", ""));
    let url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-x86_64-pc-windows-msvc.zip";
    let it = finished(&e, e.add(url, Some(out("https-proxy")), 4, true, None), 60);
    assert_eq!(it.status, Status::Complete, "{:?}", it.error);
    assert!(it.size.unwrap_or(0) > 1_000_000);
    assert!(server_counter("count") > before, "HTTPS did not go through the proxy");
}

#[test]
#[ignore]
fn scenario_44_proxy_unreachable() {
    let mut o = proxy_opts(ProxyMode::Http, 0, "", "");
    o.proxy.port = 9; // nothing listens there
    let e = configured(o);
    let it = finished(&e, e.add(&format!("{}/plain/1000/x.bin", base()), Some(out("proxy-down")), 1, true, None), 40);
    println!("message: {:?}", it.error);
    assert_eq!(it.status, Status::Error);
    assert!(it.error.unwrap_or_default().contains("through the proxy"));
}

fn login_opts(pattern: &str, user: &str, pass: &str) -> DownloadOptions {
    DownloadOptions {
        logins: vec![SiteLogin { pattern: pattern.into(), username: user.into(), password: Secret(pass.into()) }],
        ..Default::default()
    }
}

#[test]
#[ignore]
fn scenario_45_site_login_basic() {
    let e = configured(login_opts("127.0.0.1", "user", "pass"));
    let it = finished(&e, e.add(&format!("{}/auth/500000/members.bin", base()), Some(out("login-basic")), 4, true, None), 20);
    verify(&it, 500_000);
}

#[test]
#[ignore]
fn scenario_46_site_login_digest() {
    let e = configured(login_opts("127.0.0.1", "user", "pass"));
    let url = format!("{}/digest/2000000/nas.bin?r=1000000", base());
    let it = finished(&e, e.add(&url, Some(out("login-digest")), 4, true, None), 30);
    verify(&it, 2_000_000);
    println!("digest download used {} segments", it.segments.len());
}

#[test]
#[ignore]
fn scenario_47_site_login_wrong_password() {
    let e = configured(login_opts("127.0.0.1", "user", "nope"));
    let it = finished(&e, e.add(&format!("{}/auth/1000/x.bin", base()), Some(out("login-wrong")), 1, true, None), 20);
    println!("message: {:?}", it.error);
    assert_eq!(it.status, Status::Error);
    assert!(it.error.unwrap_or_default().contains("Login failed"));
}

#[test]
#[ignore]
fn scenario_48_login_is_not_sent_to_other_sites() {
    // A login for another site, and one for this site but another folder.
    let mut o = login_opts("files.example.com", "user", "secret1");
    o.logins.push(SiteLogin { pattern: "127.0.0.1/vip/".into(), username: "user".into(), password: Secret("secret2".into()) });
    let e = configured(o);
    let it = finished(&e, e.add(&format!("{}/whoauth/1000/public.bin", base()), Some(out("no-leak")), 1, true, None), 20);
    verify(&it, 1000);
    assert_eq!(server_get("/__auth?path=/whoauth/1000/public.bin"), "null", "a password was sent where it doesn't belong");
}
