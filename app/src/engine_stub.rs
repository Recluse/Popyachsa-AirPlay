//! Non-Windows stub of the in-process AirPlay engine.
//!
//! Lets the tray app COMPILE and RUN on Linux/macOS while the real per-OS
//! host-window + `GstVideoOverlay` driver is built (Linux: L3 X11-XID bind;
//! macOS: M3 NSView). For now `start` just logs that the engine isn't wired on
//! this platform yet — the tray, Settings, About, config, i18n and the update
//! channel all work, and the L0/L1-validated `uxplay-core.so` will be driven
//! from here once the host window exists.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use crate::config::Config;
use crate::status::Status;

// Kept symmetric with the Windows engine so `main` wires the same status bridge.
// Nothing sends yet on this platform (no engine), so it is write-only for now.
#[allow(dead_code)]
static STATUS_TX: Mutex<Option<Sender<Status>>> = Mutex::new(None);

/// Main installs a sender so connection transitions reach the tray icon.
pub fn install_status_sender(tx: Sender<Status>) {
    *STATUS_TX.lock().unwrap() = Some(tx);
}

/// Engine log directory — a **user-writable** XDG path (`<data_dir>/logs`), NOT
/// next to the exe: a macOS `.app` bundle (and any system install) is read-only.
pub fn engine_log_dir() -> PathBuf {
    crate::config::log_dir()
}

/// Tray-side handle to the in-process engine (stub on non-Windows).
pub struct Engine {
    running: AtomicBool,
}

impl Engine {
    pub fn new() -> Self {
        Self { running: AtomicBool::new(false) }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn start(&self, _cfg: &Config) -> anyhow::Result<()> {
        eprintln!(
            "[engine] (stub) the in-process engine is not wired on this platform yet \
             (Linux host-window/overlay = L3; macOS = M3). Tray + Settings still work."
        );
        // Do NOT flip `running`: the engine isn't actually up, so the tray stays
        // "off" and Stop/Restart stay consistent with reality.
        Ok(())
    }

    pub fn stop(&self) {}

    /// Synchronous stop for app quit (uniform API; `stop` is already synchronous here).
    pub fn stop_blocking(&self) {}

    pub fn restart(&self, _cfg: &Config) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn set_topmost(&self, _on: bool) {}
}
