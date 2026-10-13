use std::fs::File;
use std::io;
use std::path::Path;

pub fn now() -> i64 {
    chrono::Local::now().timestamp()
}

pub fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    cleaned.trim().trim_end_matches('.').to_string()
}

pub fn name_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let last = parsed.path_segments()?.filter(|s| !s.is_empty()).last()?;
    let decoded = percent_encoding::percent_decode_str(last).decode_utf8_lossy();
    let name = sanitize(&decoded);
    (!name.is_empty()).then_some(name)
}

pub fn name_from_disposition(value: Option<&str>) -> Option<String> {
    let value = value?;
    // RFC 5987: filename*=UTF-8''name%20here.zip
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename*=") {
            let encoded = rest.splitn(2, "''").nth(1).unwrap_or(rest);
            let decoded = percent_encoding::percent_decode_str(encoded.trim_matches('"')).decode_utf8_lossy();
            let name = sanitize(&decoded);
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    for part in value.split(';') {
        if let Some(rest) = part.trim().strip_prefix("filename=") {
            let name = sanitize(rest.trim_matches('"'));
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Returns `name`, or `name (1).ext`, `name (2).ext`... until nothing on disk
/// (final file or partial file) and nothing in `taken` uses it.
pub fn unique_name(dir: &Path, name: &str, taken: &[String]) -> String {
    let free = |n: &str| {
        !dir.join(n).exists() && !dir.join(part_name(n)).exists() && !taken.iter().any(|t| t == n)
    };
    if free(name) {
        return name.to_string();
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|n| free(n))
        .unwrap()
}

pub fn part_name(name: &str) -> String {
    format!("{name}.kilatpart")
}

/// Marks a new file as sparse so writing far into it doesn't make NTFS fill
/// the gap with zeros first (about 0.8 s per GB on an SSD, far more on a hard disk).
pub fn make_sparse(file: &File) {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Ioctl::FSCTL_SET_SPARSE;
        use windows::Win32::System::IO::DeviceIoControl;
        let mut returned = 0u32;
        let _ = DeviceIoControl(HANDLE(file.as_raw_handle()), FSCTL_SET_SPARSE, None, 0, None, 0, Some(&mut returned), None);
    }
}

/// Renames, or copies and deletes when the temporary folder is on another drive.
pub fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // ERROR_NOT_SAME_DEVICE
        Err(e) if e.raw_os_error() == Some(17) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

pub fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_write(file, buf, offset)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::write_at(file, buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "disk write failed"));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Appends "kind<TAB>detail" to the file named by KILATX_EVENT_LOG, if set.
/// Lets automated tests see things that only happen on screen (dialogs, notifications).
pub fn event_log(kind: &str, detail: &str) {
    let Some(path) = std::env::var_os("KILATX_EVENT_LOG") else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = std::io::Write::write_all(&mut f, format!("{kind}	{detail}
").as_bytes());
    }
}

pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn fmt_speed(bps: f64) -> String {
    format!("{}/s", fmt_size(bps.max(0.0) as u64))
}

pub fn fmt_eta(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return String::new();
    }
    let s = secs.round() as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

pub fn fmt_time(ts: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts, 0).single() {
        Some(t) => t.format("%d %b %H:%M").to_string(),
        None => String::new(),
    }
}
