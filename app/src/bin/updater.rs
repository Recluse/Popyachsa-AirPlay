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
//!   4. Extract over the install dir (stripping the top-level folder).
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

    if let Err(e) = extract_over(&zip, &args.dir) {
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

/// Extract the zip into `install_dir`, stripping the single top-level folder
/// (the dist zip wraps everything in `PopyachsaAirPlay/`). Overwrites existing
/// files. Zip-slip safe via `enclosed_name`.
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
