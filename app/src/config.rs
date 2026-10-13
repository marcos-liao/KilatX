//! Download options (Settings dialog): file types and categories, save-to
//! folders, existing files, user agent, proxy and site logins.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CategoryDef {
    pub name: String,
    /// Lower-case extensions without dot.
    pub extensions: Vec<String>,
    /// Own folder; `None` means `<base folder>\<name>`.
    #[serde(default)]
    pub folder: Option<PathBuf>,
}

/// What to do when the file already exists in the target folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExistingFile {
    #[default]
    Rename,
    Overwrite,
    Skip,
    Ask,
}

/// What to do when a link that is already in the list is added again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplicateLink {
    #[default]
    Ask,
    AddAgain,
    Skip,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyMode {
    /// Windows proxy settings (and HTTP(S)_PROXY variables).
    #[default]
    System,
    None,
    Http,
    Socks5,
}

/// A password kept in memory as text but saved encrypted for the current Windows
/// user (DPAPI), so `state.json` never contains it in plain text.
#[derive(Clone, Default, PartialEq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() { "Secret(empty)" } else { "Secret(***)" })
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.0.is_empty() {
            return s.serialize_str("");
        }
        s.serialize_str(&format!("dpapi:{}", to_hex(&protect(self.0.as_bytes()))))
    }
}

/// Request headers whose values (cookies) are saved encrypted, like passwords.
pub mod secret_headers {
    use super::Secret;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(headers: &[(String, String)], s: S) -> Result<S::Ok, S::Error> {
        headers.iter().map(|(k, v)| (k.clone(), Secret(v.clone()))).collect::<Vec<_>>().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, String)>, D::Error> {
        let list = Vec::<(String, Secret)>::deserialize(d)?;
        // A value that can't be decrypted (other PC or user) is dropped with its header.
        Ok(list.into_iter().filter(|(_, v)| !v.0.is_empty()).map(|(k, v)| (k, v.0)).collect())
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        // Can't be read on another PC or by another user: comes back empty.
        let plain = text
            .strip_prefix("dpapi:")
            .and_then(from_hex)
            .and_then(|b| unprotect(&b))
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        Ok(Secret(plain))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default)]
    pub mode: ProxyMode,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: Secret,
    /// Hosts that skip the proxy (`localhost`, `*.corp.local`, ...).
    #[serde(default)]
    pub bypass: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SiteLogin {
    /// `example.com`, `*.example.com` or `files.example.com/members/`.
    pub pattern: String,
    pub username: String,
    #[serde(default)]
    pub password: Secret,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadOptions {
    #[serde(default = "default_categories")]
    pub categories: Vec<CategoryDef>,
    /// Extensions offered from the clipboard (and caught from browsers later).
    #[serde(default = "default_capture")]
    pub capture_types: Vec<String>,
    /// Sites never offered/caught (wildcards allowed).
    #[serde(default)]
    pub capture_exceptions: Vec<String>,
    /// `None`: the Windows Downloads folder.
    #[serde(default)]
    pub base_folder: Option<PathBuf>,
    /// Partial files go here while downloading; `None`: next to the final file.
    #[serde(default)]
    pub temp_folder: Option<PathBuf>,
    #[serde(default)]
    pub existing_file: ExistingFile,
    #[serde(default)]
    pub duplicate_link: DuplicateLink,
    /// Empty: KilatX's own agent.
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub logins: Vec<SiteLogin>,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            categories: default_categories(),
            capture_types: default_capture(),
            capture_exceptions: Vec::new(),
            base_folder: None,
            temp_folder: None,
            existing_file: ExistingFile::Rename,
            duplicate_link: DuplicateLink::Ask,
            user_agent: String::new(),
            proxy: ProxyConfig::default(),
            logins: Vec::new(),
        }
    }
}

/// Category for files that match no other.
pub const OTHER: &str = "General";

pub fn default_categories() -> Vec<CategoryDef> {
    let cat = |name: &str, exts: &str| CategoryDef {
        name: name.to_string(),
        extensions: exts.split_whitespace().map(str::to_string).collect(),
        folder: None,
    };
    vec![
        cat("Compressed", "zip rar 7z gz tar bz2 xz tgz iso cab"),
        cat("Documents", "pdf doc docx xls xlsx ppt pptx txt rtf odt ods csv epub md"),
        cat("Music", "mp3 wav flac aac ogg m4a wma opus"),
        cat("Programs", "exe msi apk dmg deb rpm appx msix msixbundle bat"),
        cat("Video", "mp4 mkv avi mov wmv flv webm m4v 3gp ts"),
    ]
}

fn default_capture() -> Vec<String> {
    default_categories().into_iter().flat_map(|c| c.extensions).collect()
}

pub const AGENT_PRESETS: [(&str, &str); 3] = [
    ("Chrome", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36"),
    ("Edge", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0"),
    ("Firefox", "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:143.0) Gecko/20100101 Firefox/143.0"),
];

pub fn default_agent() -> String {
    concat!("KilatX/", env!("CARGO_PKG_VERSION")).to_string()
}

impl DownloadOptions {
    pub fn agent(&self) -> String {
        let a = self.user_agent.trim();
        if a.is_empty() { default_agent() } else { a.to_string() }
    }

    pub fn category_for(&self, file_name: &str) -> String {
        let ext = crate::util::extension(file_name);
        if ext.is_empty() {
            return OTHER.into();
        }
        self.categories
            .iter()
            .find(|c| c.extensions.iter().any(|e| *e == ext))
            .map_or_else(|| OTHER.to_string(), |c| c.name.clone())
    }

    pub fn base(&self) -> PathBuf {
        self.base_folder.clone().unwrap_or_else(|| {
            dirs::download_dir()
                .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
                .unwrap_or_else(|| PathBuf::from("."))
        })
    }

    /// Where files of `category` go: its own folder, else `<base>\<category>`
    /// ("General" goes straight into the base folder).
    pub fn folder_for(&self, category: &str) -> PathBuf {
        if let Some(dir) = self.categories.iter().find(|c| c.name == category).and_then(|c| c.folder.clone()) {
            return dir;
        }
        if category == OTHER { self.base() } else { self.base().join(category) }
    }

    /// Should a copied/caught link be offered?
    pub fn wants(&self, url: &str) -> bool {
        crate::util::name_from_url(url).is_some_and(|name| self.wants_file(&name, url))
    }

    /// Should a file called `name`, from `url`, be taken (file type and site exceptions)?
    pub fn wants_file(&self, name: &str, url: &str) -> bool {
        let ext = crate::util::extension(name);
        if ext.is_empty() || !self.capture_types.iter().any(|t| *t == ext) {
            return false;
        }
        !self.is_excepted(url)
    }

    /// The site is in "Never offer links from these sites".
    pub fn is_excepted(&self, url: &str) -> bool {
        let host = host_of(url);
        self.capture_exceptions.iter().any(|p| host_matches(p, &host))
    }

    /// The most specific login whose pattern matches `url`.
    pub fn login_for(&self, url: &str) -> Option<&SiteLogin> {
        let host = host_of(url);
        let path = reqwest::Url::parse(url).map(|u| u.path().to_string()).unwrap_or_default();
        self.logins
            .iter()
            .filter(|l| {
                let (h, p) = split_pattern(&l.pattern);
                host_matches(h, &host) && path.starts_with(p)
            })
            .max_by_key(|l| {
                let (h, p) = split_pattern(&l.pattern);
                (!h.starts_with("*."), h.len() + p.len())
            })
    }
}

pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// `files.example.com/members/` -> ("files.example.com", "/members/"); scheme ignored.
fn split_pattern(pattern: &str) -> (&str, &str) {
    let p = pattern.trim();
    let p = p.split_once("://").map_or(p, |(_, rest)| rest);
    match p.find('/') {
        Some(i) => (&p[..i], &p[i..]),
        None => (p, "/"),
    }
}

/// `example.com` matches only that host; `*.example.com` matches it and every subdomain.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let pat = split_pattern(pattern).0.to_ascii_lowercase();
    match pat.strip_prefix("*.") {
        Some(base) => host == base || host.ends_with(&format!(".{base}")),
        None => !pat.is_empty() && host == pat,
    }
}

/// Comma/space/semicolon separated list, lower-cased, without dots or empties.
pub fn parse_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

#[cfg(windows)]
fn protect(data: &[u8]) -> Vec<u8> {
    dpapi(data, true).unwrap_or_default()
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    dpapi(data, false)
}

#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        let ok = if encrypt {
            CryptProtectData(&input, None, None, None, None, 0, &mut output)
        } else {
            CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
        };
        ok.ok()?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as _)));
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_patterns() {
        assert!(host_matches("example.com", "example.com"));
        assert!(!host_matches("example.com", "www.example.com"));
        assert!(host_matches("*.example.com", "www.example.com"));
        assert!(host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "badexample.com"));
        assert!(host_matches("https://Files.Example.com/members/", "files.example.com"));
    }

    #[test]
    fn most_specific_login_wins_and_others_get_nothing() {
        let login = |p: &str, u: &str| SiteLogin { pattern: p.into(), username: u.into(), password: Secret("x".into()) };
        let o = DownloadOptions {
            logins: vec![login("*.example.com", "wild"), login("files.example.com", "host"), login("files.example.com/vip/", "vip")],
            ..Default::default()
        };
        assert_eq!(o.login_for("https://files.example.com/vip/a.zip").unwrap().username, "vip");
        assert_eq!(o.login_for("https://files.example.com/pub/a.zip").unwrap().username, "host");
        assert_eq!(o.login_for("https://cdn.example.com/a.zip").unwrap().username, "wild");
        assert!(o.login_for("https://example.org/a.zip").is_none(), "no login for other sites");
    }

    #[test]
    fn capture_types_and_exceptions() {
        let o = DownloadOptions { capture_exceptions: vec!["*.google.com".into()], ..Default::default() };
        assert!(o.wants("https://example.com/setup.exe"));
        assert!(!o.wants("https://example.com/page.html"));
        assert!(!o.wants("https://dl.google.com/setup.exe"), "excepted site");
    }

    #[test]
    fn categories_and_folders() {
        let mut o = DownloadOptions { base_folder: Some(PathBuf::from(r"D:\DL")), ..Default::default() };
        o.categories[0].extensions.push("zst".into());
        o.categories.push(CategoryDef { name: "Images".into(), extensions: vec!["png".into()], folder: Some(PathBuf::from(r"E:\Pics")) });
        assert_eq!(o.category_for("a.tar.zst"), "Compressed");
        assert_eq!(o.category_for("cat.PNG"), "Images");
        assert_eq!(o.category_for("x.unknown"), OTHER);
        assert_eq!(o.folder_for("Images"), PathBuf::from(r"E:\Pics"));
        assert_eq!(o.folder_for("Video"), PathBuf::from(r"D:\DL\Video"));
        assert_eq!(o.folder_for(OTHER), PathBuf::from(r"D:\DL"));
    }

    #[test]
    fn passwords_are_saved_encrypted() {
        let o = DownloadOptions {
            logins: vec![SiteLogin { pattern: "a.com".into(), username: "u".into(), password: Secret("hunter2-Secret".into()) }],
            ..Default::default()
        };
        let json = serde_json::to_string(&o).unwrap();
        assert!(!json.contains("hunter2-Secret"), "password must not be in plain text");
        assert!(json.contains("dpapi:"));
        let back: DownloadOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.logins[0].password.0, "hunter2-Secret");
    }
}
