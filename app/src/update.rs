//! Auto-update channel.
//!
//! Fetches a small JSON manifest over HTTPS, verifies an Ed25519 signature
//! against a public key embedded at compile time, and reports whether a newer
//! version is available. A compromised web host therefore cannot push a
//! malicious build — the private signing key never leaves the maintainer's
//! machine (see tools/make-update.py).
//!
//! Integrity chain:
//!   embedded pubkey  →  verify sig over (tag\nplatform\nversion\nsha256\nurl\npub_date)
//!                    →  artifact url looks like THIS platform's artifact
//!                    →  download url  →  check zip sha256 == signed sha256
//!
//! The signed message MUST stay byte-for-byte identical to the one produced by
//! tools/make-update.py::canonical_msg_v2.
//!
//! Signature format v2 (this release), and the only one accepted: the platform is
//! inside the signed bytes. Before it, one key signed three feeds over a message
//! with no platform in it, so a manifest lifted from any feed verified for every
//! client — copy the Windows manifest over updates-linux.json and every Linux
//! client renames a 99 MB zip over its AppImage. The feed still publishes that
//! legacy signature for clients ≤ 0.2.12; this build ignores it. See the
//! publishing-order note on `verify_signature`.

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

/// Which feed this build belongs to. Signed (v2), so a manifest published for
/// another platform cannot verify here even though one key signs all three feeds.
/// Must match `make-update.py sign --platform`.
#[cfg(windows)]
pub const PLATFORM: &str = "windows";
#[cfg(target_os = "linux")]
pub const PLATFORM: &str = "linux";
#[cfg(not(any(windows, target_os = "linux")))]
pub const PLATFORM: &str = "macos";

/// Filename extension this platform's artifact must have, lowercase.
///
/// This is the cheap half of the cross-feed guard and works against manifests
/// signed with the old platform-less message too — including ones already
/// published. It is deliberately weak: Windows and macOS both ship a `.zip`, so
/// it only separates Linux from the other two. That is where the damage was
/// (update_linux chmods+renames whatever it downloads over `$APPIMAGE`); the v2
/// signature is what actually binds a manifest to one platform.
#[cfg(target_os = "linux")]
const ARTIFACT_EXT: &str = ".appimage";
#[cfg(not(target_os = "linux"))]
const ARTIFACT_EXT: &str = ".zip";

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
    /// Publication date (`YYYY-MM-DD`), signed by v2 so it cannot be rewritten,
    /// but not yet *enforced* — nothing here rejects a stale manifest. Parsed and
    /// signed only so that adding a freshness policy later needs no second
    /// signature-format flag day.
    #[serde(default)]
    pub pub_date: String,
    /// v2 signature — message includes the platform (and pub_date). The only
    /// signature this build looks at; see `verify_signature`. (The feed also
    /// carries a legacy `signature` field for clients ≤ 0.2.12; it is deliberately
    /// not deserialised here, so there is nothing to fall back to.)
    #[serde(default)]
    pub signature_v2: String,
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

/// Ed25519-verify one hex signature over `msg` with the embedded pubkey.
fn verify_msg(sig_hex: &str, msg: &str) -> Result<()> {
    use ed25519_compact::{PublicKey, Signature};
    let pk_bytes = hex::decode(EMBEDDED_PUBKEY_HEX)?;
    let pk = PublicKey::from_slice(&pk_bytes).map_err(|e| anyhow!("embedded pubkey: {e}"))?;
    let sig_bytes =
        hex::decode(sig_hex.trim()).map_err(|_| anyhow!("manifest signature is not valid hex"))?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|e| anyhow!("signature: {e}"))?;
    pk.verify(msg.as_bytes(), &sig)
        .map_err(|_| anyhow!("manifest signature does not verify — refusing update"))
}

/// The v2 signed message. Byte-identical to make-update.py::canonical_msg_v2 —
/// both sides pin the same literal in a test, so drift on either side fails.
fn signed_msg_v2(m: &Manifest, platform: &str) -> String {
    format!(
        "{APP_TAG}\n{platform}\n{}\n{}\n{}\n{}",
        m.version, m.sha256, m.url, m.pub_date
    )
}

/// `signature_v2` is REQUIRED — the legacy message is never accepted here.
///
/// Backwards compatibility is a property of the FEED, not of this function: every
/// manifest `tools/make-update.py sign` writes carries both `signature` and
/// `signature_v2`, so clients ≤ 0.2.12 (which only know the legacy form and ignore
/// the extra field) keep updating untouched. Accepting the legacy form *here* buys
/// those clients nothing — they run their own old binary — and costs the platform
/// binding outright: the legacy message has no platform in it, so anyone who can
/// serve the feed strips `signature_v2` and a signed macOS manifest verifies for a
/// Windows client (both artifacts are `.zip`, so `check_platform_artifact` does not
/// catch it either). That is a downgrade attack, and the fix is to have no weaker
/// form to be downgraded to.
///
/// PUBLISHING ORDER, and it is not optional: the feed a client of this function
/// reads must already carry `signature_v2`, or that client refuses every check
/// and can only be moved off its installed build by a manual reinstall.
///
/// `make-update.py sign` has emitted both signatures since the v2 rollout, and
/// both CI release jobs plus the macOS publishing script call the in-repo script — so
/// publishing a release regenerates that platform's feed with both, which is what
/// normally keeps this safe. What is NOT safe is any path that leaves an old feed
/// in place: all three live feeds were fetched on 2026-09-10 and are still
/// legacy-only (`signature` present, `signature_v2` absent, all at 0.2.12),
/// because they were signed before the rollout. Re-uploading one of those next to
/// a ≥ 0.2.13 build is the way to break this.
///
/// (Dropping `signature` from the feed is the *later*, separate step, and only
/// once pre-0.2.13 has aged out.)
fn verify_signature(m: &Manifest) -> Result<()> {
    if m.signature_v2.trim().is_empty() {
        return Err(anyhow!(
            "manifest carries no signature_v2 — refusing update (a stripped signature \
             is how a manifest for another platform gets in)"
        ));
    }
    verify_msg(&m.signature_v2, &signed_msg_v2(m, PLATFORM))
}

/// Reject an artifact that is plainly not this platform's build.
///
/// Signature-verified says "the maintainer published this"; it does not say "for
/// your OS" on a legacy-signed manifest. Without this, a Windows manifest served
/// as updates-linux.json passes verification and update_linux renames a 99 MB zip
/// over the user's AppImage — install destroyed, no rollback.
fn check_platform_artifact(m: &Manifest) -> Result<()> {
    // Strip a query/fragment first: the extension lives in the path, not after `?`.
    let path = m.url.split(['?', '#']).next().unwrap_or(&m.url);
    if !path.to_ascii_lowercase().ends_with(ARTIFACT_EXT) {
        return Err(anyhow!(
            "manifest url {} is not a {ARTIFACT_EXT} — wrong platform's feed, refusing update",
            m.url
        ));
    }
    Ok(())
}

/// Fetch + parse + verify the manifest. Errors on network failure, bad JSON, a
/// signature that doesn't check out, or an artifact meant for another platform.
pub fn fetch_manifest() -> Result<Manifest> {
    let bytes = download(FEED_URL, 64 * 1024)?;
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| anyhow!("manifest json: {e}"))?;
    verify_signature(&m)?;
    check_platform_artifact(&m)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The real, live 0.2.12 macOS manifest, exactly as the feed serves it — i.e.
    /// with a legacy `signature` and no `signature_v2`, which is precisely the
    /// shape this build must now refuse.
    const LIVE_MACOS_MANIFEST: &str = r#"{
      "version": "0.2.12",
      "url": "https://airplay.popyachsa.com/download/PopyachsaAirPlay-macos-0.2.12.zip",
      "sha256": "d92bf79ba1c16afa7568fdce31ec47ec9829def9df482c0c6a672e6f52ebda54",
      "pub_date": "2026-06-22",
      "signature": "a9becb4ed0af8c01026ab62850adf39ea934fe3ee5bda6005a8826b9a49ff57f78fbf95ad833e4d0985983b0ccf781bff332407ea8ea0bea14f0d9f52e414d0e"
    }"#;

    /// That manifest's legacy signature. Not accepted by `verify_signature` any
    /// more, but it is a genuine signature by the embedded key, so it is what pins
    /// the crypto plumbing (hex → pubkey → verify) to something known-good.
    const LIVE_LEGACY_SIG: &str =
        "a9becb4ed0af8c01026ab62850adf39ea934fe3ee5bda6005a8826b9a49ff57f78fbf95ad833e4d0985983b0ccf781bff332407ea8ea0bea14f0d9f52e414d0e";

    fn parse(s: &str) -> Manifest {
        serde_json::from_str(s).expect("manifest json")
    }

    #[test]
    fn the_embedded_key_verifies_a_real_signature() {
        let m = parse(LIVE_MACOS_MANIFEST);
        let legacy_msg = format!("{APP_TAG}\n{}\n{}\n{}", m.version, m.sha256, m.url);
        verify_msg(LIVE_LEGACY_SIG, &legacy_msg).expect("embedded pubkey / verify_msg must work");
    }

    /// Stripping `signature_v2` used to drop us onto the platform-less legacy
    /// message, so a signed macOS manifest verified for a Windows client (both
    /// artifacts are `.zip`, so the extension check does not catch it). There is
    /// no weaker form to fall back to any more.
    #[test]
    fn a_manifest_without_v2_is_refused() {
        let mut m = parse(LIVE_MACOS_MANIFEST);
        assert!(m.signature_v2.is_empty()); // the field isn't even read
        assert!(verify_signature(&m).is_err());
        m.signature_v2 = "   ".into(); // blank is not "present"
        assert!(verify_signature(&m).is_err());
    }

    /// The signer and the verifier must agree byte-for-byte or every client stops
    /// updating. The same literal is asserted by `make-update.py selftest`.
    #[test]
    fn v2_message_matches_make_update_py() {
        let m = parse(
            r#"{"version":"1.2.3","url":"https://h/a.zip","sha256":"abc","pub_date":"2026-01-02"}"#,
        );
        assert_eq!(
            signed_msg_v2(&m, "linux"),
            "popyachsa-airplay\nlinux\n1.2.3\nabc\nhttps://h/a.zip\n2026-01-02"
        );
    }

    #[test]
    fn bad_v2_is_fatal_never_falls_back_to_legacy() {
        let mut m = parse(LIVE_MACOS_MANIFEST);
        m.signature_v2 = LIVE_LEGACY_SIG.into(); // a valid legacy sig is NOT a valid v2
        assert!(verify_signature(&m).is_err());
    }

    #[test]
    fn foreign_platform_artifact_is_rejected() {
        #[cfg(target_os = "linux")]
        let (ours, theirs) = ("https://x/App-x86_64.AppImage", "https://x/App.zip");
        #[cfg(not(target_os = "linux"))]
        let (ours, theirs) = ("https://x/App.zip", "https://x/App-x86_64.AppImage");

        let mut m = parse(LIVE_MACOS_MANIFEST);
        m.url = theirs.into();
        assert!(check_platform_artifact(&m).is_err());
        m.url = ours.into();
        assert!(check_platform_artifact(&m).is_ok());
        m.url = format!("{ours}?v=2"); // a query string must not hide the extension
        assert!(check_platform_artifact(&m).is_ok());
    }
}
