//! Configuration model — `%APPDATA%\PopyachsaAirPlay\config.json`.
//!
//! # On-disk format
//!
//! Plain JSON with `#[serde(default)]` so older configs that don't carry the
//! newer keys still load with defaults filled in.  Pretty-printed on save
//! because the user is expected to open it in their editor occasionally for
//! ad-hoc tweaks; the main app's mtime watcher reloads + restarts uxplay
//! automatically when they hit Save.
//!
//! # Migration from "PopyachsaTV"
//!
//! Earlier builds stored everything under `%APPDATA%\PopyachsaTV`.  We renamed
//! the product to "Popyachsa AirPlay" mid-development.  On first run of a
//! renamed build, [`main`] checks whether [`legacy_data_dir`] exists and
//! [`data_dir`] does not, and if so, renames the folder so old config + logs
//! carry forward.  After that the legacy id is unused.
//!
//! # Source of truth
//!
//! `config.json` *is* the truth — the in-memory `Config` is a cached read.
//! Either the Settings sub-window or the user editing the file directly
//! changes the on-disk state; the tray's `config-watcher` thread observes
//! mtime, reloads, and pushes `AppEvent::ConfigChanged` into the event loop.
//! That handler also propagates `borderless` / `always_on_top` into the
//! shared `AtomicBool`s read by the focus watcher, so toggles apply live
//! without restarting uxplay.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const APP_ID: &str = "PopyachsaAirPlay";          // %APPDATA% folder, registry, mutex
pub const APP_NAME: &str = "Popyachsa AirPlay";        // user-visible product name
pub const APP_ID_LEGACY: &str = "PopyachsaTV";        // pre-rename id; auto-migrate

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub device_name: String,
    /// UI language: "auto" (detect from OS) or a code like "en"/"ru"/"de".
    pub language: String,
    pub autostart_with_windows: bool,
    pub autostart_on_app_launch: bool,
    pub fullscreen: bool,
    pub target_fps: u32,
    pub enable_h265: bool,
    /// Hardware video decoder: "d3d11" (DXVA, any GPU), "d3d12" (DXVA, any GPU),
    /// or "nvidia" (NVDEC). D3D11/12 also work on AMD/Intel.
    pub video_decoder: String,
    /// `""` = audio off; common values: wasapisink, wasapi2sink, autoaudiosink.
    pub audio_sink: String,
    pub debug_logging: bool,
    /// Display the renderer should appear on.  `None` = primary monitor.
    /// Indices match `monitors::list()` (zero-based, EnumDisplayMonitors order).
    /// Falls back to primary if the saved index no longer exists at startup.
    pub preferred_monitor: Option<u32>,
    /// Keep the uxplay video window pinned above the taskbar (HWND_TOPMOST).
    /// Off by default so the user can Alt+Tab away and reach the taskbar; turn
    /// on for kiosk-style "TV mode" where the picture should never be covered.
    pub always_on_top: bool,
    /// Strip the window frame + title bar from the video window (WS_POPUP).
    /// Off by default — when on, the video looks like a clean overlay.
    pub borderless: bool,
    /// Extra flags appended verbatim to the uxplay command line.
    pub custom_flags: String,
    /// On startup, quietly check the website for a newer signed build and
    /// prompt to install if one exists. On by default (best practice).
    pub check_updates_on_launch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: APP_NAME.to_string(),
            language: "auto".to_string(),
            autostart_with_windows: true,
            autostart_on_app_launch: true,
            // macOS opens WINDOWED by default (owner's choice); Windows/Linux keep
            // fullscreen-on-connect. The Settings checkbox controls it either way.
            fullscreen: !cfg!(target_os = "macos"),
            target_fps: 120,
            enable_h265: true,
            // Per-OS sensible defaults; the Settings UI shows OS-appropriate
            // choices and the per-OS engine maps these to GStreamer elements.
            // Windows: d3d11/wasapisink; macOS: VideoToolbox/Core-Audio; Linux &
            // other: auto-decode/system audio.
            video_decoder: if cfg!(windows) { "d3d11" }
                           else if cfg!(target_os = "macos") { "videotoolbox" }
                           else { "auto" }.to_string(),
            audio_sink: if cfg!(windows) { "wasapisink" } else { "autoaudiosink" }.to_string(),
            // Debug logging OFF by default: with it on, UxPlay emits a per-frame
            // DEBUG line that floods the host log callback on the streaming thread
            // and adds noticeable latency. Markers ("Begin streaming" etc.) are
            // INFO and still fire. Turn on in Settings only when diagnosing.
            debug_logging: false,
            preferred_monitor: None,
            always_on_top: false,
            borderless: false,
            custom_flags: String::new(),
            check_updates_on_launch: true,
        }
    }
}

/// Per-app folder under %APPDATA% — config, logs, and any future state.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join(APP_ID)
}

/// Older versions stored everything under %APPDATA%\\PopyachsaTV. Returns that
/// path so a one-shot migration on startup can pull settings forward.
pub fn legacy_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join(APP_ID_LEGACY)
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Self>(&text) {
                Ok(cfg) => return cfg,
                Err(e) => eprintln!("[config] {}: {e}; using defaults", path.display()),
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<()> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let text = serde_json::to_string_pretty(self)?;
        let path = config_path();
        // Atomic write: a crash/power-loss mid-write must NOT truncate the live
        // config — a half-written file fails to parse and load() silently falls
        // back to all-defaults, losing every setting. Write a sibling temp file,
        // then rename over the target (atomic replace on one filesystem; on Windows
        // std::fs::rename uses MoveFileEx + REPLACE_EXISTING).
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}
