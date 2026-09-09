//! Popyachsa AirPlay self-update helper.
//!
//! Launched by the tray app when the user accepts an update:
//!   updater.exe --url <zip> --sha256 <hex> --dir <install_dir>
//!               --relaunch <exe> --wait-pid <pid>
//!
//! Flow:
//!   1. Re-exec a copy of ourselves from %TEMP% (so the install-dir updater.exe
//!      isn't locked and can be replaced by the new build).
//!   2. Wait for the tray app (wait-pid) to exit.
//!   3. Download the release zip, verify its SHA-256 against the signed value.
//!   4. Build the new install in a sibling `<dir>.new`, then swap it in (with
//!      rollback) — the live directory is never half-written.
//!   5. Relaunch the app.
//!
//! The zip's authenticity was already established by the tray app: it only
//! passes us a sha256 that came from a signature-verified manifest, and we
//! refuse to install anything whose bytes don't hash to exactly that.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use popyachsa_airplay::update;

struct Args {
    url: String,
    mirror_url: String,
    sha256: String,
    dir: PathBuf,
    relaunch: PathBuf,
    wait_pid: u32,
    staged: bool,
}

fn parse_args() -> Option<Args> {
    let mut url = None;
    let mut mirror_url = String::new();
    let mut sha256 = None;
    let mut dir = None;
    let mut relaunch = None;
    let mut wait_pid = 0u32;
    let mut staged = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--url" => url = it.next(),
            "--mirror-url" => mirror_url = it.next().unwrap_or_default(),
            "--sha256" => sha256 = it.next(),
            "--dir" => dir = it.next().map(PathBuf::from),
            "--relaunch" => relaunch = it.next().map(PathBuf::from),
            "--wait-pid" => wait_pid = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--staged" => staged = true,
            _ => {}
        }
    }
    Some(Args {
        url: url?,
        mirror_url,
        sha256: sha256?,
        dir: dir?,
        relaunch: relaunch?,
        wait_pid,
        staged,
    })
}

fn die(msg: &str) -> ! {
    error_box(msg);
    std::process::exit(1);
}

fn main() {
    let args = match parse_args() {
        Some(a) => a,
        None => die("Updater: missing arguments."),
    };

    // Stage 1: copy ourselves to %TEMP% and re-exec, so the install-dir
    // updater.exe is free to be overwritten by the new build.
    if !args.staged {
        if let Err(e) = restage_and_exec(&args) {
            die(&format!("Updater could not stage itself:\n{e}"));
        }
        return; // restage_and_exec spawns the staged copy and we exit.
    }

    // Stage 2 (running from %TEMP%): do the work.
    wait_for_exit(args.wait_pid, Duration::from_secs(30));

    // Try the primary URL; on any failure (network or checksum) fall back to the
    // mirror. Both paths verify the signed SHA-256, so a bad mirror can't sneak
    // in a tampered build.
    let zip = match fetch_verified(&args.url, &args.sha256) {
        Ok(b) => b,
        Err(e1) => {
            if !args.mirror_url.is_empty() {
                eprintln!("[updater] primary failed ({e1}); trying mirror {}", args.mirror_url);
                match fetch_verified(&args.mirror_url, &args.sha256) {
                    Ok(b) => b,
                    Err(e2) => die(&format!(
                        "Update download failed.\nprimary: {e1}\nmirror:  {e2}")),
                }
            } else {
                die(&format!("Update download failed:\n{e1}"));
            }
        }
    };

    // No blanket "your installation was left untouched" here — install_staged says
    // what it actually did, and for the one failure where that claim would be false
    // (the rollback rename failing) it says where the old build is instead.
    if let Err(e) = install_staged(&zip, &args.dir) {
        die(&format!("Install failed:\n{e}"));
    }

    // Relaunch the app, then exit. (We leave our temp copy behind; Windows
    // reclaims %TEMP% in due course.)
    let _ = Command::new(&args.relaunch).current_dir(&args.dir).spawn();
}

/// Download `url` and verify it hashes to `sha256_hex`. Returns the bytes, or an
/// error string (network failure OR checksum mismatch) so the caller can fall
/// back to a mirror.
fn fetch_verified(url: &str, sha256_hex: &str) -> Result<Vec<u8>, String> {
    let bytes = update::download(url, 512 * 1024 * 1024).map_err(|e| e.to_string())?;
    let got = update::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(sha256_hex.trim()) {
        return Err(format!("checksum mismatch (expected {sha256_hex}, got {got})"));
    }
    Ok(bytes)
}

/// Copy the running exe to %TEMP% and re-exec it with `--staged` + same args.
fn restage_and_exec(args: &Args) -> std::io::Result<()> {
    let me = std::env::current_exe()?;
    let staged = std::env::temp_dir().join(format!("pa-updater-{}.exe", args.wait_pid));
    std::fs::copy(&me, &staged)?;
    let mut cmd = Command::new(&staged);
    cmd.arg("--staged")
        .arg("--url").arg(&args.url)
        .arg("--sha256").arg(&args.sha256)
        .arg("--dir").arg(&args.dir)
        .arg("--relaunch").arg(&args.relaunch)
        .arg("--wait-pid").arg(args.wait_pid.to_string());
    if !args.mirror_url.is_empty() {
        cmd.arg("--mirror-url").arg(&args.mirror_url);
    }
    // Run the staged copy from %TEMP%, NOT from the install dir we inherited from
    // the tray: on Windows a directory that is some process's current directory
    // cannot be renamed, and the swap in install_staged() renames exactly that one.
    cmd.current_dir(std::env::temp_dir());
    cmd.spawn()?;
    Ok(())
}

/// Block until the given PID exits, or the timeout elapses. Best-effort: if the
/// process is already gone (or pid 0), returns immediately.
fn wait_for_exit(pid: u32, timeout: Duration) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            if !h.is_invalid() {
                let _ = WaitForSingleObject(h, timeout.as_millis() as u32);
                let _ = CloseHandle(h);
                // Small grace so file handles are fully released.
                std::thread::sleep(Duration::from_millis(400));
                return;
            }
        }
    }
    // Fallback: brief fixed wait.
    let start = Instant::now();
    while start.elapsed() < timeout.min(Duration::from_secs(3)) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Install the new build without ever leaving the live directory half-written.
///
/// This used to extract straight over `install_dir`. Any mid-extract failure — a
/// file locked because the user relaunched the app during the download (nothing
/// holds the single-instance lock once the tray exits), a power cut, an AV
/// quarantine — left a directory that was half 0.2.12 and half 0.2.11 with no
/// marker, and a mismatched exe/DLL pair is a documented tray-killing crash
/// (BACKLOG.md). Recovery was a manual reinstall.
///
/// So: assemble the finished tree in a sibling `<dir>.new`, then swap by renaming.
/// The rename is the only step that touches the live install and it either happens
/// or it doesn't. `.new` is seeded with a copy of the current install so files the
/// zip doesn't carry survive the swap — above all NSIS's `uninstall.exe`, whose
/// loss would orphan the Add/Remove Programs entry.
///
/// Mirrors `update_macos::apply`'s stage → rename → roll back shape.
fn install_staged(zip_bytes: &[u8], install_dir: &Path) -> anyhow::Result<()> {
    use anyhow::anyhow;
    let parent = install_dir
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", install_dir.display()))?;
    let name = install_dir
        .file_name()
        .ok_or_else(|| anyhow!("{} has no directory name", install_dir.display()))?
        .to_string_lossy()
        .into_owned();
    // Siblings, so the renames stay on one volume (a cross-device rename is a
    // copy+delete, i.e. no longer atomic).
    let staged = parent.join(format!("{name}.new"));
    let old = parent.join(format!("{name}.old"));
    // Recover from a run that died between the two renames below: the install then
    // exists ONLY as `.old`. This has to happen before the cleanup that follows,
    // which would otherwise delete the user's only copy and then fail on a missing
    // install dir.
    if !install_dir.exists() && old.is_dir() {
        std::fs::rename(&old, install_dir).map_err(|e| {
            anyhow!(
                "a previous update left the installation in {}; putting it back failed: {e}",
                old.display()
            )
        })?;
    }
    let _ = std::fs::remove_dir_all(&staged); // leftovers from an aborted run
    let _ = std::fs::remove_dir_all(&old);

    // ponytail: copy the whole install (~100 MB, seconds on any SSD) instead of
    // diffing which files the zip replaces. Swap to a diff only if the copy ever
    // shows up as slow.
    copy_dir_all(install_dir, &staged)
        .map_err(|e| anyhow!("stage a copy of the current install: {e}"))?;
    extract_over(zip_bytes, &staged)?;

    // Swap. On Windows renaming a directory that holds open files fails outright
    // rather than half-succeeding, which is the outcome we want: the live install
    // is still the old build and the user can retry after closing the app.
    std::fs::rename(install_dir, &old).map_err(|e| {
        anyhow!(
            "could not move the current install aside ({}): {e}\nIs Popyachsa AirPlay still running?",
            install_dir.display()
        )
    })?;
    if let Err(e) = std::fs::rename(&staged, install_dir) {
        // Report what the rollback actually did. Swallowing its error let the
        // caller tell the user the install was untouched while it was in fact
        // gone — the one message that must never be wrong.
        return Err(match std::fs::rename(&old, install_dir) {
            Ok(()) => anyhow!("install the new build: {e}\n\nThe old build was put back."),
            Err(e2) => anyhow!(
                "install the new build: {e}\n\nThe old build could NOT be put back ({e2}). \
                 It is intact in {} — rename that folder to {} , or reinstall.",
                old.display(),
                install_dir.display()
            ),
        });
    }
    let _ = std::fs::remove_dir_all(&old); // best-effort; harmless if it lingers
    Ok(())
}

/// Recursive directory copy. std only — not worth a dependency for fifteen lines.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Extract the zip into `dest` (the staging tree), stripping the single top-level
/// folder (the dist zip wraps everything in `PopyachsaAirPlay/`). Overwrites
/// existing files. Zip-slip safe via `enclosed_name`.
fn extract_over(zip_bytes: &[u8], install_dir: &Path) -> anyhow::Result<()> {
    use anyhow::anyhow;
    let reader = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader)?;
    std::fs::create_dir_all(install_dir)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(safe) = entry.enclosed_name() else {
            continue; // skip unsafe paths
        };
        // Strip the leading component (the wrapper folder).
        let mut comps = safe.components();
        comps.next();
        let rel: PathBuf = comps.as_path().to_path_buf();
        if rel.as_os_str().is_empty() {
            continue;
        }
        // enclosed_name() (zip 2.4.2) only guarantees the path never climbs above
        // ITS OWN root — it keeps interior `..`, so `PopyachsaAirPlay/../evil`
        // passes and dropping the wrapper leaves `../evil`, one level outside the
        // staging dir. Re-check what survived the strip; a real dist zip has none.
        if rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow!("zip entry {} escapes the install dir", safe.display()));
        }
        let dest = install_dir.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        // Overwrite. If a file is momentarily locked, retry a couple of times.
        let mut last_err = None;
        for _ in 0..5 {
            match std::fs::write(&dest, &bytes) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
        if let Some(e) = last_err {
            return Err(anyhow!("write {}: {e}", dest.display()));
        }
    }
    Ok(())
}

/// The swap is the one step that can destroy a user's install, and it cannot be
/// compiled for Windows on the maintainer's Mac — so exercise it here, where the
/// logic is plain `std::fs`.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A dist-shaped zip: everything under one wrapper folder, which the extractor
    /// strips.
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            w.start_file(format!("PopyachsaAirPlay/{name}"), opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn swap_installs_new_build_and_keeps_installer_owned_files() {
        let root = std::env::temp_dir().join(format!("pa-updater-test-{}", std::process::id()));
        let dir = root.join("PopyachsaAirPlay");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(dir.join("locales")).unwrap();
        std::fs::write(dir.join("popyachsa-airplay.exe"), b"old").unwrap();
        std::fs::write(dir.join("uninstall.exe"), b"nsis").unwrap(); // not in the zip
        std::fs::write(dir.join("locales/ru.json"), b"old").unwrap();

        let zip = make_zip(&[
            ("popyachsa-airplay.exe", b"new" as &[u8]),
            ("locales/ru.json", b"new"),
            ("uxplay-core.dll", b"new"),
        ]);
        install_staged(&zip, &dir).unwrap();

        assert_eq!(std::fs::read(dir.join("popyachsa-airplay.exe")).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("locales/ru.json")).unwrap(), b"new");
        assert!(dir.join("uxplay-core.dll").exists());
        assert_eq!(std::fs::read(dir.join("uninstall.exe")).unwrap(), b"nsis");
        assert!(!root.join("PopyachsaAirPlay.new").exists());
        assert!(!root.join("PopyachsaAirPlay.old").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_download_leaves_the_install_untouched() {
        let root = std::env::temp_dir().join(format!("pa-updater-bad-{}", std::process::id()));
        let dir = root.join("PopyachsaAirPlay");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("popyachsa-airplay.exe"), b"old").unwrap();

        assert!(install_staged(b"not a zip at all", &dir).is_err());
        assert_eq!(std::fs::read(dir.join("popyachsa-airplay.exe")).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `enclosed_name` lets `Wrapper/../x` through, and stripping the wrapper turns
    /// it into `../x`. Verified to write `<root>/pwned.txt` before the guard.
    #[test]
    fn an_entry_that_escapes_the_wrapper_is_refused() {
        let root = std::env::temp_dir().join(format!("pa-updater-slip-{}", std::process::id()));
        let dir = root.join("PopyachsaAirPlay");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("popyachsa-airplay.exe"), b"old").unwrap();

        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("PopyachsaAirPlay/../pwned.txt", opts).unwrap();
        w.write_all(b"pwned").unwrap();
        let zip = w.finish().unwrap().into_inner();

        assert!(install_staged(&zip, &dir).is_err());
        assert!(!root.join("pwned.txt").exists());
        assert_eq!(std::fs::read(dir.join("popyachsa-airplay.exe")).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The state a crash between the two renames leaves behind: install gone, `.old`
    /// holding everything. The next run must put it back, not delete it.
    #[test]
    fn a_run_interrupted_mid_swap_is_recovered() {
        let root = std::env::temp_dir().join(format!("pa-updater-crash-{}", std::process::id()));
        let dir = root.join("PopyachsaAirPlay");
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join("PopyachsaAirPlay.old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("popyachsa-airplay.exe"), b"old").unwrap();
        std::fs::write(old.join("uninstall.exe"), b"nsis").unwrap();

        install_staged(&make_zip(&[("popyachsa-airplay.exe", b"new" as &[u8])]), &dir).unwrap();

        assert_eq!(std::fs::read(dir.join("popyachsa-airplay.exe")).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("uninstall.exe")).unwrap(), b"nsis");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(windows)]
fn error_box(msg: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text = HSTRING::from(msg);
    let title = HSTRING::from("Popyachsa AirPlay — Update");
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn error_box(msg: &str) {
    eprintln!("{msg}");
}
