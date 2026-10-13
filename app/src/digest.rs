//! HTTP Digest login (RFC 7616), for sites listed in Site logins that ask for it.

use md5::{Digest as _, Md5};
use sha2::Sha256;

#[derive(Clone, Debug)]
pub struct Challenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub qop_auth: bool,
    pub algorithm: String,
    /// Request counter for this nonce (`nc`).
    count: u32,
}

impl Challenge {
    /// Parses `Digest realm="...", nonce="...", qop="auth", ...`; `None` for other schemes.
    pub fn parse(header: &str) -> Option<Self> {
        let rest = header.trim().strip_prefix("Digest")?.trim_start();
        let mut fields = std::collections::HashMap::new();
        for part in split_params(rest) {
            if let Some((k, v)) = part.split_once('=') {
                fields.insert(k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_string());
            }
        }
        Some(Self {
            realm: fields.get("realm").cloned().unwrap_or_default(),
            nonce: fields.get("nonce")?.clone(),
            opaque: fields.get("opaque").cloned(),
            qop_auth: fields.get("qop").is_some_and(|q| q.split(',').any(|x| x.trim() == "auth")),
            algorithm: fields.get("algorithm").cloned().unwrap_or_else(|| "MD5".into()),
            count: 0,
        })
    }

    /// The Authorization header for one request; advances the request counter.
    pub fn authorization(&mut self, method: &str, url: &str, user: &str, pass: &str) -> String {
        let seed = format!("{}:{}:{:?}", self.nonce, self.count, std::time::SystemTime::now());
        let cnonce = hex(&Md5::digest(seed.as_bytes()))[..16].to_string();
        self.build(method, url, user, pass, &cnonce)
    }

    fn build(&mut self, method: &str, url: &str, user: &str, pass: &str, cnonce: &str) -> String {
        self.count += 1;
        let uri = reqwest::Url::parse(url)
            .map(|u| match u.query() {
                Some(q) => format!("{}?{q}", u.path()),
                None => u.path().to_string(),
            })
            .unwrap_or_else(|_| "/".into());
        let sha = self.algorithm.to_ascii_uppercase().starts_with("SHA-256");
        let h = |s: String| if sha { hex(&Sha256::digest(s.as_bytes())) } else { hex(&Md5::digest(s.as_bytes())) };
        let nc = format!("{:08x}", self.count);
        let mut ha1 = h(format!("{user}:{}:{pass}", self.realm));
        if self.algorithm.to_ascii_lowercase().ends_with("-sess") {
            ha1 = h(format!("{ha1}:{}:{cnonce}", self.nonce));
        }
        let ha2 = h(format!("{method}:{uri}"));
        let response = if self.qop_auth {
            h(format!("{ha1}:{}:{nc}:{cnonce}:auth:{ha2}", self.nonce))
        } else {
            h(format!("{ha1}:{}:{ha2}", self.nonce))
        };
        let mut header = format!(
            "Digest username=\"{user}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", algorithm={}, response=\"{response}\"",
            self.realm, self.nonce, self.algorithm
        );
        if self.qop_auth {
            header += &format!(", qop=auth, nc={nc}, cnonce=\"{cnonce}\"");
        }
        if let Some(o) = &self.opaque {
            header += &format!(", opaque=\"{o}\"");
        }
        header
    }
}

/// Splits on commas that are not inside quotes.
fn split_params(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_2617_example() {
        // The worked example from RFC 2617, section 3.5.
        let mut ch = Challenge::parse(
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
        )
        .unwrap();
        let header = ch.build("GET", "http://www.nowhere.org/dir/index.html", "Mufasa", "Circle Of Life", "0a4f113b");
        assert!(header.contains(r#"response="6629fae49393a05397450978507c4ef1""#), "{header}");
        assert!(header.contains(r#"uri="/dir/index.html""#));
        assert!(header.contains("nc=00000001"));
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
    }

    #[test]
    fn other_schemes_are_ignored() {
        assert!(Challenge::parse(r#"Basic realm="x""#).is_none());
    }
}
