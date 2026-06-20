//! macOS (`.app` bundle) self-update — the twin of [`update_linux`].
//!
//! Reuses the same Ed25519-signed channel as Windows/Linux: [`update`] fetches
//! and verifies `updates-macos.json` (signature checked against the embedded
//! pubkey) before we touch anything. This module then brings the on-disk `.app`
//! up to that signed version and relaunches.
//!
//! The download is a zipped `.app`. We stage it next to the live bundle, require
//! the bytes to hash to the **signed** sha256, then atomically swap the bundle —
//! so a failed or tampered update leaves the running install untouched. macOS lets
//! us replace a running `.app` (the process keeps its open inodes).
//!
//! NOTE: this module is wired into `main.rs` under the name `update_linux` (via
//! `#[path]`) so the call sites are platform-agnostic — the public API below
//! intentionally mirrors `update_linux.rs` (`appimage_path`, `notify`, `apply`,
//! `relaunch_after_exit`).
//!
//! The Ed25519 signature only protects the *download channel*; the shipped `.app`
//! must independently be Developer-ID codesigned + notarized for Gatekeeper.

use anyhow::{anyhow, bail, Result};
use popyachsa_airplay::update::{self, Manifest};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::APP_NAME;

/// The running `*.app` bundle directory, if we're inside one
/// (`…/Foo.app/Contents/MacOS/exe` → `…/Foo.app`). `None` for a bare binary
/// (e.g. `cargo run`). Named to match the Linux module's `appimage_path()` so the
/// shared call sites in `main.rs` stay platform-agnostic.
pub fn appimage_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut p: &Path = exe.as_path();
    while let Some(parent) = p.parent() {
        if parent.extension().map_or(false, |e| e == "app") {
            return Some(parent.to_path_buf());
        }
        p = parent;
    }
    None
}

/// Best-effort desktop notification via `osascript` (no-ops if it fails — the
/// action still happens, the user just doesn't see a toast).
pub fn notify(summary: &str, body: &str) {
    let script = format!(
        "display notification {:?} with title {:?}",
        body,
        format!("{APP_NAME} — {summary}")
    );
    let _ = Command::new("osascript").arg("-e").arg(script).status();
}

/// Download the signed zip, verify it hashes to the signed sha256, swap the
/// running `.app`, and return its path so the caller can relaunch. The live
/// install is untouched unless the staged bundle matches the signed hash.
pub fn apply(m: &Manifest) -> Result<PathBuf> {
    let app = appimage_path()
        .ok_or_else(|| anyhow!("not running from a .app bundle — reinstall to update"))?;
    let parent = app
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", app.display()))?;

    // 1. Fetch (primary, then mirror).
    const CAP: u64 = 512 * 1024 * 1024;
    let bytes = match update::download(&m.url, CAP) {
        Ok(b) => b,
        Err(e) if !m.mirror_url.trim().is_empty() => {
            eprintln!("[update] primary {} failed ({e}); trying mirror", m.url);
            update::download(m.mirror_url.trim(), CAP)?
        }
        Err(e) => return Err(e),
    };

    // 2. Verify against the SIGNED hash before touching anything.
    let got = update::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(m.sha256.trim()) {
        bail!("downloaded zip sha256 {got} != signed {} — refusing", m.sha256.trim());
    }

    // 3. Unzip to a staging dir on the SAME filesystem (so the final rename is
    //    atomic and cross-device-free).
    let staging = parent.join(format!(
        ".{}.update",
        app.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| anyhow!("create staging dir: {e}"))?;
    unzip(&bytes, &staging)?;
    let new_app = find_app(&staging)?
        .ok_or_else(|| anyhow!("no .app found inside the downloaded zip"))?;

    // 4. Strip the download quarantine so Gatekeeper doesn't re-prompt.
    let _ = Command::new("xattr")
        .arg("-dr")
        .arg("com.apple.quarantine")
        .arg(&new_app)
        .status();

    // 5. Swap: move the old bundle aside, move the new one into place (same FS).
    let old = app.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(&app, &old).map_err(|e| anyhow!("move old bundle aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new_app, &app) {
        let _ = std::fs::rename(&old, &app); // roll back
        let _ = std::fs::remove_dir_all(&staging);
        return Err(anyhow!("install new bundle: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&staging);
    Ok(app)
}

/// Relaunch the updated bundle *after* this process exits (so the new instance
/// doesn't race the old one for the AirPlay ports / single-instance lock): a
/// detached shell waits for our PID to disappear, then `open -n`s the bundle.
pub fn relaunch_after_exit(app: &Path) {
    let pid = std::process::id();
    let path = app.to_string_lossy().replace('"', "\\\"");
    let script =
        format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; open -n \"{path}\"");
    let _ = Command::new("sh").arg("-c").arg(script).spawn();
}

/// Extract a zip blob into `dest` (pure-Rust via the `zip` crate, already a dep),
/// preserving unix permissions so the bundle's executable + dylibs stay runnable.
fn unzip(bytes: &[u8], dest: &Path) -> Result<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| anyhow!("open zip: {e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| anyhow!("zip entry {i}: {e}"))?;
        // enclosed_name() rejects absolute / `..` paths (zip-slip safe).
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).ok();
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let mut f =
            std::fs::File::create(&out).map_err(|e| anyhow!("create {}: {e}", out.display()))?;
        std::io::copy(&mut entry, &mut f).map_err(|e| anyhow!("write {}: {e}", out.display()))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

/// First `*.app` directory at the top level of `dir`.
fn find_app(dir: &Path) -> Result<Option<PathBuf>> {
    for e in std::fs::read_dir(dir).map_err(|e| anyhow!("read staging dir: {e}"))? {
        let p = e.map_err(|e| anyhow!("staging entry: {e}"))?.path();
        if p.is_dir() && p.extension().map_or(false, |x| x == "app") {
            return Ok(Some(p));
        }
    }
    Ok(None)
}
