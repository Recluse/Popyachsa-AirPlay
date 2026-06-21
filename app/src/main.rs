//! Popyachsa AirPlay — system-tray AirPlay receiver with an in-process engine.
//!
//! # What this process does
//!
//! `popyachsa-airplay.exe` puts a tray icon next to the clock with three jobs:
//!
//! 1. **Run the AirPlay engine in-process.** Instead of spawning `uxplay.exe`,
//!    we load `uxplay-core.dll` (a patched UxPlay fork exposing a flat C ABI)
//!    via the `airplay-lib` crate, create a renderer window WE own, and hand
//!    its HWND to the engine so the mirror renders straight into our window.
//!    See [`engine`] — it spawns a dedicated host-window thread that owns the
//!    window + the engine + its Win32 message pump.
//!
//! 2. **Surface state via the tray icon** (off / ready / connected). The menu
//!    offers Start / Stop / Restart / Always-on-top / Settings… / open logs /
//!    About / Quit. (Per-stream "connected" detection returns once the engine
//!    status callback is wired to UxPlay connection events.)
//!
//! 3. **Own the renderer window natively.** Because the window is ours, the
//!    chrome work (borderless / drag / aspect-resize / fullscreen / snap / PiP)
//!    is plain Win32 in our own WndProc, not cross-process poking.
//!
//! # Sub-windows
//!
//! Settings and About each run as a re-launched `popyachsa-airplay.exe
//! --settings` / `--about` subprocess so eframe can own its own event loop
//! (tao owns ours). They communicate purely through `config.json`; the tray's
//! config-watcher thread turns "Settings saved" into "engine restarts".

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod about_ui;
mod autostart;
mod config;
// The in-process engine driver is per-OS: real Win32 host-window on Windows,
// real raw-Xlib host-window on Linux (GstVideoOverlay XID), and a stub elsewhere
// (so the tray/Settings/About/update all run) until that platform's host-window
// driver lands (macOS M3).
#[cfg(windows)]
#[path = "engine.rs"]
mod engine;
#[cfg(target_os = "macos")]
#[path = "engine_macos.rs"]
mod engine;
#[cfg(target_os = "linux")]
#[path = "engine_linux.rs"]
mod engine;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
#[path = "engine_stub.rs"]
mod engine;
mod fonts;
mod i18n;
mod monitors;
mod settings_ui;
mod status;
/// Per-OS in-process self-update, exposed under one module name so the call sites
/// stay platform-agnostic: the AppImage updater on Linux (`update_linux.rs`) and
/// the `.app`-bundle twin on macOS (`update_macos.rs`, same API). Windows uses the
/// sibling `updater.exe` instead.
#[cfg(target_os = "linux")]
mod update_linux;
#[cfg(target_os = "macos")]
#[path = "update_macos.rs"]
mod update_linux;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::Result;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

use crate::config::{data_dir, Config, APP_NAME};
use crate::engine::Engine;
use crate::status::Status;
use popyachsa_airplay::update;

// Embed the .ico bytes so the dist exe has no run-time icon dependency.
const ICON_OFF_BYTES:       &[u8] = include_bytes!("../icons/tray-off.ico");
const ICON_READY_BYTES:     &[u8] = include_bytes!("../icons/tray-ready.ico");
const ICON_CONNECTED_BYTES: &[u8] = include_bytes!("../icons/tray-connected.ico");

fn icon_for(status: Status) -> Icon {
    let bytes = match status {
        Status::Off       => ICON_OFF_BYTES,
        Status::Ready     => ICON_READY_BYTES,
        Status::Connected => ICON_CONNECTED_BYTES,
    };
    // tray-icon wants raw RGBA + width/height. Decode the (embedded) .ico via
    // `image`. Failure is effectively impossible (compile-time asset), but this runs
    // on every status change, so never panic the whole tray over it — fall back to a
    // 1x1 transparent icon (trivially valid).
    let icon = image::load_from_memory(bytes).ok().and_then(|img| {
        let img = img.to_rgba8();
        let (w, h) = img.dimensions();
        Icon::from_rgba(img.into_raw(), w, h).ok()
    });
    icon.unwrap_or_else(|| {
        Icon::from_rgba(vec![0, 0, 0, 0], 1, 1).expect("1x1 transparent icon is always valid")
    })
}

/// One enum the tao event loop carries — tray/menu events + engine status.
#[derive(Debug, Clone)]
enum AppEvent {
    StatusChanged(Status), // engine connect/disconnect -> icon (green/ready/off)
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    ConfigChanged, // config.json was edited externally; reload + restart engine
    UpdateChecked(UpdateOutcome, bool), // result of an update check; bool = user-initiated
    #[cfg(target_os = "macos")]
    MirrorAspectChanged, // iPhone rotated mid-stream -> re-fit the mirror window
    #[cfg(target_os = "macos")]
    EngineStopped, // off-main engine teardown finished -> finish the (re)start
}

/// Result of an update check, marshalled back to the event loop so the prompt
/// + the actual quit-and-swap happen on the main thread (which owns the engine).
#[derive(Debug, Clone)]
enum UpdateOutcome {
    Available(update::Manifest),
    UpToDate,
    Failed,
}

/// Menu ids — used to dispatch from MenuEvent.
struct MenuIds {
    start_stop: tray_icon::menu::MenuId,
    restart: tray_icon::menu::MenuId,
    always_on_top: tray_icon::menu::MenuId,
    settings: tray_icon::menu::MenuId,
    open_logs: tray_icon::menu::MenuId,
    check_updates: tray_icon::menu::MenuId,
    about: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

fn status_word(lang: i18n::Lang, status: Status) -> &'static str {
    let t = i18n::s(lang);
    match status {
        Status::Off => t.status_off,
        Status::Ready => t.status_ready,
        Status::Connected => t.status_connected,
    }
}

fn build_menu(running: bool, status: Status, topmost: bool, lang: i18n::Lang) -> (Menu, MenuIds) {
    let t = i18n::s(lang);
    let menu = Menu::new();
    let status_item = MenuItem::new(format!("● {}", status_word(lang, status)), false, None);
    let start_stop_item = MenuItem::new(if running { t.stop } else { t.start }, true, None);
    // Restart is only meaningful while the engine is running -- grey it out when
    // we are stopped so the menu mirrors the actual valid action.
    let restart_item = MenuItem::new(t.restart, running, None);
    let always_on_top_item = CheckMenuItem::new(t.always_on_top, true, topmost, None);
    let settings_item = MenuItem::new(t.settings, true, None);
    let open_logs_item = MenuItem::new(t.open_logs, true, None);
    let check_updates_item = MenuItem::new(t.check_updates, true, None);
    let about_item = MenuItem::new(t.about, true, None);
    let quit_item = MenuItem::new(t.quit, true, None);

    let ids = MenuIds {
        start_stop: start_stop_item.id().clone(),
        restart: restart_item.id().clone(),
        always_on_top: always_on_top_item.id().clone(),
        settings: settings_item.id().clone(),
        open_logs: open_logs_item.id().clone(),
        check_updates: check_updates_item.id().clone(),
        about: about_item.id().clone(),
        quit: quit_item.id().clone(),
    };

    menu.append(&status_item).ok();
    menu.append(&PredefinedMenuItem::separator()).ok();
    menu.append(&start_stop_item).ok();
    menu.append(&restart_item).ok();
    menu.append(&always_on_top_item).ok();
    menu.append(&PredefinedMenuItem::separator()).ok();
    menu.append(&settings_item).ok();
    menu.append(&open_logs_item).ok();
    menu.append(&check_updates_item).ok();
    menu.append(&about_item).ok();
    menu.append(&PredefinedMenuItem::separator()).ok();
    menu.append(&quit_item).ok();
    (menu, ids)
}

fn open_about_window(children: &Arc<Mutex<Vec<std::process::Child>>>) -> Result<()> {
    // Re-launch ourselves with `--about` so eframe can own its event loop.
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(&exe).arg("--about").spawn()?;
    children.lock().unwrap_or_else(|e| e.into_inner()).push(child);
    prune_dead_children(children);
    Ok(())
}

/// Drop already-exited child entries from the vec so it doesn't grow forever
/// across many Settings/About open/close cycles.
fn prune_dead_children(children: &Arc<Mutex<Vec<std::process::Child>>>) {
    let mut v = children.lock().unwrap_or_else(|e| e.into_inner());
    v.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
}

/// Quit-time helper: kill every sub-window process we have a handle for.
fn kill_all_children(children: &Arc<Mutex<Vec<std::process::Child>>>) {
    let mut v = children.lock().unwrap_or_else(|e| e.into_inner());
    for c in v.iter_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    v.clear();
}

/// Per-monitor DPI awareness so `GetSystemMetrics` and related window-coord
/// queries return raw pixel counts on every display. (Windows-only; other
/// platforms scale via their own toolkit.)
#[cfg(windows)]
fn enable_per_monitor_dpi() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
#[cfg(not(windows))]
fn enable_per_monitor_dpi() {}

/// Named-mutex single-instance for the main tray process.
#[cfg(windows)]
fn acquire_tray_single_instance() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    let h = unsafe { CreateMutexW(None, false, w!("PopyachsaAirPlay.Tray.SingleInstance")) };
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already { return false; }
    // Intentional leak so the mutex stays alive for the process's lifetime.
    std::mem::forget(h);
    true
}
#[cfg(not(windows))]
fn acquire_tray_single_instance() -> bool {
    // TODO(L2): lockfile-based single-instance (e.g. flock on $XDG_RUNTIME_DIR).
    true
}

fn open_settings_window(children: &Arc<Mutex<Vec<std::process::Child>>>) -> Result<()> {
    // Re-launch ourselves with `--settings` in a separate process; that child
    // owns the eframe event loop, edits the config, writes it back, exits.
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(&exe).arg("--settings").spawn()?;
    children.lock().unwrap_or_else(|e| e.into_inner()).push(child);
    prune_dead_children(children);
    Ok(())
}

/// Run an update check on a worker thread; report the result back to the event
/// loop (which owns the engine and does the quit-and-swap). `user_initiated`
/// controls whether "up to date" / "failed" are surfaced — auto-checks stay
/// silent unless they actually find an update.
fn spawn_update_check(proxy: tao::event_loop::EventLoopProxy<AppEvent>, user_initiated: bool) {
    std::thread::spawn(move || {
        let outcome = match update::check_for_update() {
            Ok(Some(m)) => UpdateOutcome::Available(m),
            Ok(None) => UpdateOutcome::UpToDate,
            Err(e) => {
                eprintln!("[update] check failed: {e}");
                UpdateOutcome::Failed
            }
        };
        let _ = proxy.send_event(AppEvent::UpdateChecked(outcome, user_initiated));
    });
}

/// Whether the in-app updater applies to this install. Windows: always. Linux/
/// macOS: only a self-updatable AppImage — a distro package (.deb/.rpm) or a
/// Flatpak is owned by its package manager, so we never ping the feed or offer an
/// in-app install there (the user runs apt/dnf/pacman/flatpak instead).
#[cfg(windows)]
fn update_check_supported() -> bool { true }
#[cfg(not(windows))]
fn update_check_supported() -> bool { update_linux::appimage_path().is_some() }

/// Spawn `updater.exe` (sibling of our exe) to download + verify + swap in the
/// new build. It waits for our PID to exit before touching files, so the caller
/// must shut the app down right after this returns Ok. Windows-only — Linux/macOS
/// self-update in-process via `update_linux`.
#[cfg(windows)]
fn launch_updater(m: &update::Manifest) -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("exe has no parent directory"))?;
    let mut cmd = std::process::Command::new(dir.join("updater.exe"));
    cmd.arg("--url").arg(&m.url)
        .arg("--sha256").arg(&m.sha256)
        .arg("--dir").arg(&dir)
        .arg("--relaunch").arg(&exe)
        .arg("--wait-pid").arg(std::process::id().to_string());
    if !m.mirror_url.trim().is_empty() {
        cmd.arg("--mirror-url").arg(m.mirror_url.trim());
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(windows)]
fn msgbox_yesno(text: &str, title: &str) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONQUESTION, MB_TOPMOST, MB_YESNO,
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_YESNO | MB_ICONQUESTION | MB_TOPMOST,
        ) == IDYES
    }
}

#[cfg(windows)]
fn msgbox_info(text: &str, title: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_TOPMOST,
    };
    unsafe {
        let _ = MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONINFORMATION | MB_TOPMOST,
        );
    }
}

// Non-Windows has no native MessageBox: the update flow there reports results
// via `update_linux::notify` (libnotify toast) instead of a modal prompt, so no
// msgbox stubs are needed.

/// Redirect this process's stdout + stderr to `logs/engine.log` (next to the
/// exe). The engine's native output — UxPlay, the dnssd shim, GStreamer warnings
/// — goes to stderr/stdout, NOT the log callback, so this is the only way to
/// capture the startup/mDNS/decoder lines a tester needs when it "doesn't work".
/// stderr is unbuffered (C convention) so lines land promptly.
#[cfg(windows)]
fn redirect_stdio_to_log() {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    // The engine (uxplay-core.dll, MinGW/UCRT) logs via the C runtime FILE*
    // stderr/stdout. The app (MSVC) and the DLL share ucrtbase.dll, so
    // `_wfreopen` on the shared stderr re-points it for BOTH — unlike SetStdHandle
    // or _dup2, which don't update a FILE*'s cached handle. We then point stdout
    // at stderr's fd, and Rust's own Win32-handle stderr/stdout at the same file.
    extern "C" {
        fn _wfreopen(path: *const u16, mode: *const u16, stream: *mut core::ffi::c_void)
            -> *mut core::ffi::c_void;
        fn __acrt_iob_func(idx: u32) -> *mut core::ffi::c_void;
        fn _dup2(fd1: i32, fd2: i32) -> i32;
        fn _fileno(stream: *mut core::ffi::c_void) -> i32;
        fn _get_osfhandle(fd: i32) -> isize;
    }

    let dir = engine::engine_log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path: Vec<u16> = dir
        .join("engine.log")
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mode: [u16; 2] = [b'w' as u16, 0];
    unsafe {
        let se = __acrt_iob_func(2); // stderr FILE*
        if _wfreopen(path.as_ptr(), mode.as_ptr(), se).is_null() {
            return;
        }
        // stdout (FILE*) shares stderr's fd → same file, single offset.
        _dup2(_fileno(se), _fileno(__acrt_iob_func(1)));
        // Rust's eprintln!/println! go through the Win32 std handles; point them
        // at the same handle freopen just opened.
        let oh = _get_osfhandle(_fileno(se));
        if oh != -1 {
            let h = HANDLE(oh as *mut core::ffi::c_void);
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
        }
    }
}

#[cfg(not(windows))]
fn redirect_stdio_to_log() {
    use std::os::unix::io::AsRawFd;
    // engine_log_dir() is a user-writable XDG path on Linux/macOS (NOT next to the
    // exe — that's read-only on a system/Flatpak install). Point this process's
    // stdout(1) + stderr(2) at logs/engine.log; the dlopen'd engine shares these
    // fds, so its UxPlay / GStreamer / dnssd output is captured here too.
    let dir = engine::engine_log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = match std::fs::OpenOptions::new()
        .create(true).write(true).truncate(true)
        .open(dir.join("engine.log"))
    {
        Ok(f) => f,
        Err(e) => { eprintln!("[log] {}: {e}", dir.display()); return; }
    };
    let fd = file.as_raw_fd();
    unsafe {
        // dup2 both onto the file's open description -> shared offset, no clobber.
        libc::dup2(fd, libc::STDOUT_FILENO);
        libc::dup2(fd, libc::STDERR_FILENO);
    }
    // The dup'd fds reference the file; keep it open for the process lifetime.
    std::mem::forget(file);
}

fn main() -> Result<()> {
    // X11 multithreading must be initialized before ANY Xlib call in the process
    // (GTK/tray and our engine worker thread both touch X), so do it FIRST — before
    // the tray, the engine, or any window exists.
    #[cfg(target_os = "linux")]
    unsafe { x11::xlib::XInitThreads(); }

    // Pre-set AVAHI_COMPAT_NOWARN so the bundled UxPlay engine SKIPS its own
    // `putenv("AVAHI_COMPAT_NOWARN=1")` (uxplay.cpp): that putenv stores a pointer
    // into uxplay-core.so's STATIC data into `environ` — and we dlclose
    // uxplay-core.so on every engine stop/restart, which unmaps that memory and
    // leaves a DANGLING `environ` entry. The next getenv() (the restarted engine's
    // X worker calling XOpenDisplay, or a forked Settings/About child that
    // inherited the corrupted environ) then walks into freed memory and SIGSEGVs.
    // It's layout/glibc-dependent (survives by luck on the build distro, fatal on
    // newer glibc). set_var routes through libc setenv, which COPIES into
    // libc-owned memory that outlives any dlclose, and the non-null getenv makes
    // the engine skip its putenv entirely. Must run before the engine ever loads.
    #[cfg(target_os = "linux")]
    std::env::set_var("AVAHI_COMPAT_NOWARN", "1");

    enable_per_monitor_dpi();

    // Sub-windows spawned from the tray each own their own event loop.
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--settings" || a == "--about") {
        if !acquire_tray_single_instance() {
            eprintln!("[popyachsa-airplay] another tray instance is already running");
            return Ok(());
        }
    }
    if args.iter().any(|a| a == "--settings") {
        std::fs::create_dir_all(data_dir()).ok();
        let cfg = Config::load();
        if let Err(e) = settings_ui::run(cfg) {
            eprintln!("[settings_ui] {e}");
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--about") {
        std::fs::create_dir_all(data_dir()).ok();
        if let Err(e) = about_ui::run() {
            eprintln!("[about_ui] {e}");
        }
        return Ok(());
    }

    // Capture engine output (UxPlay / dnssd shim / GStreamer) to logs/engine.log
    // for troubleshooting. Tray-only — sub-windows must not truncate it.
    redirect_stdio_to_log();

    // One-shot migration from the pre-rename folder %APPDATA%\PopyachsaTV.
    {
        let new_dir = data_dir();
        let old_dir = config::legacy_data_dir();
        if !new_dir.exists() && old_dir.exists() {
            if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
                eprintln!("[migrate] failed to move {} -> {}: {e}",
                          old_dir.display(), new_dir.display());
            } else {
                eprintln!("[migrate] moved {} -> {}",
                          old_dir.display(), new_dir.display());
            }
        }
    }

    std::fs::create_dir_all(data_dir()).ok();
    // (Engine log lives next to the exe in logs/engine.log — see engine_log_dir
    // + redirect_stdio_to_log. %APPDATA%\…\logs is no longer used.)

    // Persist the GStreamer plugin registry to a writable per-user path. Without
    // this the 241-plugin scan reruns on every launch and the AirPlay service
    // only starts advertising *after* it finishes (gst_init precedes the mDNS
    // registration inside uxplay) — so the receiver is invisible to iPhones for
    // the first few seconds after each launch. Pinning GST_REGISTRY means the
    // scan happens once; later launches advertise immediately. GStreamer reads
    // this via GetEnvironmentVariable, which sees our SetEnvironmentVariableW.
    std::env::set_var("GST_REGISTRY", data_dir().join("gstreamer-registry.bin"));

    if !config::config_path().exists() {
        let _ = Config::default().save();
    }

    let cfg = Arc::new(Mutex::new(Config::load()));

    // Keep the autostart registry entry in sync with config on startup.
    autostart::sync(cfg.lock().unwrap_or_else(|e| e.into_inner()).autostart_with_windows);

    // The single in-process engine, shared between event-loop callbacks.
    let engine = Arc::new(Engine::new());

    // Child handles for the spawned Settings / About sub-windows.
    let sub_windows: Arc<Mutex<Vec<std::process::Child>>> = Arc::new(Mutex::new(Vec::new()));

    // Cross-thread channel for forwarded muda/tray/config events.
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let stop_flag = Arc::new(Mutex::new(false));

    // Always-on-top state: menu toggle + engine.set_topmost. No more log/focus
    // watchers — the engine owns its window in-process.
    let always_on_top = Arc::new(AtomicBool::new(cfg.lock().unwrap_or_else(|e| e.into_inner()).always_on_top));

    // Watch config.json on disk: when the user edits/saves it, reload + restart.
    {
        let tx2 = tx.clone();
        let stop = stop_flag.clone();
        std::thread::spawn(move || {
            let path = config::config_path();
            let mut last_mtime: Option<std::time::SystemTime> = std::fs::metadata(&path)
                .and_then(|m| m.modified()).ok();
            loop {
                if *stop.lock().unwrap_or_else(|e| e.into_inner()) { return; }
                std::thread::sleep(std::time::Duration::from_millis(800));
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(t) = meta.modified() {
                        if Some(t) != last_mtime {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            last_mtime = Some(t);
                            let _ = tx2.send(AppEvent::ConfigChanged);
                        }
                    }
                }
            }
        });
    }

    // tao event loop with our custom UserEvent.
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Forward muda + tray-icon events into the tao loop.
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |ev| { let _ = p.send_event(AppEvent::Menu(ev)); }));
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |ev| { let _ = p.send_event(AppEvent::Tray(ev)); }));
    }

    // Forward channel events (config changes) into the loop.
    {
        let p = proxy.clone();
        std::thread::spawn(move || {
            for ev in rx { if p.send_event(ev).is_err() { return; } }
        });
    }

    // Engine -> tray status bridge: the engine sends Status on device
    // connect/disconnect (it shows/hides its own window in lockstep).
    {
        let (status_tx, status_rx) = mpsc::channel::<Status>();
        crate::engine::install_status_sender(status_tx);
        let p = proxy.clone();
        std::thread::spawn(move || {
            for s in status_rx {
                if p.send_event(AppEvent::StatusChanged(s)).is_err() {
                    return;
                }
            }
        });
    }

    // macOS: worker -> main re-fit bridge — the engine signals on a mid-stream
    // aspect change (iPhone rotation) so we resize the mirror window to match.
    #[cfg(target_os = "macos")]
    {
        let (refit_tx, refit_rx) = mpsc::channel::<()>();
        crate::engine::install_refit_sender(refit_tx);
        let p = proxy.clone();
        std::thread::spawn(move || {
            for _ in refit_rx {
                if p.send_event(AppEvent::MirrorAspectChanged).is_err() {
                    return;
                }
            }
        });
    }

    // macOS: off-main engine-teardown bridge — the throwaway join thread pings here
    // when stop()/restart() finishes, so the (re)start resumes on the main thread
    // without the run loop ever blocking on the worker join.
    #[cfg(target_os = "macos")]
    {
        let (life_tx, life_rx) = mpsc::channel::<()>();
        crate::engine::install_lifecycle_sender(life_tx);
        let p = proxy.clone();
        std::thread::spawn(move || {
            for _ in life_rx {
                if p.send_event(AppEvent::EngineStopped).is_err() {
                    return;
                }
            }
        });
    }

    // Now that the status bridge is live, optionally start the engine (it will
    // advertise; its window stays hidden until a device connects).
    // macOS: deferred to StartCause::Init — the mirror window (and thus the
    // engine's host NSView) only exists once the event loop is running.
    #[cfg(not(target_os = "macos"))]
    if cfg.lock().unwrap_or_else(|e| e.into_inner()).autostart_on_app_launch {
        let c = cfg.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Err(e) = engine.start(&c) {
            eprintln!("[engine] autostart failed: {e}");
        }
    }

    // Quiet auto-check for a newer signed build (config-gated). Windows prompts
    // (modal) if it finds one; AppImage/macOS apply via their per-OS module.
    // Skipped on installs the package manager owns (no feed ping) — see
    // update_check_supported().
    if cfg.lock().unwrap_or_else(|e| e.into_inner()).check_updates_on_launch && update_check_supported() {
        spawn_update_check(proxy.clone(), false);
    }

    let mut current_status = Status::Off;
    let mut current_lang = i18n::Lang::from_config(&cfg.lock().unwrap_or_else(|e| e.into_inner()).language);
    let mut tray_icon: Option<tray_icon::TrayIcon> = None;
    let mut ids: Option<MenuIds> = None;

    event_loop.run(move |event, event_target, control_flow| {
        *control_flow = ControlFlow::Wait;
        // event_target is only used on macOS (to create the mirror tao window).
        #[cfg(not(target_os = "macos"))]
        let _ = event_target;

        match event {
            Event::NewEvents(StartCause::Init) => {
                eprintln!("[popyachsa-airplay] event loop init -- creating tray icon");
                // macOS: create the mirror window now (loop is running -> the
                // worker's dispatch_sync(main) overlay bind is safe), then honour
                // autostart (deferred from before the loop: the NSView didn't
                // exist yet). Done before build_menu so it reflects running state.
                #[cfg(target_os = "macos")]
                {
                    if let Err(e) = engine.attach_window(event_target) {
                        eprintln!("[engine-macos] attach_window failed: {e}");
                    } else if cfg.lock().unwrap().autostart_on_app_launch {
                        let c = cfg.lock().unwrap().clone();
                        if let Err(e) = engine.start(&c) {
                            eprintln!("[engine-macos] autostart failed: {e}");
                        }
                    }
                }
                current_status = if engine.is_running() { Status::Ready } else { Status::Off };
                let (menu, new_ids) = build_menu(engine.is_running(), current_status,
                                                 always_on_top.load(Ordering::Relaxed), current_lang);
                match TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip(format!("{APP_NAME} — {}", status_word(current_lang, current_status)))
                    .with_icon(icon_for(current_status))
                    .build()
                {
                    Ok(t) => {
                        eprintln!("[popyachsa-airplay] tray icon registered");
                        tray_icon = Some(t);
                        ids = Some(new_ids);
                    }
                    Err(e) => {
                        eprintln!("[popyachsa-airplay] FAILED to build tray icon: {e}");
                    }
                }
            }
            Event::UserEvent(app_ev) => {
                let (Some(tray), Some(ids_ref)) = (tray_icon.as_ref(), ids.as_mut()) else {
                    return;
                };
                match app_ev {
                    AppEvent::StatusChanged(s) => {
                        current_status = s;
                        // macOS: show the mirror window while a device streams,
                        // hide it otherwise (engine reports over this channel).
                        #[cfg(target_os = "macos")]
                        engine.set_mirror_visible(s == Status::Connected);
                        let (m, new_ids) = build_menu(engine.is_running(), current_status,
                                                      always_on_top.load(Ordering::Relaxed), current_lang);
                        *ids_ref = new_ids;
                        tray.set_menu(Some(Box::new(m)));
                        let _ = tray.set_tooltip(Some(format!("{APP_NAME} — {}", status_word(current_lang, current_status))));
                        let _ = tray.set_icon(Some(icon_for(current_status)));
                    }
                    #[cfg(target_os = "macos")]
                    AppEvent::MirrorAspectChanged => {
                        // iPhone rotated mid-stream: re-fit the window aspect so the
                        // avlayer fills it with no black bars (no tray change).
                        engine.refit_mirror();
                    }
                    #[cfg(target_os = "macos")]
                    AppEvent::EngineStopped => {
                        // Off-main engine teardown finished: finish the pending
                        // (re)start, then refresh the tray to the resulting state.
                        engine.on_engine_stopped();
                        current_status = if engine.is_running() { Status::Ready } else { Status::Off };
                        let (m, new_ids) = build_menu(engine.is_running(), current_status,
                                                      always_on_top.load(Ordering::Relaxed), current_lang);
                        *ids_ref = new_ids;
                        tray.set_menu(Some(Box::new(m)));
                        let _ = tray.set_icon(Some(icon_for(current_status)));
                        let _ = tray.set_tooltip(Some(format!("{APP_NAME} — {}",
                            status_word(current_lang, current_status))));
                    }
                    AppEvent::Menu(ev) => {
                        let id = ev.id();
                        if id == &ids_ref.start_stop {
                            if engine.is_running() {
                                engine.stop();
                                current_status = Status::Off;
                            } else {
                                let new_cfg = Config::load();
                                *cfg.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.clone();
                                if let Err(e) = engine.start(&new_cfg) {
                                    eprintln!("[engine] start: {e}");
                                } else {
                                    current_status = Status::Ready;
                                }
                            }
                            let (m, new_ids) = build_menu(engine.is_running(), current_status,
                                                      always_on_top.load(Ordering::Relaxed), current_lang);
                            *ids_ref = new_ids;
                            tray.set_menu(Some(Box::new(m)));
                            let _ = tray.set_icon(Some(icon_for(current_status)));
                            let _ = tray.set_tooltip(Some(format!("{APP_NAME} — {}", status_word(current_lang, current_status))));
                        } else if id == &ids_ref.always_on_top {
                            let new = !always_on_top.load(Ordering::Relaxed);
                            always_on_top.store(new, Ordering::Relaxed);
                            engine.set_topmost(new);
                            {
                                let mut c = cfg.lock().unwrap_or_else(|e| e.into_inner());
                                c.always_on_top = new;
                                let _ = c.save();
                            }
                            let (m, new_ids) = build_menu(engine.is_running(),
                                                          current_status, new, current_lang);
                            *ids_ref = new_ids;
                            tray.set_menu(Some(Box::new(m)));
                        } else if id == &ids_ref.restart {
                            let new_cfg = Config::load();
                            *cfg.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.clone();
                            if let Err(e) = engine.restart(&new_cfg) {
                                eprintln!("[engine] restart: {e}");
                            }
                            current_status = if engine.is_running() { Status::Ready } else { Status::Off };
                        } else if id == &ids_ref.settings {
                            if let Err(e) = open_settings_window(&sub_windows) {
                                eprintln!("[settings] {e}");
                            }
                        } else if id == &ids_ref.open_logs {
                            // Engine log lives next to the exe (logs/engine.log);
                            // ensure the folder exists so Explorer opens cleanly.
                            let ld = engine::engine_log_dir();
                            let _ = std::fs::create_dir_all(&ld);
                            #[cfg(windows)]
                            let _ = std::process::Command::new("explorer").arg(&ld).spawn();
                            #[cfg(not(windows))]
                            let _ = std::process::Command::new("xdg-open").arg(&ld).spawn();
                        } else if id == &ids_ref.check_updates {
                            // On a package-manager-owned install, don't ping the
                            // feed — point the user at their package manager.
                            if update_check_supported() {
                                spawn_update_check(proxy.clone(), true);
                            } else {
                                #[cfg(not(windows))]
                                update_linux::notify(i18n::s(current_lang).upd_title,
                                    "Updates are managed by your package manager (apt / dnf / pacman / flatpak).");
                            }
                        } else if id == &ids_ref.about {
                            if let Err(e) = open_about_window(&sub_windows) {
                                eprintln!("[about] {e}");
                            }
                        } else if id == &ids_ref.quit {
                            *stop_flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
                            engine.stop_blocking(); // sync teardown before exit
                            kill_all_children(&sub_windows);
                            *control_flow = ControlFlow::Exit;
                            std::process::exit(0);
                        }
                    }
                    AppEvent::Tray(_ev) => { /* left-click could open menu later */ }
                    AppEvent::ConfigChanged => {
                        eprintln!("[popyachsa-airplay] config.json changed -- reloading");
                        let new_cfg = Config::load();
                        autostart::sync(new_cfg.autostart_with_windows);
                        // Live-applicable settings apply WITHOUT a restart.
                        always_on_top.store(new_cfg.always_on_top, Ordering::Relaxed);
                        engine.set_topmost(new_cfg.always_on_top);
                        // Only restart the engine when a setting that actually
                        // needs it changed — NOT for live toggles like
                        // always-on-top (whose config save also trips this
                        // watcher and would otherwise restart mid-stream).
                        let needs_restart = {
                            let old = cfg.lock().unwrap_or_else(|e| e.into_inner());
                            old.device_name != new_cfg.device_name
                                || old.target_fps != new_cfg.target_fps
                                || old.enable_h265 != new_cfg.enable_h265
                                || old.video_decoder != new_cfg.video_decoder
                                || old.audio_sink != new_cfg.audio_sink
                                || old.debug_logging != new_cfg.debug_logging
                                || old.custom_flags != new_cfg.custom_flags
                        };
                        *cfg.lock().unwrap_or_else(|e| e.into_inner()) = new_cfg.clone();
                        if needs_restart && engine.is_running() {
                            if let Err(e) = engine.restart(&new_cfg) {
                                eprintln!("[engine] restart on config change: {e}");
                            }
                        }
                        // Language may have changed in Settings — re-resolve.
                        current_lang = i18n::Lang::from_config(&new_cfg.language);
                        // Rebuild the tray menu so its "Always on top" checkmark,
                        // Start/Stop and language reflect changes made in Settings.
                        let (m, new_ids) = build_menu(engine.is_running(), current_status,
                                                      new_cfg.always_on_top, current_lang);
                        *ids_ref = new_ids;
                        tray.set_menu(Some(Box::new(m)));
                    }
                    AppEvent::UpdateChecked(outcome, user_initiated) => {
                        let t = i18n::s(current_lang);
                        match outcome {
                            UpdateOutcome::Available(m) => {
                                // Windows: modal prompt, then hand off to updater.exe.
                                #[cfg(windows)]
                                {
                                    let notes = if m.notes.trim().is_empty() {
                                        String::new()
                                    } else {
                                        format!("\n\n{}", m.notes.trim())
                                    };
                                    let text = format!("{} v{}{}\n\n{}",
                                                       t.upd_available, m.version, notes, t.upd_install);
                                    if msgbox_yesno(&text, t.upd_title) {
                                        match launch_updater(&m) {
                                            Ok(()) => {
                                                *stop_flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
                                                engine.stop_blocking(); // sync teardown before exit
                                                kill_all_children(&sub_windows);
                                                *control_flow = ControlFlow::Exit;
                                                std::process::exit(0);
                                            }
                                            Err(e) => {
                                                eprintln!("[update] launch updater: {e}");
                                                msgbox_info(t.upd_failed, t.upd_title);
                                            }
                                        }
                                    }
                                }
                                // Linux/macOS: no modal — clicking the menu item is the
                                // consent. Download the signed AppImage, swap it over the
                                // running file in place, relaunch.
                                #[cfg(not(windows))]
                                {
                                    if !user_initiated {
                                        // Auto-check on launch: non-intrusive nudge only —
                                        // point at the tray item; install on the user's terms.
                                        update_linux::notify(t.upd_title,
                                            &format!("{} v{} — {}", t.upd_available, m.version, t.check_updates));
                                    } else {
                                        // Manual: clicking the menu item is the consent. Delta-
                                        // update (full-download fallback), verify, relaunch.
                                        update_linux::notify(t.upd_title,
                                            &format!("{} v{}", t.upd_available, m.version));
                                        match update_linux::apply(&m) {
                                            Ok(path) => {
                                                *stop_flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
                                                engine.stop_blocking(); // sync teardown before exit
                                                kill_all_children(&sub_windows);
                                                update_linux::relaunch_after_exit(&path);
                                                *control_flow = ControlFlow::Exit;
                                                std::process::exit(0);
                                            }
                                            Err(e) => {
                                                eprintln!("[update] apply: {e}");
                                                update_linux::notify(t.upd_title, t.upd_failed);
                                            }
                                        }
                                    }
                                }
                            }
                            UpdateOutcome::UpToDate => {
                                if user_initiated {
                                    #[cfg(windows)]
                                    msgbox_info(t.upd_uptodate, t.upd_title);
                                    #[cfg(not(windows))]
                                    update_linux::notify(t.upd_title, t.upd_uptodate);
                                }
                            }
                            UpdateOutcome::Failed => {
                                if user_initiated {
                                    #[cfg(windows)]
                                    msgbox_info(t.upd_failed, t.upd_title);
                                    #[cfg(not(windows))]
                                    update_linux::notify(t.upd_title, t.upd_failed);
                                }
                            }
                        }
                    }
                }
            }
            // X on the mirror window: INTERRUPT the current connection but keep the
            // receiver running + advertising (wait for a new connection) — NOT a
            // full Stop. engine.restart() drops the current client and re-advertises;
            // the window hides and re-shows on the next "Begin streaming". (We never
            // DROP the window while the sink renders into its NSView -> restart's
            // stop() releases the sink first.) The tray stays in the running state.
            #[cfg(target_os = "macos")]
            Event::WindowEvent { event: tao::event::WindowEvent::CloseRequested, .. } => {
                let c = cfg.lock().unwrap().clone();
                if let Err(e) = engine.restart(&c) {
                    eprintln!("[engine-macos] restart on window close: {e}");
                }
                current_status = if engine.is_running() { Status::Ready } else { Status::Off };
                if let (Some(tray), Some(ids_ref)) = (tray_icon.as_ref(), ids.as_mut()) {
                    let (m, new_ids) = build_menu(engine.is_running(), current_status,
                        always_on_top.load(Ordering::Relaxed), current_lang);
                    *ids_ref = new_ids;
                    tray.set_menu(Some(Box::new(m)));
                    let _ = tray.set_icon(Some(icon_for(current_status)));
                    let _ = tray.set_tooltip(Some(format!("{APP_NAME} — {}",
                        status_word(current_lang, current_status))));
                }
            }
            // Mirror window resized (incl. returning from fullscreen): apply a
            // rotation that happened while we were fullscreen, now we're windowed.
            #[cfg(target_os = "macos")]
            Event::WindowEvent { event: tao::event::WindowEvent::Resized(_), .. } => {
                engine.on_window_resized();
            }
            _ => {}
        }
    });
}
