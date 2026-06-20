//! Linux (AppImage) self-update.
//!
//! Reuses the same signed update channel as Windows: [`update`] fetches the
//! Ed25519-signed `updates-linux.json` and verifies the signature. This module
//! then brings the on-disk AppImage up to that signed version and relaunches.
//!
//! Two download paths, both gated by the *signed* sha256 so a compromised host
//! can never land a build the maintainer didn't sign:
//!   * **delta (preferred)** — the AppImage embeds zsync update-information and
//!     bundles `appimageupdatetool`; we delta-fetch only the changed blocks into
//!     a working copy, verify, then swap.
//!   * **full (fallback)** — download the whole AppImage from the manifest url.
//!
//! In both cases we operate on a *copy* and only rename it over the live
//! `$APPIMAGE` after the bytes hash to the signed value — so a failed or tampered
//! update leaves the running install untouched.
//!
//! Only AppImage installs self-update here (they expose `$APPIMAGE`). A distro
//! package (.deb/.rpm) updates through apt/dnf, so we refuse with a clear message.

use anyhow::{anyhow, bail, Result};
use popyachsa_airplay::update::{self, Manifest};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::APP_NAME;

/// The running .AppImage path, if we're inside one. `None` for a plain binary or
/// a distro-package install.
pub fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// The bundled `appimageupdatetool` (in the AppImage's usr/bin, next to our exe).
/// `None` if this build doesn't ship it (then we use the full-download path).
fn updater_tool() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let tool = dir.join("appimageupdatetool");
    tool.exists().then_some(tool)
}

/// Best-effort desktop notification (silently no-ops without `notify-send` /
/// libnotify — the action still happens, the user just doesn't see a toast).
pub fn notify(summary: &str, body: &str) {
    let _ = Command::new("notify-send")
        .arg("-a")
        .arg(APP_NAME)
        .arg(summary)
        .arg(body)
        .status();
}

/// Bring the on-disk AppImage up to the signed manifest version and return its
/// path so the caller can relaunch. Tries the delta path first, falls back to a
/// full download (both verify against the signed sha256).
pub fn apply(m: &Manifest) -> Result<PathBuf> {
    let target = appimage_path().ok_or_else(|| {
        anyhow!("not running as an AppImage — update through your package manager (apt/dnf)")
    })?;

    if let Some(tool) = updater_tool() {
        match apply_delta(&tool, &target, m) {
            Ok(p) => return Ok(p),
            // A bad delta (network, or a sha that doesn't match the signed hash)
            // falls through to the full download, which is gated by the same hash.
            Err(e) => eprintln!("[update] delta update failed ({e}); full download"),
        }
    }
    apply_full(&target, m)
}

/// Delta path: `appimageupdatetool` zsync-updates a working *copy* (seeded by the
/// current AppImage), we verify it, then swap.
fn apply_delta(tool: &Path, target: &Path, m: &Manifest) -> Result<PathBuf> {
    let work = target.with_extension("update"); // sibling in the same dir
    std::fs::copy(target, &work).map_err(|e| anyhow!("copy seed: {e}"))?;

    // Run the tool on the copy: it reads the copy's embedded zsync URL, fetches
    // the .zsync + only the changed blocks, and overwrites the copy in place.
    // APPIMAGE_EXTRACT_AND_RUN lets the (nested) tool run without libfuse.
    let st = Command::new(tool)
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .arg("--overwrite")
        .arg(&work)
        .status();
    let st = match st {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&work);
            return Err(anyhow!("run appimageupdatetool: {e}"));
        }
    };
    if !st.success() {
        let _ = std::fs::remove_file(&work);
        bail!("appimageupdatetool exit {:?}", st.code());
    }
    finish_verified_swap(&work, target, m)
}

/// Full path: download the whole AppImage (primary, then mirror) into a sibling,
/// verify, swap.
fn apply_full(target: &Path, m: &Manifest) -> Result<PathBuf> {
    const CAP: u64 = 512 * 1024 * 1024;
    let bytes = match update::download(&m.url, CAP) {
        Ok(b) => b,
        Err(e) if !m.mirror_url.trim().is_empty() => {
            eprintln!("[update] primary {} failed ({e}); trying mirror", m.url);
            update::download(m.mirror_url.trim(), CAP)?
        }
        Err(e) => return Err(e),
    };
    let work = target.with_extension("update");
    std::fs::write(&work, &bytes).map_err(|e| anyhow!("write {}: {e}", work.display()))?;
    finish_verified_swap(&work, target, m)
}

/// Shared tail: the staged `work` file must hash to the signed sha256; if so make
/// it executable and atomically rename it over `target`, else discard it (the
/// live install is untouched).
fn finish_verified_swap(work: &Path, target: &Path, m: &Manifest) -> Result<PathBuf> {
    let bytes = std::fs::read(work).map_err(|e| anyhow!("read staged update: {e}"))?;
    let got = update::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(m.sha256.trim()) {
        let _ = std::fs::remove_file(work);
        bail!("staged AppImage sha256 {got} != signed {} — refusing", m.sha256.trim());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(work, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| anyhow!("chmod staged update: {e}"))?;
    }
    std::fs::rename(work, target).map_err(|e| anyhow!("swap {}: {e}", target.display()))?;
    Ok(target.to_path_buf())
}

/// Relaunch the updated AppImage *after* this process exits, so the new instance
/// doesn't race the old one for the AirPlay ports / single-instance lock. A
/// detached shell polls until our PID disappears, then execs the AppImage.
pub fn relaunch_after_exit(appimage: &Path) {
    let pid = std::process::id().to_string();
    let path = appimage.to_string_lossy().into_owned();
    // Pass pid + path as positional argv ($1/$2) — NEVER interpolate the path into
    // the shell string: a path containing $(...), backticks, or \ would otherwise be
    // command injection (only the quotes were escaped before).
    let _ = Command::new("sh")
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$2""#)
        .arg("sh") // $0
        .arg(&pid) // $1
        .arg(&path) // $2
        .spawn();
}
