//! Checking for a new KilatX, Notepad++/VLC style: KilatX looks, then asks; nothing is
//! installed without the user saying so.
//!
//! The update server holds one small JSON file:
//! `{"version":"0.5.1","notes":"…","page":"https://…","files":{"x64":{"url":"…","sha256":"…"},
//!   "arm64":{…}},"sig":"<hex>"}`
//! `sig` is an Ed25519 signature (key in `update-signing/`, never shipped) over the same
//! JSON without `sig`. KilatX refuses a file whose signature or setup checksum doesn't match,
//! so a hacked server or a swapped download can't make it run anything else.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where KilatX looks for updates: the update.json attached to the latest GitHub release.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub const UPDATE_URL: &str = "https://github.com/marcos-liao/KilatX/releases/latest/download/update.json";
/// Public half of the update signing key (`update-signing/update.key`).
pub const PUBLIC_KEY: [u8; 32] = [
    0x1b, 0x19, 0x89, 0x15, 0x92, 0xbd, 0xa1, 0x32, 0xcd, 0x80, 0x49, 0x82, 0x7c, 0x37, 0xe2, 0x10, 0x5e, 0xc5, 0x29, 0x3b, 0x44, 0x43,
    0x81, 0xff, 0x09, 0xe8, 0xf1, 0x2e, 0xac, 0x97, 0xd2, 0x46,
];
/// Automatic checks: at most once a day.
pub const CHECK_EVERY: i64 = 24 * 60 * 60;
/// Always KilatX's own name, even when downloads use a browser's user agent.
const AGENT: &str = concat!("KilatX/", env!("CARGO_PKG_VERSION"));
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_SETUP: u64 = 300 << 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FileRef {
    pub url: String,
    pub sha256: String,
}

/// What the signature covers: everything in the file except `sig`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub files: BTreeMap<String, FileRef>,
}

#[derive(Deserialize, Serialize)]
pub struct Manifest {
    #[serde(flatten)]
    pub release: Release,
    pub sig: String,
}

impl Release {
    /// The exact bytes that are signed (field order and key order are fixed).
    pub fn signed_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The setup for this PC's processor.
    pub fn file(&self) -> Option<&FileRef> {
        self.files.get(arch())
    }
}

/// "x64" or "arm64", as the setups are named.
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "x64",
    }
}

pub fn public_key() -> [u8; 32] {
    // Debug builds only: unit tests sign with throwaway keys.
    #[cfg(debug_assertions)]
    if let Ok(hex) = std::env::var("KILATX_UPDATE_KEY") {
        if let Some(k) = from_hex(&hex).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
            return k;
        }
    }
    PUBLIC_KEY
}

pub fn update_url() -> String {
    // Debug builds (tests, development) never contact the real update server.
    #[cfg(debug_assertions)]
    return std::env::var("KILATX_UPDATE_URL").unwrap_or_else(|_| "http://127.0.0.1:9/debug-build".into());
    #[cfg(not(debug_assertions))]
    UPDATE_URL.to_string()
}

/// Reads an update file and checks its signature. Anything wrong is an error.
pub fn parse(json: &[u8], key: &[u8; 32]) -> Result<Release, String> {
    let m: Manifest = serde_json::from_slice(json).map_err(|e| format!("The update information is not valid ({e})."))?;
    let key = VerifyingKey::from_bytes(key).map_err(|_| "The update key is not valid.".to_string())?;
    let sig = from_hex(&m.sig).and_then(|b| <[u8; 64]>::try_from(b).ok()).ok_or("The update information is not signed.")?;
    key.verify(&m.release.signed_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| "The update information is not signed by KilatX. It was not used.".to_string())?;
    if parse_version(&m.release.version).is_none() {
        return Err("The update information has no valid version.".into());
    }
    Ok(m.release)
}

/// "1.2.3" → [1, 2, 3]; anything else (letters, empty parts) is not a version.
pub fn parse_version(v: &str) -> Option<Vec<u64>> {
    let parts: Option<Vec<u64>> = v.trim().split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty() && p.len() <= 4)
}

/// True if `candidate` is a later version than `current` (0.5 == 0.5.0).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parse_version(candidate), parse_version(current)) else { return false };
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

/// True if an automatic check is due (also when the clock went back).
pub fn check_due(last: i64, now: i64) -> bool {
    last <= 0 || now - last >= CHECK_EVERY || now < last
}

/// What a check found.
#[derive(Debug, PartialEq)]
pub enum Found {
    UpToDate,
    /// A newer version with a setup for this PC.
    Newer(Release),
}

/// Compares a checked release with the running version and the skipped one.
pub fn evaluate(release: Release, current: &str, skipped: &str, manual: bool) -> Found {
    if !is_newer(&release.version, current) || release.file().is_none() {
        return Found::UpToDate;
    }
    // "Skip this version" only quiets the automatic check.
    if !manual && !skipped.is_empty() && release.version.trim() == skipped.trim() {
        return Found::UpToDate;
    }
    Found::Newer(release)
}

/// Downloads and checks the update file.
pub fn check(client: &reqwest::Client, url: &str) -> Result<Release, String> {
    if !allowed_url(url) {
        return Err("The update address is not secure (https only).".into());
    }
    let rt = runtime()?;
    let bytes = rt.block_on(async {
        let r = client.get(url).header(reqwest::header::USER_AGENT, AGENT).timeout(Duration::from_secs(30)).send().await.map_err(|e| net_error(&e))?;
        if !r.status().is_success() {
            return Err(format!("The update server answered {}.", r.status()));
        }
        let mut body = Vec::new();
        let mut stream = r.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.map_err(|e| net_error(&e))?);
            if body.len() > MAX_MANIFEST {
                return Err("The update information is too large.".to_string());
            }
        }
        Ok(body)
    })?;
    parse(&bytes, &public_key())
}

/// Downloads the setup into `dir`, checking its size and SHA-256. Calls `progress`
/// with a percentage now and then.
pub fn download(client: &reqwest::Client, file: &FileRef, version: &str, dir: &Path, progress: &dyn Fn(u32)) -> Result<PathBuf, String> {
    if !allowed_url(&file.url) {
        return Err("The update address is not secure (https only).".into());
    }
    let expected = file.sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || from_hex(&expected).is_none() {
        return Err("The update information has no valid checksum.".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let safe_version: String = version.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    let path = dir.join(format!("KilatX-Setup-{safe_version}.exe"));
    let partial = path.with_extension("exe.part");
    let rt = runtime()?;
    let result = rt.block_on(async {
        use tokio::io::AsyncWriteExt;
        let r = client.get(&file.url).header(reqwest::header::USER_AGENT, AGENT).send().await.map_err(|e| net_error(&e))?;
        if !r.status().is_success() {
            return Err(format!("The update server answered {}.", r.status()));
        }
        let total = r.content_length().unwrap_or(0);
        if total > MAX_SETUP {
            return Err("The update file is too large.".to_string());
        }
        let mut out = tokio::fs::File::create(&partial).await.map_err(|e| format!("Cannot write {}: {e}", partial.display()))?;
        let mut hasher = Sha256::new();
        let (mut got, mut shown) = (0u64, u32::MAX);
        let mut stream = r.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| net_error(&e))?;
            got += chunk.len() as u64;
            if got > MAX_SETUP {
                return Err("The update file is too large.".to_string());
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await.map_err(|e| format!("Cannot write the update: {e}"))?;
            let pct = if total > 0 { (got * 100 / total) as u32 } else { 0 };
            if pct != shown {
                shown = pct;
                progress(pct);
            }
        }
        out.flush().await.map_err(|e| e.to_string())?;
        drop(out);
        if to_hex(&hasher.finalize()) != expected {
            return Err("The downloaded update is damaged or not the right file (checksum mismatch). It was deleted.".to_string());
        }
        Ok(())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&partial, &path).map_err(|e| format!("Cannot save the update: {e}"))?;
    Ok(path)
}

/// Starts the downloaded setup (Windows asks for administrator permission). The file is
/// opened so no other program can change it, checked once more, and only then started.
pub fn run_setup(path: &Path, expected_sha256: &str) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    const FILE_SHARE_READ: u32 = 1;
    // Read sharing only: nobody can write, rename or delete it until setup is running.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .map_err(|e| format!("Cannot open the update: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if to_hex(&hasher.finalize()) != expected_sha256.trim().to_ascii_lowercase() {
        return Err("The update file was changed after download. It was not started.".into());
    }
    let file = HSTRING::from(path.as_os_str());
    let params = HSTRING::from("/SILENT /SUPPRESSMSGBOXES /NORESTART /launch");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let started = unsafe { ShellExecuteExW(&mut info) };
    drop(f);
    match started {
        Ok(()) => {
            if !info.hProcess.is_invalid() {
                unsafe {
                    let _ = CloseHandle(info.hProcess);
                }
            }
            Ok(())
        }
        // ERROR_CANCELLED: "No" in the Windows permission prompt.
        Err(e) if e.code().0 as u32 == 0x8007_04C7 => Err("The update was cancelled (no administrator permission).".into()),
        Err(e) => Err(format!("Cannot start the update: {e}")),
    }
}

/// Release builds only talk to https addresses; debug builds also allow local test servers.
fn allowed_url(url: &str) -> bool {
    url.starts_with("https://") || (cfg!(debug_assertions) && url.starts_with("http://127.0.0.1"))
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())
}

fn net_error(e: &reqwest::Error) -> String {
    if e.is_connect() || e.is_timeout() {
        "Cannot reach the update server. Check the internet connection.".into()
    } else {
        format!("Update check failed: {e}")
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn release(version: &str) -> Release {
        let mut files = BTreeMap::new();
        for a in ["x64", "arm64"] {
            files.insert(a.to_string(), FileRef { url: format!("https://example.com/KilatX-Setup-{version}-{a}.exe"), sha256: "ab".repeat(32) });
        }
        Release { version: version.into(), notes: "Faster \"downloads\"\nand fixes".into(), page: "https://example.com".into(), files }
    }

    fn signed(r: &Release, k: &SigningKey) -> Vec<u8> {
        let sig = k.sign(&r.signed_bytes());
        serde_json::to_vec(&Manifest { release: r.clone(), sig: to_hex(&sig.to_bytes()) }).unwrap()
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.5.1", "0.5.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.5", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        assert!(!is_newer("0.5.0-beta", "0.4.0"));
        assert!(!is_newer("", "0.4.0"));
        assert!(!is_newer("1..2", "0.4.0"));
    }

    #[test]
    fn signed_file_is_accepted() {
        let k = key(7);
        let r = release("0.5.1");
        let got = parse(&signed(&r, &k), &k.verifying_key().to_bytes()).unwrap();
        assert_eq!(got, r);
    }

    #[test]
    fn changed_field_is_refused() {
        let k = key(7);
        let r = release("0.5.1");
        let json = String::from_utf8(signed(&r, &k)).unwrap();
        for (from, to) in [("0.5.1", "0.5.2"), ("https://example.com/KilatX-Setup-0.5.1-x64.exe", "https://evil.example/x.exe"), ("abab", "cdab"), ("Faster", "Slower")] {
            let tampered = json.replacen(from, to, 1);
            assert_ne!(tampered, json, "{from}");
            assert!(parse(tampered.as_bytes(), &k.verifying_key().to_bytes()).is_err(), "{from} → {to} accepted");
        }
    }

    #[test]
    fn added_field_or_file_is_refused() {
        let k = key(7);
        let r = release("0.5.1");
        let mut v: serde_json::Value = serde_json::from_slice(&signed(&r, &k)).unwrap();
        v["files"]["x86"] = serde_json::json!({"url": "https://evil.example/x.exe", "sha256": "00"});
        assert!(parse(&serde_json::to_vec(&v).unwrap(), &k.verifying_key().to_bytes()).is_err());
    }

    #[test]
    fn other_key_or_missing_signature_is_refused() {
        let r = release("0.5.1");
        assert!(parse(&signed(&r, &key(8)), &key(7).verifying_key().to_bytes()).is_err());
        let unsigned = serde_json::to_vec(&r).unwrap();
        assert!(parse(&unsigned, &key(7).verifying_key().to_bytes()).is_err());
        let bad_sig = serde_json::to_vec(&Manifest { release: r, sig: "zz".into() }).unwrap();
        assert!(parse(&bad_sig, &key(7).verifying_key().to_bytes()).is_err());
        assert!(parse(b"<html>not json</html>", &key(7).verifying_key().to_bytes()).is_err());
    }

    #[test]
    fn signed_but_invalid_version_is_refused() {
        let k = key(7);
        assert!(parse(&signed(&release("latest"), &k), &k.verifying_key().to_bytes()).is_err());
    }

    #[test]
    fn evaluate_skips_old_skipped_and_missing_arch() {
        assert_eq!(evaluate(release("0.4.0"), "0.5.0", "", true), Found::UpToDate);
        assert_eq!(evaluate(release("0.5.0"), "0.5.0", "", true), Found::UpToDate);
        assert!(matches!(evaluate(release("0.5.1"), "0.5.0", "", false), Found::Newer(_)));
        // Skipped: quiet for the automatic check, still shown when asked.
        assert_eq!(evaluate(release("0.5.1"), "0.5.0", "0.5.1", false), Found::UpToDate);
        assert!(matches!(evaluate(release("0.5.1"), "0.5.0", "0.5.1", true), Found::Newer(_)));
        // A later version than the skipped one is offered again.
        assert!(matches!(evaluate(release("0.5.2"), "0.5.0", "0.5.1", false), Found::Newer(_)));
        let mut no_setup = release("0.5.1");
        no_setup.files.remove(arch());
        assert_eq!(evaluate(no_setup, "0.5.0", "", true), Found::UpToDate);
    }

    #[test]
    fn automatic_check_once_a_day() {
        let now = 1_800_000_000;
        assert!(check_due(0, now));
        assert!(!check_due(now - 3600, now));
        assert!(check_due(now - CHECK_EVERY, now));
        assert!(check_due(now + 3600, now), "clock went back");
    }

    #[test]
    fn insecure_addresses_are_refused() {
        assert!(allowed_url(UPDATE_URL));
        assert!(!allowed_url("http://github.com/marcos-liao/KilatX/releases/latest/download/update.json"));
        assert!(!allowed_url("file:///C:/update.json"));
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(from_hex(&to_hex(&[0, 1, 254, 255])).unwrap(), vec![0, 1, 254, 255]);
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
        assert!(from_hex("éé").is_none());
    }

    /// Local server: (status, body) per request path.
    fn serve(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = routes.iter().find(|r| r.0 == path).map(|r| (r.1, r.2.clone())).unwrap_or((404, b"missing".to_vec()));
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}")
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[test]
    fn download_checks_the_checksum() {
        let setup = b"MZ pretend setup".repeat(1000);
        let good = to_hex(&Sha256::digest(&setup));
        let base = serve(vec![("/setup.exe", 200, setup.clone()), ("/gone.exe", 404, vec![])]);
        let dir = std::env::temp_dir().join(format!("kilatx-update-test-{}", std::process::id()));
        let file = FileRef { url: format!("{base}/setup.exe"), sha256: good.clone() };
        let pcts = std::sync::Mutex::new(Vec::new());
        let path = download(&client(), &file, "0.5.1", &dir, &|p| pcts.lock().unwrap().push(p)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), setup);
        assert_eq!(pcts.lock().unwrap().last(), Some(&100));

        let wrong = FileRef { url: format!("{base}/setup.exe"), sha256: "00".repeat(32) };
        let err = download(&client(), &wrong, "0.5.2", &dir, &|_| {}).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        assert!(!dir.join("KilatX-Setup-0.5.2.exe").exists() && !dir.join("KilatX-Setup-0.5.2.exe.part").exists());

        let missing = FileRef { url: format!("{base}/gone.exe"), sha256: good };
        assert!(download(&client(), &missing, "0.5.3", &dir, &|_| {}).unwrap_err().contains("404"));
        // A version string can't steer the file out of the folder.
        let odd = download(&client(), &file, "..\\..\\evil", &dir, &|_| {}).unwrap();
        assert_eq!(odd.parent().unwrap(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_reports_server_problems() {
        let k = key(9);
        let base = serve(vec![
            ("/ok.json", 200, signed(&release("0.9.0"), &k)),
            ("/err.json", 500, b"oops".to_vec()),
            ("/big.json", 200, vec![b' '; MAX_MANIFEST + 10]),
        ]);
        std::env::set_var("KILATX_UPDATE_KEY", to_hex(&k.verifying_key().to_bytes()));
        assert_eq!(check(&client(), &format!("{base}/ok.json")).unwrap().version, "0.9.0");
        assert!(check(&client(), &format!("{base}/err.json")).unwrap_err().contains("500"));
        assert!(check(&client(), &format!("{base}/big.json")).unwrap_err().contains("too large"));
        // Nothing listening: a clear message, no panic.
        let err = check(&client(), "http://127.0.0.1:9/update.json").unwrap_err();
        assert!(err.contains("Cannot reach"), "{err}");
    }

    #[test]
    fn run_setup_refuses_a_changed_file() {
        let dir = std::env::temp_dir().join(format!("kilatx-run-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("KilatX-Setup-9.9.9.exe");
        std::fs::write(&path, b"not the checked file").unwrap();
        let err = run_setup(&path, &"00".repeat(32)).unwrap_err();
        assert!(err.contains("changed"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
