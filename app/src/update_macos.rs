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
    let pid = std::process::id().to_string();
    let path = app.to_string_lossy().into_owned();
    // Pass pid + path as positional argv ($1/$2) — NEVER interpolate the path into
    // the shell string: a bundle path containing $(...), backticks, or a trailing \
    // would otherwise be command injection or a syntax error, and the only symptom
    // is that the app never comes back after the update. (Same fix as update_linux.)
    let _ = Command::new("sh")
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; open -n "$2""#)
        .arg("sh") // $0
        .arg(&pid) // $1
        .arg(&path) // $2
        .spawn();
}

/// Where a symlink at `base` pointing at `target` lands — `None` if that is
/// outside `root`.
///
/// Resolved lexically, not with `canonicalize`: none of this exists on disk yet,
/// and canonicalize would follow the very links we are creating. An absolute
/// target is refused outright — a relocatable `.app` has no business containing
/// one, so it can only be an escape attempt. The caller must additionally ensure
/// nothing on the way in or out is itself a symlink; see `unzip`.
fn resolve_within(root: &Path, base: &Path, target: &str) -> Option<PathBuf> {
    use std::path::Component;
    if Path::new(target).is_absolute() {
        return None;
    }
    let normalise = |p: &Path| -> PathBuf {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    };
    // If `..` popped past the root the candidate simply stops sharing its prefix.
    let landed = normalise(&base.join(target));
    landed.starts_with(normalise(root)).then_some(landed)
}

/// Extract a zip blob into `dest` (pure-Rust via the `zip` crate, already a dep),
/// preserving unix permissions so the bundle's executable + dylibs stay runnable.
fn unzip(bytes: &[u8], dest: &Path) -> Result<()> {
    use std::path::Component;
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| anyhow!("open zip: {e}"))?;
    // Every symlink we have created so far. A zip escapes the lexical guard below
    // by CHAINING links — `a -> .` then `a/b -> ..` writes `<dest>/b -> ..`, and
    // each hop passes `resolve_within` on its own because the kernel resolves the
    // second one THROUGH the first. The staging dir starts empty, so these are the
    // only symlinks in the tree: a path that crosses none of them resolves exactly
    // as `resolve_within` predicts, and every link then provably points inside.
    // ponytail: crossing a link is refused outright rather than followed. Our zip
    // has exactly one (Contents/Frameworks/GStreamer, make-app.sh), and nothing is
    // stored under it. Repackaging GStreamer as a real Versions/A framework would
    // introduce `Headers -> Versions/Current/Headers` chains and need real
    // following here.
    let mut links: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let crosses = |links: &std::collections::HashSet<PathBuf>, p: &Path| {
        p.ancestors().skip(1).any(|a| links.contains(a))
    };
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| anyhow!("zip entry {i}: {e}"))?;
        // enclosed_name() rejects absolute paths and any prefix that climbs above
        // the root, but it KEEPS interior `..` components (`a/../b`). Refuse them:
        // it costs nothing (a ditto'd bundle has none) and it buys the invariant
        // that `dest.join(rel)` is dest plus plain components.
        let Some(rel) = entry.enclosed_name() else { continue };
        if rel.components().any(|c| matches!(c, Component::ParentDir)) {
            bail!("zip entry {} contains `..`", rel.display());
        }
        let out = dest.join(rel);
        if crosses(&links, &out) {
            bail!("zip entry {} is written through a symlink", out.display());
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&out).ok();
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).ok();
        }
        // Symlinks must be recreated as symlinks. A zip stores a symlink's TARGET
        // as the entry body, so the plain File::create path below would write
        // `Contents/Frameworks/GStreamer` as a 22-byte text file where dyld expects
        // a directory — the engine would never load again. make-app.sh creates
        // exactly that link so codesign seals it as one resource instead of
        // refusing to descend it.
        #[cfg(unix)]
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            use std::io::Read;
            let mut target = String::new();
            entry
                .read_to_string(&mut target)
                .map_err(|e| anyhow!("read link {}: {e}", out.display()))?;
            // A symlink inside an archive is an escape primitive: a LATER entry
            // whose path goes through this link would be written wherever it
            // points. enclosed_name() only vets the entry's own path, so the
            // target has to be vetted here or the zip-slip guard has a back door.
            let Some(landed) = resolve_within(dest, out.parent().unwrap_or(dest), &target) else {
                bail!("zip symlink {} escapes the bundle", out.display());
            };
            // …and the target must not be reached through an earlier link either,
            // or the lexical answer above is not the one the kernel gives.
            if crosses(&links, &landed) {
                bail!("zip symlink {} resolves through a symlink", out.display());
            }
            links.insert(out.clone());
            let _ = std::fs::remove_file(&out);
            std::os::unix::fs::symlink(&target, &out)
                .map_err(|e| anyhow!("symlink {}: {e}", out.display()))?;
            continue;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The symlink guard is the only thing standing between a hostile update zip
    /// and an arbitrary write outside the bundle, so it gets the one check.
    #[test]
    fn symlink_targets_may_not_leave_the_bundle() {
        let root = Path::new("/tmp/stage/Popyachsa AirPlay.app");
        let frameworks = root.join("Contents/Frameworks");
        let ok = |base: &Path, t: &str| resolve_within(root, base, t).is_some();

        // The real link make-app.sh creates.
        assert!(ok(&frameworks, "../Resources/GStreamer"));
        assert!(ok(root, "Contents/MacOS"));
        assert!(ok(&frameworks, "./sibling"));

        // Escapes.
        assert!(!ok(&frameworks, "../../../../etc"));
        assert!(!ok(root, "../outside"));
        assert!(!ok(&frameworks, "/etc/passwd"));
        assert!(!ok(&frameworks, "/"));
        // Lands exactly on the parent of the root, not inside it.
        assert!(!ok(root, ".."));
    }

    /// The lexical guard above is per-entry, so two links that each pass can still
    /// compose into an escape. Verified to write `<root>/outside.txt` before the
    /// `links` set was added to `unzip`.
    #[test]
    fn chained_symlinks_cannot_compose_into_an_escape() {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.add_symlink("a", ".", opts).unwrap(); // <staging>/a -> <staging>
        w.add_symlink("a/b", "..", opts).unwrap(); // really <staging>/b -> <root>
        w.start_file("b/outside.txt", opts).unwrap(); // really <root>/outside.txt
        w.write_all(b"pwned").unwrap();
        let bytes = w.finish().unwrap().into_inner();

        let root = std::env::temp_dir().join(format!("pa-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let err = unzip(&bytes, &staging).unwrap_err();
        assert!(!root.join("outside.txt").exists(), "escaped: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
