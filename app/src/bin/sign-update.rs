//! Owner's tool to publish an update (not shipped to users).
//!
//! sign-update keygen <folder>        new signing key: <folder>\update.key (keep it secret,
//!                                    back it up), prints the public key for update.rs
//! sign-update pubkey <update.key>    prints the public key again
//! sign-update sign <update.key> <version> <notes.txt> <page-url> <out.json>
//!             --file <arch> <download-url> <setup.exe> [--file …]
//!                                    writes the signed update.json for the website

#[allow(dead_code)]
#[path = "../update.rs"]
mod update;

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("keygen") if args.len() == 2 => keygen(Path::new(&args[1])),
        Some("pubkey") if args.len() == 2 => load(Path::new(&args[1])).map(|k| print_public(&k)),
        Some("sign") if args.len() >= 6 => sign(&args[1..]),
        _ => Err("usage: see the top of src/bin/sign-update.rs".into()),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn keygen(dir: &Path) -> Result<(), String> {
    let path = dir.join("update.key");
    if path.exists() {
        return Err(format!("{} already exists; a new key would lock out every installed KilatX", path.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, update::to_hex(&seed)).map_err(|e| e.to_string())?;
    println!("wrote {}", path.display());
    print_public(&SigningKey::from_bytes(&seed));
    Ok(())
}

fn load(path: &Path) -> Result<SigningKey, String> {
    let hex = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let seed: [u8; 32] = update::from_hex(&hex).and_then(|b| b.try_into().ok()).ok_or("not a key file")?;
    Ok(SigningKey::from_bytes(&seed))
}

fn print_public(key: &SigningKey) {
    let bytes = key.verifying_key().to_bytes();
    let list: Vec<String> = bytes.iter().map(|b| format!("0x{b:02x}")).collect();
    println!("public key (hex): {}", update::to_hex(&bytes));
    println!("pub const PUBLIC_KEY: [u8; 32] = [{}];", list.join(", "));
}

fn sign(args: &[String]) -> Result<(), String> {
    let key = load(Path::new(&args[0]))?;
    let version = args[1].clone();
    if update::parse_version(&version).is_none() {
        return Err(format!("{version} is not a version like 0.5.1"));
    }
    let notes = if args[2] == "-" { String::new() } else { std::fs::read_to_string(&args[2]).map_err(|e| format!("{}: {e}", args[2]))? };
    let page = args[3].clone();
    let out = Path::new(&args[4]);
    let mut files = BTreeMap::new();
    let mut rest = &args[5..];
    while let [flag, arch, url, setup, tail @ ..] = rest {
        if flag != "--file" {
            return Err(format!("expected --file, got {flag}"));
        }
        let bytes = std::fs::read(setup).map_err(|e| format!("{setup}: {e}"))?;
        files.insert(arch.clone(), update::FileRef { url: url.clone(), sha256: update::to_hex(&Sha256::digest(&bytes)) });
        rest = tail;
    }
    if !rest.is_empty() || files.is_empty() {
        return Err("give at least one --file <arch> <url> <setup.exe>".into());
    }
    let release = update::Release { version, notes: notes.trim_end().to_string(), page, files };
    let sig = key.sign(&release.signed_bytes());
    let manifest = update::Manifest { release, sig: update::to_hex(&sig.to_bytes()) };
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    // Check it the way KilatX will.
    update::parse(json.as_bytes(), &key.verifying_key().to_bytes())?;
    std::fs::write(out, json).map_err(|e| e.to_string())?;
    println!("wrote {}", out.display());
    Ok(())
}
