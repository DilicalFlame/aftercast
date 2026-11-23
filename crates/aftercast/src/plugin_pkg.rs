//! Plugin packages and registries (specs/2026-11-23-overlay-plugins.md, sections 3 and 12).
//!
//! A package is a zip of a plugin folder. Its sha256 is what a registry lists and what a signature
//! covers: the publisher signs the 32 byte digest with an ed25519 key. A user trusts a publisher by
//! listing `name:base64-public-key` in `[plugin_registry] trusted_keys`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ac_core::Home;
use ac_server::plugins::Plugin;

use crate::error::{AppError, Result};

fn usage<T>(message: impl Into<String>) -> Result<T> {
    Err(AppError::Usage(message.into()))
}

/// `<package>.sig.json`, or a registry entry's `signature` fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SigFile {
    pub sha256: String,
    /// The publisher's public key, base64.
    pub key: String,
    /// What the publisher calls themselves.
    pub signer: String,
    /// The signature of the digest, base64.
    pub signature: String,
}

/// What checking a package found out.
#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    Unsigned,
    /// Signed by a key the user trusts (its name).
    Trusted(String),
    /// Signed correctly, by a key the user has not said they trust (name, fingerprint).
    Unknown(String, String),
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fingerprint(key: &[u8]) -> String {
    Sha256::digest(key)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Checks a package against what is known about it: the digest a registry listed, and a signature.
pub fn check(
    bytes: &[u8],
    expected_sha: Option<&str>,
    sig: Option<&SigFile>,
    trusted: &[String],
) -> std::result::Result<Trust, String> {
    let digest = Sha256::digest(bytes);
    let hex = sha256_hex(bytes);
    if let Some(want) = expected_sha
        && !want.eq_ignore_ascii_case(&hex)
    {
        return Err(format!(
            "the package does not match the digest it was listed with (got {hex})"
        ));
    }
    let Some(sig) = sig else {
        return Ok(Trust::Unsigned);
    };
    if !sig.sha256.eq_ignore_ascii_case(&hex) {
        return Err("the signature is for a different package (the digest differs)".into());
    }
    let key = B64
        .decode(&sig.key)
        .map_err(|_| "the signing key is not base64")?;
    let key: [u8; 32] = key
        .as_slice()
        .try_into()
        .map_err(|_| "the signing key is not 32 bytes")?;
    let verifying = VerifyingKey::from_bytes(&key).map_err(|e| format!("the signing key: {e}"))?;
    let raw = B64
        .decode(&sig.signature)
        .map_err(|_| "the signature is not base64")?;
    let signature = Signature::from_slice(&raw).map_err(|e| format!("the signature: {e}"))?;
    verifying
        .verify(&digest, &signature)
        .map_err(|_| "the signature is not valid for this package".to_owned())?;
    for t in trusted {
        if let Some((name, k)) = t.split_once(':')
            && k.trim() == sig.key
        {
            return Ok(Trust::Trusted(name.trim().to_owned()));
        }
    }
    Ok(Trust::Unknown(sig.signer.clone(), fingerprint(&key)))
}

fn key_path(home: &Home, name: &str) -> PathBuf {
    home.join("state")
        .join("plugin-keys")
        .join(format!("{name}.key"))
}

fn name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A new signing key for `name`. The secret stays in `<home>/state/plugin-keys/`; the line to
/// share (and to put in someone's `trusted_keys`) is returned.
pub fn keygen(home: &Home, name: &str) -> Result<String> {
    if !name_ok(name) {
        return usage("a key name is 1 to 32 of a-z A-Z 0-9 - _");
    }
    let path = key_path(home, name);
    if path.exists() {
        return usage(format!("{} exists already", path.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| AppError::Usage(format!("no randomness: {e}")))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, B64.encode(seed))?;
    let public = SigningKey::from_bytes(&seed).verifying_key();
    Ok(format!("{name}:{}", B64.encode(public.to_bytes())))
}

fn load_key(home: &Home, name: &str) -> Result<SigningKey> {
    let path = key_path(home, name);
    let text = fs::read_to_string(&path).map_err(|_| {
        AppError::Usage(format!(
            "no key {name:?} (make one with `aftercast plugin keygen {name}`)"
        ))
    })?;
    let seed = B64
        .decode(text.trim())
        .map_err(|_| AppError::Usage(format!("{} is not a key", path.display())))?;
    let seed: [u8; 32] = seed
        .as_slice()
        .try_into()
        .map_err(|_| AppError::Usage(format!("{} is not a key", path.display())))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Zips a plugin folder (leaving out `node_modules`, `.git` and links) and, with a key, signs it.
/// Writes `<out>` and `<out>.sig.json`; returns the digest.
pub fn pack(home: &Home, dir: &Path, out: &Path, sign: Option<&str>) -> Result<String> {
    let plugin = Plugin::load(dir).map_err(AppError::Usage)?;
    let top = plugin.id.clone();
    let mut zip = zip::ZipWriter::new(fs::File::create(out)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    fn walk(
        zip: &mut zip::ZipWriter<fs::File>,
        options: zip::write::SimpleFileOptions,
        root: &Path,
        dir: &Path,
        top: &str,
    ) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            let n = name.to_string_lossy();
            if n == "node_modules" || n == ".git" {
                continue;
            }
            let kind = e.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if kind.is_dir() {
                walk(zip, options, root, &path, top)?;
            } else {
                zip.start_file(format!("{top}/{rel}"), options)
                    .map_err(|e| AppError::Usage(format!("zip: {e}")))?;
                zip.write_all(&fs::read(&path)?)?;
            }
        }
        Ok(())
    }
    walk(&mut zip, options, dir, dir, &top)?;
    zip.finish()
        .map_err(|e| AppError::Usage(format!("zip: {e}")))?;
    let bytes = fs::read(out)?;
    let sha = sha256_hex(&bytes);
    if let Some(name) = sign {
        let key = load_key(home, name)?;
        let signature = key.sign(&Sha256::digest(&bytes));
        let sig = SigFile {
            sha256: sha.clone(),
            key: B64.encode(key.verifying_key().to_bytes()),
            signer: name.to_owned(),
            signature: B64.encode(signature.to_bytes()),
        };
        let mut sig_path = out.as_os_str().to_owned();
        sig_path.push(".sig.json");
        fs::write(
            sig_path,
            serde_json::to_string_pretty(&sig).unwrap_or_default(),
        )?;
    }
    Ok(sha)
}

/// Reads `source`: an http(s) URL, a `file://` URL or a path.
pub async fn fetch(source: &str) -> Result<Vec<u8>> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| AppError::Usage(format!("http: {e}")))?;
        let res = client
            .get(source)
            .send()
            .await
            .map_err(|e| AppError::Usage(format!("{source}: {e}")))?;
        if !res.status().is_success() {
            return usage(format!("{source}: {}", res.status()));
        }
        return res
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| AppError::Usage(format!("{source}: {e}")));
    }
    let path = source
        .strip_prefix("file:///")
        .or_else(|| source.strip_prefix("file://"))
        .unwrap_or(source);
    Ok(fs::read(path)?)
}

/// What a registry says about one plugin.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    /// Where the zip is (an URL, or a path next to a local registry file).
    pub source: String,
    pub sha256: String,
    pub key: String,
    pub signer: String,
    pub signature: String,
}

impl Entry {
    pub fn sig(&self) -> Option<SigFile> {
        (!self.signature.is_empty()).then(|| SigFile {
            sha256: self.sha256.clone(),
            key: self.key.clone(),
            signer: self.signer.clone(),
            signature: self.signature.clone(),
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Index {
    plugins: Vec<Entry>,
}

/// Every entry of the registries in `[plugin_registry] urls`; a source that is not a URL is taken
/// relative to the registry file's own address.
pub async fn entries(urls: &[String]) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for url in urls {
        let bytes = fetch(url).await?;
        let index: Index = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Usage(format!("{url}: not a plugin registry: {e}")))?;
        let base = url.rsplit_once('/').map_or("", |(b, _)| b).to_owned();
        for mut e in index.plugins {
            if !e.source.contains("://") && !Path::new(&e.source).is_absolute() && !base.is_empty()
            {
                e.source = format!("{base}/{}", e.source);
            }
            out.push(e);
        }
    }
    Ok(out)
}
