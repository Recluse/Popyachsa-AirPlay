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
// (an INFO line) reliably fires on every connect / reconnect / engine restart, so
// it's the window-show trigger. The "begin video stream wxh" DEBUG line is only
// used to capture the aspect (it was unreliable across an engine restart).
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
    // Fall back to a bare name and let the loader search path resolve it.
    PathBuf::from("uxplay-core.dylib").to_string_lossy().into_owned()
}

/// When running from a packaged `.app`, point GStreamer at the BUNDLED plugins +
/// a writable registry, so the receiver is fully self-contained (needs no system
/// GStreamer.framework). No-op for `cargo run` (the bundle dir won't exist). Must
/// run before the dylib's `gst_init`, i.e. before `ap.start()`.
fn set_bundled_gst_env() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(macos) = exe.parent() else { return }; // …/Foo.app/Contents/MacOS
    let gst = macos.join("../Frameworks/GStreamer");
    let plugins = gst.join("lib/gstreamer-1.0");
    if !plugins.exists() {
        return; // not a bundled run
    }
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
        let _ = ap.set_device_name(&cfg.device_name);
        let _ = ap.set_window(inner.nsview as *mut c_void);
        let _ = ap.set_options(&build_options(cfg));
        CONNECTED.store(false, Ordering::SeqCst);
        ASPECT_WH.store(0, Ordering::SeqCst);
        ap.start()?;
        inner.airplay = Some(ap);
        inner.running = true;
        inner.fullscreen = cfg.fullscreen;
        eprintln!("[engine-macos] engine started (dylib: {dll}, fullscreen={})", cfg.fullscreen);
        Ok(())
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
                Some(cfg) => {
                    if let Err(e) = self.start_inner(&cfg) {
                        eprintln!("[engine-macos] start: {e}");
                    }
                }
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
        std::thread::spawn(move || {
            let mut ap = ap;
            ap.stop(); // airplay_core_stop: request shutdown + worker.join()
            drop(ap); // airplay_core_destroy()
            signal_engine_stopped();
        });
    }

    /// Main-thread completion of an async teardown (AppEvent::EngineStopped).
    pub fn on_engine_stopped(&self) {
        let (queued, next) = {
            let mut inner = self.inner.borrow_mut();
            inner.transitioning = false;
            (inner.queued.take(), inner.next_cfg.take())
        };
        if let Some(req) = queued {
            self.begin(req); // a newer request arrived mid-teardown — apply it now
        } else if let Some(cfg) = next {
            if let Err(e) = self.start_inner(&cfg) {
                eprintln!("[engine-macos] restart-start: {e}");
            }
        }
        // else: a plain stop completed — stay stopped.
    }

    pub fn stop(&self) {
        self.begin(None);
    }

    pub fn restart(&self, cfg: &Config) -> anyhow::Result<()> {
        self.begin(Some(cfg.clone()));
        Ok(())
    }

    /// Synchronous stop for app quit only: block on the worker join (the brief pause
    /// is irrelevant at exit) so the AirPlay ports are released cleanly before the
    /// process ends — the async `stop()` would be abandoned by `process::exit`.
    pub fn stop_blocking(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.transitioning = false;
        inner.queued = None;
        inner.next_cfg = None;
        if let Some(mut ap) = inner.airplay.take() {
            ap.stop();
        }
        inner.running = false;
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
