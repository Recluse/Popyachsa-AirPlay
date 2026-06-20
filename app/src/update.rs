//! Auto-update channel.
//!
//! Fetches a small JSON manifest over HTTPS, verifies an Ed25519 signature
//! against a public key embedded at compile time, and reports whether a newer
//! version is available. A compromised web host therefore cannot push a
//! malicious build — the private signing key never leaves the maintainer's
//! machine (see tools/make-update.py).
//!
//! Integrity chain:
//!   embedded pubkey  →  verify sig over (tag\nversion\nsha256\nurl)
//!                    →  download url  →  check zip sha256 == signed sha256
//!
//! The signed message MUST stay byte-for-byte identical to the one produced by
//! tools/make-update.py::canonical_msg.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;

/// Where the signed manifest lives — one feed per platform (identical schema and
/// signing key; each feed's `url` points at that platform's artifact: Windows
/// zip, Linux AppImage, macOS dmg).
#[cfg(windows)]
pub const FEED_URL: &str = "https://airplay.popyachsa.com/download/updates.json";
#[cfg(target_os = "linux")]
pub const FEED_URL: &str = "https://airplay.popyachsa.com/download/updates-linux.json";
#[cfg(not(any(windows, target_os = "linux")))]
pub const FEED_URL: &str = "https://airplay.popyachsa.com/download/updates-macos.json";

/// Public half of the release signing key (see tools/make-update.py keygen).
const EMBEDDED_PUBKEY_HEX: &str =
    "b9518fcf9de8c5df08a75432e6fbe96e6d54e5233a3508d1a0639859eed3cbd5";

/// Domain-separation tag prefixed to the signed message.
const APP_TAG: &str = "popyachsa-airplay";

/// Version of *this* build, from Cargo.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub signature: String,
    /// Optional fallback download URL (e.g. a Google-hosted mirror) tried if the
    /// primary `url` fails. Not part of the signed payload — integrity is still
    /// guaranteed because whatever is downloaded must hash to the signed sha256.
    #[serde(default)]
    pub mirror_url: String,
}

/// HTTP GET into memory (capped, with a timeout). Used for the manifest and,
/// in the updater, for the release zip.
pub fn download(url: &str, cap_bytes: u64) -> Result<Vec<u8>> {
    // Refuse non-HTTPS. The feed URL is a compile-time https const and the artifact
    // url is signed, but a mis-configured signing run could embed http:// — which
    // strips TLS (still hash-gated, but never a reason to allow plaintext).
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(anyhow!("refusing non-https URL: {url}"));
    }
    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .get(url)
        .call()
        .map_err(|e| anyhow!("GET {url}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(cap_bytes)
        .read_to_end(&mut buf)
        .map_err(|e| anyhow!("read {url}: {e}"))?;
    Ok(buf)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn verify_signature(m: &Manifest) -> Result<()> {
    use ed25519_compact::{PublicKey, Signature};
    let pk_bytes = hex::decode(EMBEDDED_PUBKEY_HEX)?;
    let pk = PublicKey::from_slice(&pk_bytes).map_err(|e| anyhow!("embedded pubkey: {e}"))?;
    let sig_bytes = hex::decode(m.signature.trim())
        .map_err(|_| anyhow!("manifest signature is not valid hex"))?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|e| anyhow!("signature: {e}"))?;
    let msg = format!("{APP_TAG}\n{}\n{}\n{}", m.version, m.sha256, m.url);
    pk.verify(msg.as_bytes(), &sig)
        .map_err(|_| anyhow!("manifest signature does not verify — refusing update"))?;
    Ok(())
}

/// Fetch + parse + verify the manifest. Errors on network failure, bad JSON, or
/// (critically) a signature that doesn't check out.
pub fn fetch_manifest() -> Result<Manifest> {
    let bytes = download(FEED_URL, 64 * 1024)?;
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| anyhow!("manifest json: {e}"))?;
    verify_signature(&m)?;
    Ok(m)
}

/// Strict semver "is `remote` newer than `current`".
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (
        semver::Version::parse(remote.trim()),
        semver::Version::parse(current.trim()),
    ) {
        (Ok(r), Ok(c)) => r > c,
        _ => false,
    }
}

/// Returns `Some(manifest)` only when a *verified* newer version is available.
pub fn check_for_update() -> Result<Option<Manifest>> {
    let m = fetch_manifest()?;
    Ok(if is_newer(&m.version, CURRENT_VERSION) {
        Some(m)
    } else {
        None
    })
}
