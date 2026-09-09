//! macOS in-process AirPlay engine (M3 full integration).
//!
//! Unlike the Windows engine (`engine.rs`, a thread-per-Win32-window model), on
//! macOS the mirror window is a **second `tao::Window` in the app's single main
//! EventLoop** — because GStreamer requires a live `NSApplication` run loop on
//! the main thread, and `tao` (not `gst_macos_main`) owns it (validated by the M3
//! gate). So this engine:
//!   * is `attach_window()`-ed once from `StartCause::Init` (loop running), which
//!     creates a hidden mirror window and extracts its `NSView*`;
//!   * loads `uxplay-core.dylib` and drives it on its own worker thread
//!     (`airplay_core_start`); the worker's overlay bind marshals onto the main
//!     queue (video_renderer.c), which is why the engine is only ever started
//!     AFTER the loop is up;
//!   * reports device connect/disconnect via UxPlay log markers over the existing
//!     `Status` channel (`install_status_sender`), and `main.rs` shows/hides +
//!     aspect-fits the window from the main thread in response.
//!
//! Single-threaded by design: `Engine` is only ever touched from the main
//! (event-loop) thread, so it uses `RefCell` and is intentionally `!Send`.

use std::cell::RefCell;
use std::ffi::{c_void, CStr};
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use airplay_lib::AirPlay;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Fullscreen, Window, WindowBuilder};

use crate::config::Config;
use crate::status::Status;
use crate::AppEvent;

// UxPlay log markers. With macOS always-reinit (uxplay.cpp), "Begin streaming"
// reliably fires on every connect / reconnect / engine restart, so it's the
// window-show trigger. "begin video stream wxh" only captures the aspect (it was
// unreliable across an engine restart). BOTH are LOGGER_INFO in the fork
// (renderers/video_renderer.c) and must stay that way: the host reads them
// through the log callback with debug logging OFF, which is the default, so
// demoting either to DEBUG silently breaks show-on-connect and aspect-fit.
const MARK_CONNECTED: &str = "Begin streaming";
const MARK_TEARDOWN: &str = "Open connections: 0";

// Shared with the C log callback (engine is single-instance).
static STATUS_TX: Mutex<Option<Sender<Status>>> = Mutex::new(None);
static CONNECTED: AtomicBool = AtomicBool::new(false);
// Current video frame size (w<<32 | h), 0 = unknown — parsed from the engine's
// "begin video stream wxh = WxH" log line; used to fit the window to the content
// aspect (no black bars).
static ASPECT_WH: AtomicU64 = AtomicU64::new(0);

/// Main installs a sender so connection transitions reach the tray icon.
pub fn install_status_sender(tx: Sender<Status>) {
    *STATUS_TX.lock().unwrap() = Some(tx);
}

fn send_status(s: Status) {
    if let Some(tx) = STATUS_TX.lock().unwrap().as_ref() {
        let _ = tx.send(s);
    }
}

// Worker->main "re-fit the mirror window" signal, fired when the content aspect
// changes mid-stream (iPhone rotation). Separate from STATUS_TX so a rotation
// doesn't churn the tray icon/menu or steal focus the way a Status would.
static REFIT_TX: Mutex<Option<Sender<()>>> = Mutex::new(None);

/// Main installs a sender so mid-stream rotations trigger a window re-fit.
pub fn install_refit_sender(tx: Sender<()>) {
    *REFIT_TX.lock().unwrap() = Some(tx);
}

fn send_refit() {
    if let Some(tx) = REFIT_TX.lock().unwrap().as_ref() {
        let _ = tx.send(());
    }
}

// Off-main engine teardown completion: the throwaway join thread (see Engine::begin)
// pings this so the main thread can finish a (re)start without ever blocking on the
// worker join itself.
static LIFECYCLE_TX: Mutex<Option<Sender<()>>> = Mutex::new(None);

/// Main installs a sender so an off-main engine teardown can resume on the main thread.
pub fn install_lifecycle_sender(tx: Sender<()>) {
    *LIFECYCLE_TX.lock().unwrap() = Some(tx);
}

fn signal_engine_stopped() {
    if let Some(tx) = LIFECYCLE_TX.lock().unwrap().as_ref() {
        let _ = tx.send(());
    }
}

/// Engine log directory: a user-writable XDG path (`~/Library/Application
/// Support/PopyachsaAirPlay/logs`), NOT next to the exe — a packaged `.app` is
/// read-only and codesigned. Matches `engine_linux` so `redirect_stdio_to_log`
/// (main.rs) can capture the engine's stdout/stderr to a writable location.
pub fn engine_log_dir() -> PathBuf {
    crate::config::log_dir()
}

/// Resolve `uxplay-core.dylib`: next to the exe (packaged: Contents/MacOS or
/// ../Frameworks), else the local dev build tree (cargo run).
fn core_dylib_path() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [
                dir.join("uxplay-core.dylib"),
                dir.join("../Frameworks/uxplay-core.dylib"),
            ] {
                if cand.exists() {
                    return cand.to_string_lossy().into_owned();
                }
            }
        }
    }
    // Dev fallback: the M1 build tree.
    let dev = dirs::home_dir()
        .map(|h| h.join("uxplay-mac-build/UxPlay/build-arm64/uxplay-core.dylib"))
        .unwrap_or_else(|| PathBuf::from("uxplay-core.dylib"));
    dev.to_string_lossy().into_owned()
}

/// When running from a packaged `.app`, point GStreamer at the BUNDLED plugins +
/// a writable registry, so the receiver is fully self-contained (needs no system
/// GStreamer.framework). No-op for `cargo run` (the bundle dir won't exist). Must
/// run before the dylib's `gst_init`, i.e. before `ap.start()`.
fn set_bundled_gst_env() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(macos) = exe.parent() else { return }; // …/Foo.app/Contents/MacOS
    // The tree itself always lives in Contents/Resources/GStreamer; whether a
    // symlink to it exists at Contents/Frameworks/GStreamer depends on the
    // release (make-app.sh's GST_SYMLINK — the symlink cannot ship in the same
    // release that first teaches the updater to recreate symlinks). So PROBE
    // both rather than hard-coding one: this is
    // what lets an app and a bundle from different releases work together in
    // either direction, which is exactly the pairing that has bitten this
    // project repeatedly.
    let Some(gst) = ["../Resources/GStreamer", "../Frameworks/GStreamer"]
        .iter()
        .map(|rel| macos.join(rel))
        .find(|p| p.join("lib/gstreamer-1.0").is_dir())
    else {
        return; // not a bundled run
    };
    let plugins = gst.join("lib/gstreamer-1.0");
    std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
    std::env::set_var("GST_PLUGIN_PATH_1_0", &plugins);
    let scanner = gst.join("libexec/gstreamer-1.0/gst-plugin-scanner");
    if scanner.exists() {
        std::env::set_var("GST_PLUGIN_SCANNER_1_0", &scanner);
    }
    // The .app is read-only / codesigned → the plugin registry must live in a
    // user-writable location, not inside the bundle.
    let reg = crate::config::data_dir().join("gstreamer-registry.bin");
    std::env::set_var("GST_REGISTRY_1_0", &reg);
}

/// Build the UxPlay option tail for macOS (device name goes via set_device_name).
/// glimagesink renders into our NSView; VideoToolbox decode; `-vsync no` avoids
/// macOS timestamp frame-drops; `-nc` is the macOS no-close default.
fn build_options(cfg: &Config) -> String {
    let mut a: Vec<String> = Vec::new();
    if cfg.debug_logging {
        a.push("-d".into());
    }
    a.extend(["-nh", "-nohold", "-nc", "-hls"].iter().map(|s| s.to_string()));
    a.extend(["-fps".into(), cfg.target_fps.to_string()]);
    a.extend(["-vsync".into(), "no".into()]);
    if cfg.enable_h265 {
        a.push("-h265".into());
    }
    // Decoder: VideoToolbox (vtdec — auto HW/SW, preferred over vtdec_hw) by
    // default; honour an explicit "software" choice. Carried-over Windows decoder
    // values (d3d11/d3d12/nvidia) map to VideoToolbox.
    let vdec = match cfg.video_decoder.as_str() {
        "software" | "avdec" => "avdec_h264",
        _ => "vtdec",
    };
    a.extend(["-vd".into(), vdec.to_string()]);
    // "avlayer": our OWN custom sink (renderers/avsample_sink.m) — an appsink pulls
    // decoded NV12 and we feed it to an AVSampleBufferDisplayLayer we host in the
    // NSView. Bypasses GStreamer's broken applemedia sinks entirely: no UAF on the
    // rotation caps-change (avsamplebufferlayersink), no teardown deadlock
    // (osxvideosink). Low latency (display-immediately) + clean resize (the layer
    // scales). glimagesink remains the stable fallback if this regresses.
    a.extend(["-vs".into(), "avlayer".into()]);
    // Audio sink: the config default is a Windows sink (wasapisink) that does NOT
    // exist on macOS — passing it aborts the GStreamer pipeline. Map Windows
    // sinks (and empty) to autoaudiosink (auto-picks osxaudiosink); honour an
    // explicit non-Windows sink the user may have set.
    let asink = match cfg.audio_sink.as_str() {
        "" | "wasapisink" | "directsoundsink" => "autoaudiosink",
        other => other,
    };
    a.extend(["-as".into(), asink.to_string()]);
    a.push("-FPSdata".into());
    // Bind + advertise on ONE adapter (fork-only `-bind`). Omitted when unset so
    // the argv tail stays byte-identical to what shipped before this setting
    // existed. custom_flags stays last, so a hand-typed -bind there still wins.
    if let Some(ip) = crate::net_interfaces::bind_arg(cfg.bind_ip.as_deref()) {
        a.extend(["-bind".into(), ip.to_string()]);
    }
    if !cfg.custom_flags.trim().is_empty() {
        for tok in cfg.custom_flags.split_whitespace() {
            a.push(tok.to_string());
        }
    }
    a.join(" ")
}

/// Parse the leading "WxH" of an UxPlay "begin video stream wxh = WxH; ..." tail.
fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let s = s.trim_start();
    let (w_str, rest) = s.split_once('x')?;
    let w: u32 = w_str.trim().parse().ok()?;
    let h_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let h: u32 = h_str.parse().ok()?;
    if w == 0 || h == 0 {
        None
    } else {
        Some((w, h))
    }
}

/// C log callback (runs on the engine worker thread): translate UxPlay's log
/// markers into `Status` over the channel, and capture the video size for
/// aspect-fit. Window mutations happen on the main thread in `main.rs`.
extern "C" fn engine_log_cb(_level: c_int, msg: *const c_char, _user: *mut c_void) {
    if msg.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();

    // The engine gave up during startup. Nothing else will ever tell us: the C ABI
    // called this run a success the moment the worker spawned, so without this the
    // tray keeps showing "Ready" for a receiver no device can see. Flag it and
    // report Off; the tray consumes the flag, does the real teardown (only the main
    // thread may touch the Engine here) and tells the user.
    if crate::status::is_fatal_start_line(&text) {
        eprintln!("[engine-macos] fatal start error: {text}");
        crate::status::START_FAILED.store(true, Ordering::SeqCst);
        send_status(Status::Off);
        return;
    }

    // The pinned adapter was gone and the engine fell back to every interface. It
    // KEEPS RUNNING, so this changes nothing but the tray's menu text — and the
    // resend is what gets that menu rebuilt, since the tray only rebuilds on a
    // status event and this line lands while the engine is still starting.
    if crate::status::is_pin_ignored_line(&text) {
        if !crate::status::PIN_IGNORED.swap(true, Ordering::SeqCst) {
            eprintln!("[engine-macos] pinned adapter unavailable: {text}");
            send_status(Status::Ready);
        }
        return;
    }

    // Disconnect: device gone (RAOP connections dropped to zero).
    if text.contains(MARK_TEARDOWN) {
        if CONNECTED.swap(false, Ordering::SeqCst) {
            eprintln!("[engine-macos] disconnect -> hide");
            send_status(Status::Ready);
        }
        return;
    }

    // Capture the video size for the aspect-fit. With the custom `avlayer` sink
    // (AVSampleBufferDisplayLayer, which scales cleanly — unlike glimagesink, which
    // corrupted on programmatic resize) we now ALSO re-fit on mid-stream rotation:
    // signal the main thread whenever the aspect actually changes while connected.
    if let Some(rest) = text.split("video stream wxh = ").nth(1) {
        if let Some((w, h)) = parse_wxh(rest) {
            let packed = ((w as u64) << 32) | h as u64;
            let prev = ASPECT_WH.swap(packed, Ordering::SeqCst);
            // Skip the first report (prev==0 — connect-time fit handles it) and
            // identical-size rotations (landscape<->landscape report the same WxH).
            if prev != 0 && prev != packed && CONNECTED.load(Ordering::SeqCst) {
                send_refit();
            }
        }
    }

    // Connect/show: "Begin streaming" fires on every (re)connect AND restart
    // (macOS always re-inits the pipeline). Reliable; the wxh line was not.
    if text.contains(MARK_CONNECTED) {
        if !CONNECTED.swap(true, Ordering::SeqCst) {
            eprintln!("[engine-macos] Begin streaming -> show");
            send_status(Status::Connected);
        }
    }
}

/// Wait on `done` for at most `secs`. Returns whether it was actually signalled
/// (false = the deadline won and the caller should carry on regardless).
///
/// Deliberately a plain sleep-poll and NOT a nested `CFRunLoopRunInMode`, even
/// though pumping the run loop here would let the main dispatch queue drain while
/// we wait. We are called from *inside* the tao event callback, and tao holds a
/// non-reentrant `std::sync::Mutex` on its handler for the whole duration of that
/// callback (`Handler::handle_nonuser_event` locks `callback` and invokes us
/// while still holding the guard; the inner `EventLoopHandler` then also
/// `borrow_mut`s a `RefCell`). A nested run loop dispatches Cocoa events straight
/// back into that handler on this same thread, which self-deadlocks on the mutex
/// — a hang no deadline can break, since the deadline check never runs again.
///
/// Not pumping is safe because nothing the teardown does needs the main queue any
/// more: `avlayer_sink_create` dispatches asynchronously (see
/// renderers/avsample_sink.m). Should some future path block on the main queue
/// again, the cost here is a bounded `secs` delay on quit, not a deadlock.
fn wait_bounded(done: &AtomicBool, secs: f64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
    while !done.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

struct Inner {
    window: Option<Window>,
    airplay: Option<AirPlay>,
    nsview: usize,
    running: bool,
    fullscreen: bool, // honour the Settings "fullscreen on connect" checkbox
    // Async-teardown state (see `begin`). The engine worker is joined OFF the main
    // thread so the run loop never freezes; these track the in-flight transition.
    transitioning: bool,          // a stop/restart join is running on a worker thread
    next_cfg: Option<Config>,     // start with this once the current teardown finishes
    queued: Option<Option<Config>>, // a request that arrived mid-transition (latest wins)
    // The in-flight teardown thread. Kept (not detached) so a Quit landing mid-
    // transition can WAIT for it — otherwise `stop_blocking` finds `airplay: None`,
    // skips the teardown it promises, and `process::exit(0)` runs while that thread
    // is still inside GStreamer teardown / logger_destroy / dlclose.
    join: Option<std::thread::JoinHandle<()>>,
    // A rotation arrived while fullscreen (can't resize a fullscreen window): re-fit
    // the windowed frame to the current aspect once we're back in windowed mode.
    pending_refit: bool,
}

/// Tray-side handle to the in-process engine + its mirror window. Main-thread only.
pub struct Engine {
    inner: RefCell<Inner>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            inner: RefCell::new(Inner {
                window: None,
                airplay: None,
                nsview: 0,
                running: false,
                fullscreen: false,
                transitioning: false,
                next_cfg: None,
                queued: None,
                join: None,
                pending_refit: false,
            }),
        }
    }

    /// Create the hidden mirror window once the event loop is running and stash
    /// its `NSView*`. Call from `StartCause::Init`.
    pub fn attach_window(&self, target: &EventLoopWindowTarget<AppEvent>) -> anyhow::Result<()> {
        let mut inner = self.inner.borrow_mut();
        if inner.window.is_some() {
            return Ok(());
        }
        let window = WindowBuilder::new()
            .with_title(crate::config::APP_NAME)
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_visible(false)
            .build(target)?;
        let nsview: usize = match window.window_handle()?.as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as usize,
            other => anyhow::bail!("expected AppKit window handle, got {other:?}"),
        };
        eprintln!("[engine-macos] mirror window attached, NSView = {nsview:#x}");
        inner.nsview = nsview;
        inner.window = Some(window);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.inner.borrow().running
    }

    /// The real start — synchronous and FAST: `airplay_core_start` only spawns the
    /// engine worker and returns (no join here), so the main thread never blocks.
    fn start_inner(&self, cfg: &Config) -> anyhow::Result<()> {
        set_bundled_gst_env(); // before the dylib's gst_init (no-op for `cargo run`)
        let mut inner = self.inner.borrow_mut();
        if inner.nsview == 0 {
            anyhow::bail!("mirror window not attached yet (attach_window must run first)");
        }
        let dll = core_dylib_path();
        let mut ap = AirPlay::load(&dll)?;
        ap.set_log_callback(engine_log_cb, std::ptr::null_mut());
        // NOT `let _`: a config string with an interior NUL (config.json is hand-
        // edited, and a JSON \u0000 escape is a legal way to write one) fails
        // CString::new here, and swallowing that started the engine with NO options
        // at all — no -bind, no sink, no fps — while the tray reported Ready. The C
        // side only ever returns non-zero for a null handle, which `AirPlay::load`
        // already ruled out, so these are the config errors they look like.
        ap.set_device_name(&cfg.device_name)?;
        ap.set_window(inner.nsview as *mut c_void)?;
        ap.set_options(&build_options(cfg))?;
        CONNECTED.store(false, Ordering::SeqCst);
        ASPECT_WH.store(0, Ordering::SeqCst);
        // Per-run, like the two above: last run's failure / ignored-pin verdict must
        // not outlive it, or the tray keeps warning about an adapter already fixed.
        crate::status::reset_run_flags();
        ap.start()?;
        inner.airplay = Some(ap);
        inner.running = true;
        inner.fullscreen = cfg.fullscreen;
        eprintln!("[engine-macos] engine started (dylib: {dll}, fullscreen={})", cfg.fullscreen);
        Ok(())
    }

    /// A start that happens AFTER its caller has answered — the deferred half of a
    /// restart, or a start queued behind a teardown. `restart()` returns `Ok` the
    /// moment the teardown is queued, so a failure here has no return value left
    /// to travel on: without this it was printed to the log while `running` stayed
    /// true, leaving the tray on Ready with no engine and making the next Start a
    /// silent no-op. Route it exactly like an engine that died on its own — the
    /// tray's `Status::Off` + `START_FAILED` handler does the teardown and tells
    /// the user.
    fn start_or_report(&self, cfg: &Config) {
        if let Err(e) = self.start_inner(cfg) {
            eprintln!("[engine-macos] deferred start failed: {e:#}");
            self.inner.borrow_mut().running = false;
            crate::status::START_FAILED.store(true, Ordering::SeqCst);
            send_status(Status::Off);
        }
    }

    pub fn start(&self, cfg: &Config) -> anyhow::Result<()> {
        {
            let mut inner = self.inner.borrow_mut();
            if inner.transitioning {
                // A teardown is in flight — apply this start when it completes.
                inner.queued = Some(Some(cfg.clone()));
                return Ok(());
            }
            if inner.running {
                return Ok(());
            }
        }
        self.start_inner(cfg)
    }

    /// Async lifecycle core. Tears down the current engine by joining its worker on
    /// a throwaway thread (NOT the main thread — that would freeze the NSApplication
    /// run loop and, before the avlayer dispatch_async fix, deadlock), then on
    /// completion (`on_engine_stopped`, main thread) honours `req`:
    /// `Some(cfg)` = (re)start with cfg, `None` = stay stopped.
    fn begin(&self, req: Option<Config>) {
        let mut inner = self.inner.borrow_mut();
        if inner.transitioning {
            inner.queued = Some(req); // latest intent wins; applied on completion
            return;
        }
        let Some(ap) = inner.airplay.take() else {
            // Not running -> nothing to tear down; honour the request immediately.
            drop(inner);
            match req {
                Some(cfg) => self.start_or_report(&cfg),
                None => self.inner.borrow_mut().running = false,
            }
            return;
        };
        // Reflect intent + hide the window immediately so the UI stays responsive.
        if let Some(w) = inner.window.as_ref() {
            w.set_fullscreen(None);
            w.set_visible(false);
        }
        CONNECTED.store(false, Ordering::SeqCst);
        inner.running = req.is_some(); // a restart stays "running" (tray shows Stop)
        inner.next_cfg = req;
        inner.transitioning = true;
        // Join the engine worker off-main; ping the loop (-> on_engine_stopped) when done.
        // The handle is KEPT (not detached) so a Quit arriving mid-transition can wait
        // for this teardown instead of exiting through it — see `stop_blocking`.
        inner.join = Some(std::thread::spawn(move || {
            let mut ap = ap;
            ap.stop(); // airplay_core_stop: request shutdown + worker.join()
            drop(ap); // airplay_core_destroy()
            signal_engine_stopped();
        }));
    }

    /// Main-thread completion of an async teardown (AppEvent::EngineStopped).
    pub fn on_engine_stopped(&self) {
        let (queued, next, done) = {
            let mut inner = self.inner.borrow_mut();
            inner.transitioning = false;
            (inner.queued.take(), inner.next_cfg.take(), inner.join.take())
        };
        // The teardown thread signals us as its very last act, so this join is
        // effectively instant — it just reaps the handle before `begin` overwrites it.
        if let Some(h) = done {
            let _ = h.join();
        }
        if let Some(req) = queued {
            self.begin(req); // a newer request arrived mid-teardown — apply it now
        } else if let Some(cfg) = next {
            self.start_or_report(&cfg); // `begin` already set running=true for this
        }
        // else: a plain stop completed — stay stopped.
    }

    pub fn stop(&self) {
        self.begin(None);
    }

    /// `Ok` here means "the teardown is queued", NOT "the engine is up" — the real
    /// start happens later, on the main thread, from `on_engine_stopped`. Its
    /// failure therefore cannot come back through this return value; it travels
    /// the tray's failed-start path instead (see `start_or_report`).
    pub fn restart(&self, cfg: &Config) -> anyhow::Result<()> {
        self.begin(Some(cfg.clone()));
        Ok(())
    }

    /// Synchronous stop for app quit only: wait for the engine worker (the brief pause
    /// is irrelevant at exit) so the AirPlay ports are released cleanly before the
    /// process ends — the async `stop()` would be abandoned by `process::exit`.
    ///
    /// The teardown runs on a HELPER thread and this one waits on a bounded flag —
    /// `worker.join()` must never happen on the macOS main thread. It does NOT pump
    /// the main queue while waiting: doing that from inside the tao event callback
    /// re-enters tao's handler, which holds a non-reentrant mutex for the duration
    /// of the callback, and self-deadlocks with no deadline able to break it (see
    /// `wait_bounded`). Not pumping is safe because `avlayer_sink_create` dispatches
    /// asynchronously, so the worker never waits on the main queue; if some future
    /// path does, the cost here is a bounded delay on quit rather than a hang.
    pub fn stop_blocking(&self) {
        let (ap, inflight) = {
            let mut inner = self.inner.borrow_mut();
            // Stay "transitioning" across the wait below: it makes any start/stop
            // re-entering through the run loop queue itself instead of touching the
            // engine we are tearing down. Cleared once the wait is over.
            inner.transitioning = true;
            inner.queued = None;
            inner.next_cfg = None;
            inner.running = false;
            (inner.airplay.take(), inner.join.take())
        };
        // `inflight` is a teardown `begin()` already started (it holds the handle,
        // so `airplay` is None here) — without waiting on it, `process::exit(0)`
        // would run straight through GStreamer teardown / logger_destroy / dlclose.
        if ap.is_some() || inflight.is_some() {
            let done = std::sync::Arc::new(AtomicBool::new(false));
            let flag = done.clone();
            std::thread::spawn(move || {
                if let Some(mut ap) = ap {
                    ap.stop(); // airplay_core_stop: request shutdown + worker.join()
                    drop(ap); // airplay_core_destroy()
                }
                if let Some(h) = inflight {
                    let _ = h.join();
                }
                flag.store(true, Ordering::SeqCst);
            });
            if !wait_bounded(&done, 3.0) {
                eprintln!("[engine-macos] stop_blocking: teardown still running after 3s, exiting anyway");
            }
        }
        let mut inner = self.inner.borrow_mut();
        inner.transitioning = false;
        inner.queued = None;
        inner.next_cfg = None;
        CONNECTED.store(false, Ordering::SeqCst);
        if let Some(w) = inner.window.as_ref() {
            w.set_visible(false);
        }
    }

    /// Show or hide the mirror window (called from the StatusChanged handler on
    /// the main thread). On show, fit the window to the content aspect.
    pub fn set_mirror_visible(&self, visible: bool) {
        let inner = self.inner.borrow();
        let Some(w) = inner.window.as_ref() else {
            eprintln!("[engine-macos] set_mirror_visible({visible}): NO WINDOW");
            return;
        };
        eprintln!("[engine-macos] set_mirror_visible({visible})");
        if visible {
            // Fit to the content aspect, then honour the Settings "fullscreen"
            // checkbox (default off on macOS); otherwise stay windowed and the
            // user can fullscreen manually (green button / Ctrl-Cmd-F).
            self.fit_to_aspect(w);
            w.set_visible(true);
            if inner.fullscreen {
                w.set_fullscreen(Some(Fullscreen::Borderless(None)));
            }
            w.set_focus();
        } else {
            w.set_fullscreen(None); // drop fullscreen if it was on
            w.set_visible(false);
            drop(inner);
            self.inner.borrow_mut().pending_refit = false; // stale across sessions
        }
    }

    /// Re-fit the mirror window to the new content aspect after an iPhone rotation
    /// (signalled from the worker via `REFIT_TX` when the aspect changes). Runs on
    /// the main thread. While fullscreen we can't resize (borderless must fill the
    /// screen; the avlayer's resizeAspect already letterboxes there) — so we just
    /// REMEMBER the rotation and apply it on `on_window_resized` once we're windowed
    /// again (otherwise exiting fullscreen leaves the old, wrong-aspect frame).
    pub fn refit_mirror(&self) {
        let inner = self.inner.borrow();
        if !inner.running {
            return;
        }
        let Some(w) = inner.window.as_ref() else { return };
        if inner.fullscreen || w.fullscreen().is_some() {
            drop(inner);
            self.inner.borrow_mut().pending_refit = true;
            return;
        }
        self.fit_to_aspect(w);
    }

    /// The mirror window resized — if a rotation happened while we were fullscreen,
    /// apply the deferred re-fit now that we're back in windowed mode. (Guarded by
    /// `pending_refit` so it never fights the user's own manual resizes.)
    pub fn on_window_resized(&self) {
        let do_fit = {
            let inner = self.inner.borrow();
            inner.pending_refit
                && inner.window.as_ref().map_or(false, |w| w.fullscreen().is_none())
        };
        if !do_fit {
            return;
        }
        let mut inner = self.inner.borrow_mut();
        inner.pending_refit = false;
        if let Some(w) = inner.window.as_ref() {
            self.fit_to_aspect(w);
        }
    }

    /// Live always-on-top toggle.
    pub fn set_topmost(&self, on: bool) {
        if let Some(w) = self.inner.borrow().window.as_ref() {
            w.set_always_on_top(on);
        }
    }

    /// Resize the (windowed) mirror to the current video aspect, centered within
    /// ~85% of its monitor's work area — so the portrait/landscape phone image
    /// fills the window instead of being pill/letterboxed.
    fn fit_to_aspect(&self, w: &Window) {
        let packed = ASPECT_WH.load(Ordering::SeqCst);
        if packed == 0 {
            return;
        }
        let (vw, vh) = ((packed >> 32) as f64, (packed & 0xffff_ffff) as f64);
        if vw <= 0.0 || vh <= 0.0 {
            return;
        }
        let Some(mon) = w.current_monitor() else { return };
        let scale = mon.scale_factor();
        let msize: PhysicalSize<u32> = mon.size();
        let mpos: PhysicalPosition<i32> = mon.position();
        let (maxw, maxh) = (msize.width as f64 * 0.85, msize.height as f64 * 0.85);
        // Fit vw:vh inside maxw x maxh (physical px).
        let mut pw = maxw;
        let mut ph = pw * vh / vw;
        if ph > maxh {
            ph = maxh;
            pw = ph * vw / vh;
        }
        let x = mpos.x as f64 + (msize.width as f64 - pw) / 2.0;
        let y = mpos.y as f64 + (msize.height as f64 - ph) / 2.0;
        w.set_inner_size(PhysicalSize::new(pw, ph));
        w.set_outer_position(PhysicalPosition::new(x, y));
        let _ = scale; // sizes already in physical px
    }
}
