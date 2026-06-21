//! Real Linux in-process AirPlay engine.
//!
//! A raw-Xlib host window (the analogue of the Windows `engine.rs` host window)
//! that the engine renders the AirPlay mirror into via `GstVideoOverlay` (X11
//! XID). The mechanism is proven by `research/l3-smoke`; this wires it into the
//! actual tray app.
//!
//! Why raw Xlib and not tao/GTK: the tray already owns the main thread's GTK
//! event loop, and GTK is single-main-thread. Xlib — unlike GTK — is happy on a
//! worker thread once `XInitThreads()` is called, so the engine keeps the same
//! self-contained "own window + own loop on a worker thread" shape it has on
//! Windows. Wayland is post-v1 (run under Xwayland for now; see PLAN-LINUX L4).
//!
//! v1 video path: `ximagesink` (software X11 sink) + `-avdec` (software H.264) —
//! robust on every box including headless WSL. HW decode (VA-API/NVDEC) +
//! glimagesink is the L5 selector. Fullscreen/borderless/aspect = later polish.

use std::ffi::{c_void, CStr, CString};
use std::os::raw::{c_char, c_int, c_ulong};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use airplay_lib::AirPlay;
use x11::xlib;

use crate::config::Config;
use crate::status::Status;

// Connection markers in UxPlay's log stream (same as the Windows engine).
const MARK_CONNECTED: &str = "Begin streaming";
const MARK_TEARDOWN: &str = "Open connections: 0";

static STATUS_TX: Mutex<Option<Sender<Status>>> = Mutex::new(None);
// Connection state + a generation counter the X11 loop watches to map/unmap the
// window (single-instance engine, so process-global statics are fine).
static CONNECTED: AtomicBool = AtomicBool::new(false);
// True only while a live engine owns the host window. Gates the C log callback so a
// late call from a *dying* engine (UxPlay drains its log thread during stop/destroy)
// can't mutate the connection/resize statics the NEXT worker is about to read — the
// Linux analogue of the Windows engine's `CB_HWND == 0` callback guard.
static ENGINE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CONN_GEN: AtomicU64 = AtomicU64::new(0);
// Current video frame size (w<<32 | h), 0 = unknown. Parsed from the engine's
// "begin video stream wxh = WxH" line (fires at stream start AND on rotation) —
// used to fit the window to the content aspect (no black bars).
static VIDEO_WH: AtomicU64 = AtomicU64::new(0);
static RESIZE_GEN: AtomicU64 = AtomicU64::new(0);

/// Main installs a sender so connect/disconnect reach the tray icon.
pub fn install_status_sender(tx: Sender<Status>) {
    // Poison-tolerant: a panic in another holder must not wedge status delivery.
    *STATUS_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
}
fn send_status(s: Status) {
    if let Some(tx) = STATUS_TX.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(s);
    }
}

/// Engine log directory — a **user-writable** XDG path (`<data_dir>/logs`, e.g.
/// `~/.local/share/PopyachsaAirPlay/logs`), NOT next to the exe: on a system
/// (.deb/.rpm) or Flatpak install `<exe-dir>/logs` is root-owned / read-only.
pub fn engine_log_dir() -> std::path::PathBuf {
    crate::config::log_dir()
}

/// Resolve `uxplay-core.so`: next to the exe (dist), else rely on the loader
/// search path (`LD_LIBRARY_PATH` / rpath / a system install dir).
fn core_so_path() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("uxplay-core.so");
            if c.exists() {
                return c.to_string_lossy().into_owned();
            }
        }
    }
    "uxplay-core.so".to_string()
}

/// UxPlay option tail for Linux v1 (software path; HW decode = L5 selector).
/// `xv_available` (the X11 XVideo extension) selects the video sink.
fn build_options(cfg: &Config, xv_available: bool) -> String {
    let mut a: Vec<String> = Vec::new();
    if cfg.debug_logging {
        a.push("-d".into());
    }
    a.extend(["-nh", "-nohold", "-nc"].iter().map(|s| s.to_string()));
    a.extend(["-fps".into(), cfg.target_fps.to_string()]);
    a.extend(["-vsync".into(), "no".into()]);
    // h265 is OFF on Linux v1: this UxPlay fork hits a renderer-bus assertion on
    // h265 RECONNECT (video_renderer.c::video_renderer_listen) -> SIGABRT. h264 is
    // robust; re-enabling h265 waits on the reconnect-lifecycle fix.
    let _ = cfg.enable_h265;
    // Video sink: prefer xvimagesink — it scales in the X11 XVideo hardware
    // overlay (sharper, off-CPU) instead of ximagesink's CPU bilinear path. That
    // matters on a HiDPI panel where a modest AirPlay source is scaled up (the
    // reported "blurry window"). Same GstVideoOverlay XID interface, so
    // set_window(xid) is unchanged. Fall back to ximagesink where XVideo is absent
    // (headless / WSL / many remote-X / Xwayland) — the robust software path.
    let video_sink = if xv_available { "xvimagesink" } else { "ximagesink" };
    a.extend(["-vs".into(), video_sink.into()]);
    // Decoder (from Settings, Linux values): auto = uxplay's decodebin (HW if
    // available, software fallback — the robust default); software = avdec;
    // vaapi = vah264dec (Intel/AMD); nvidia = nvh264dec.
    match cfg.video_decoder.as_str() {
        "software" => a.push("-avdec".into()),
        "vaapi" => a.extend(["-vd".into(), "vah264dec".into()]),
        "nvidia" => a.extend(["-vd".into(), "nvh264dec".into()]),
        _ => {} // "auto" / unknown -> decodebin default
    }
    // Audio (from Settings, Linux values): "" = off (fakesink); else the
    // GStreamer sink name (autoaudiosink / pulsesink / pipewiresink / alsasink).
    match cfg.audio_sink.trim() {
        "" => a.extend(["-as".into(), "fakesink".into()]),
        s => a.extend(["-as".into(), s.to_string()]),
    }
    if !cfg.custom_flags.trim().is_empty() {
        for tok in cfg.custom_flags.split_whitespace() {
            a.push(tok.to_string());
        }
    }
    a.join(" ")
}

extern "C" fn engine_log_cb(_level: c_int, msg: *const c_char, _user: *mut c_void) {
    // The C engine calls this on its own thread. A panic must NOT unwind across the
    // FFI boundary into C (UB), so contain it (the body is panic-free today, but
    // this is cheap insurance against future edits / debug builds where panics
    // unwind rather than abort).
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    if msg.is_null() {
        return;
    }
    // Drop callbacks from an engine that is no longer the active one (its log
    // thread is still draining during stop/destroy). Without this, a stale
    // "Begin streaming"/teardown/ROTATION line could flip the connection or
    // resize statics that the next worker's X11 loop reads -> ghost map/resize.
    if !ENGINE_ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    if text.contains(MARK_CONNECTED) {
        if !CONNECTED.swap(true, Ordering::SeqCst) {
            CONN_GEN.fetch_add(1, Ordering::SeqCst);
        }
    } else if text.contains(MARK_TEARDOWN) && CONNECTED.swap(false, Ordering::SeqCst) {
        CONN_GEN.fetch_add(1, Ordering::SeqCst);
    }
    // Video frame size -> fit the window to the content aspect (no black bars).
    // Parse the *display* W,H from UxPlay's "display dimensions: w=W h=H" line:
    // it is LOGGER_INFO (so it reaches this callback WITHOUT -d), is
    // codec-independent (raop_rtp_mirror, not the renderer), and re-fires on
    // rotation. The alternatives don't work for us: "begin video stream wxh" is
    // DEBUG-only, and "video format is ... video WxH" is h265-only (we run h264).
    if text.contains("display dimensions:") {
        // The " w=" / " h=" tokens carry the final display size; take the digits
        // right after each.
        let num_after = |key: &str| -> Option<u32> {
            text.split(key)
                .nth(1)?
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<u32>()
                .ok()
        };
        if let (Some(w), Some(h)) = (num_after(" w="), num_after(" h=")) {
            if w > 0 && h > 0 {
                let packed = ((w as u64) << 32) | h as u64;
                if VIDEO_WH.swap(packed, Ordering::SeqCst) != packed {
                    RESIZE_GEN.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }
    })); // end catch_unwind
}

/// Body of the host-window thread: create an X11 window, hand its XID to the
/// engine, then poll X events + connection state until stop.
fn run_host_window(cfg: Config, running: Arc<AtomicBool>, stop_flag: Arc<AtomicBool>) {
    unsafe {
        let display = xlib::XOpenDisplay(std::ptr::null());
        if display.is_null() {
            eprintln!("[engine] XOpenDisplay failed (no X11 DISPLAY? under Wayland set GDK_BACKEND=x11 / run Xwayland)");
            running.store(false, Ordering::SeqCst);
            return;
        }
        let screen = xlib::XDefaultScreen(display);
        let root = xlib::XRootWindow(display, screen);
        let black = xlib::XBlackPixel(display, screen);
        let window = xlib::XCreateSimpleWindow(display, root, 0, 0, 1280, 720, 0, black, black);

        // Probe the X11 XVideo extension -> pick xvimagesink (sharper HiDPI scaling)
        // when present, else ximagesink. Also log the screen size + KDE/Xft scale,
        // for diagnosing blurry-window reports on fractionally-scaled (e.g. 1.5x)
        // displays. (KDE writes Xft.dpi=144 for 1.5x on X11; under Xwayland the
        // compositor upscales the X buffer and there is no X11 opt-out — a native
        // Wayland sink is the eventual fix there.)
        let (mut xvo, mut xve, mut xver) = (0, 0, 0);
        let xv_available = xlib::XQueryExtension(
            display, b"XVideo\0".as_ptr() as *const c_char, &mut xvo, &mut xve, &mut xver) != 0;
        let dpi = {
            let rms = xlib::XResourceManagerString(display);
            if rms.is_null() { String::new() } else {
                CStr::from_ptr(rms).to_string_lossy()
                    .lines().find(|l| l.starts_with("Xft.dpi"))
                    .map(|l| l.trim().to_string()).unwrap_or_default()
            }
        };
        eprintln!("[engine] X11 screen {}x{}, XVideo={} (sink={}), {}",
            xlib::XDisplayWidth(display, screen), xlib::XDisplayHeight(display, screen),
            xv_available, if xv_available { "xvimagesink" } else { "ximagesink" },
            if dpi.is_empty() { "Xft.dpi=unset".into() } else { dpi });

        let title = CString::new("Popyachsa AirPlay").unwrap();
        xlib::XStoreName(display, window, title.as_ptr());

        // WM_DELETE_WINDOW so the window's close button reaches us as a
        // ClientMessage instead of killing the X connection under us.
        let wm_protocols =
            xlib::XInternAtom(display, b"WM_PROTOCOLS\0".as_ptr() as *const c_char, 0);
        let mut wm_delete =
            xlib::XInternAtom(display, b"WM_DELETE_WINDOW\0".as_ptr() as *const c_char, 0);
        xlib::XSetWMProtocols(display, window, &mut wm_delete, 1);
        xlib::XSelectInput(display, window, xlib::StructureNotifyMask);
        // Created HIDDEN (not mapped): the engine just advertises; the window is
        // shown only when a device actually connects.
        xlib::XFlush(display);

        let xid = window as c_ulong;
        CONNECTED.store(false, Ordering::SeqCst);
        VIDEO_WH.store(0, Ordering::SeqCst);
        let mut last_gen = CONN_GEN.load(Ordering::SeqCst);
        let mut last_resize = RESIZE_GEN.load(Ordering::SeqCst);

        let so = core_so_path();
        let mut ap = match AirPlay::load(&so) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[engine] loading {so}: {e}");
                xlib::XDestroyWindow(display, window);
                xlib::XCloseDisplay(display);
                running.store(false, Ordering::SeqCst);
                return;
            }
        };
        ap.set_log_callback(engine_log_cb, std::ptr::null_mut());
        let _ = ap.set_device_name(&cfg.device_name);
        let _ = ap.set_window(xid as *mut c_void); // XID -> GstVideoOverlay
        let _ = ap.set_options(&build_options(&cfg, xv_available));
        // Open the callback gate just before start so connect markers emitted during
        // startup are honored; it is closed again at teardown before stop/destroy.
        ENGINE_ACTIVE.store(true, Ordering::SeqCst);
        if let Err(e) = ap.start() {
            eprintln!("[engine] start: {e}");
        }
        running.store(true, Ordering::SeqCst);
        // Engine is up and advertising (no device yet) -> tell the tray to show the
        // "Ready" state (blue icon). Without this, an autostarted engine leaves the
        // icon on "Off" (grey) until a device connects, because the only later
        // status events are Connected (on connect) / Ready (on disconnect) / Off
        // (on teardown) — the initial Ready transition was never sent.
        send_status(Status::Ready);
        eprintln!("[engine] X11 host window 0x{xid:x} up; engine started");

        // Poll loop: drain X events, map/unmap on connect/disconnect, exit on stop.
        let mut mapped = false;
        loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            while xlib::XPending(display) > 0 {
                let mut ev: xlib::XEvent = std::mem::zeroed();
                xlib::XNextEvent(display, &mut ev);
                if ev.get_type() == xlib::ClientMessage {
                    let cm = ev.client_message;
                    if cm.message_type == wm_protocols
                        && cm.data.get_long(0) as xlib::Atom == wm_delete
                    {
                        stop_flag.store(true, Ordering::SeqCst);
                    }
                }
            }
            let g = CONN_GEN.load(Ordering::SeqCst);
            if g != last_gen {
                last_gen = g;
                let connected = CONNECTED.load(Ordering::SeqCst);
                if connected && !mapped {
                    xlib::XMapWindow(display, window);
                    xlib::XFlush(display);
                    mapped = true;
                    send_status(Status::Connected);
                } else if !connected && mapped {
                    xlib::XUnmapWindow(display, window);
                    xlib::XFlush(display);
                    mapped = false;
                    send_status(Status::Ready);
                }
            }
            // Fit the window to the video aspect (no black bars) on size change /
            // rotation. Fits W:H within ~85% of the screen, centered.
            let rg = RESIZE_GEN.load(Ordering::SeqCst);
            if rg != last_resize {
                last_resize = rg;
                let packed = VIDEO_WH.load(Ordering::SeqCst);
                if packed != 0 {
                    let vw = (packed >> 32) as i64;
                    let vh = (packed & 0xffff_ffff) as i64;
                    if vw > 0 && vh > 0 {
                        let sw = xlib::XDisplayWidth(display, screen) as i64;
                        let sh = xlib::XDisplayHeight(display, screen) as i64;
                        let maxw = (sw * 85 / 100).max(160);
                        let maxh = (sh * 85 / 100).max(120);
                        // Present at the SOURCE size, only shrinking to fit ~85% of
                        // the screen — never UPscale past native. Blowing a modest
                        // AirPlay source up to fill a HiDPI panel is what made the
                        // window look soft; at 1:1 (or smaller) each source pixel maps
                        // to >=1 screen pixel, so it stays crisp. xvimagesink still
                        // gives a clean hardware downscale when a clamp is needed.
                        let mut w = vw.min(maxw);
                        let mut h = w * vh / vw;
                        if h > maxh {
                            h = maxh;
                            w = h * vw / vh;
                        }
                        let x = ((sw - w) / 2).max(0);
                        let y = ((sh - h) / 2).max(0);
                        xlib::XMoveResizeWindow(
                            display, window, x as c_int, y as c_int, w as u32, h as u32,
                        );
                        xlib::XFlush(display);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // ORDER MATTERS: stop the engine, then DESTROY it (airplay_core_destroy),
        // and only THEN close the X display. GstVideoOverlay/ximagesink may still
        // touch the Display* during stop+destroy, so closing the display first would
        // poke freed memory (crash on restart).
        eprintln!("[engine] stopping engine, then destroying X11 window");
        // Close the callback gate FIRST: from here on UxPlay's draining log thread
        // may still invoke engine_log_cb, but those late lines must not touch the
        // shared statics the next worker will read.
        ENGINE_ACTIVE.store(false, Ordering::SeqCst);
        CONNECTED.store(false, Ordering::SeqCst);
        ap.stop();
        drop(ap); // airplay_core_destroy, on this thread, while Display* is still valid
        xlib::XDestroyWindow(display, window);
        xlib::XCloseDisplay(display);
        running.store(false, Ordering::SeqCst);
        // The window may have closed on its own (WM_DELETE) — tell the tray so it
        // joins the worker + rebuilds the menu (Start instead of Stop).
        send_status(Status::Off);
    }
}

/// Tray-side handle to the in-process engine + its X11 host window.
pub struct Engine {
    running: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Engine {
    pub fn new() -> Self {
        // XInitThreads() is called first thing in main() (before any GTK/X use),
        // which is what makes our worker-thread Xlib use safe — not here.
        Self {
            running: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn start(&self, cfg: &Config) -> anyhow::Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
        self.stop_flag.store(false, Ordering::SeqCst);
        let cfg = cfg.clone();
        let running = self.running.clone();
        let stop_flag = self.stop_flag.clone();
        match std::thread::Builder::new()
            .name("airplay-host-window".into())
            .spawn(move || run_host_window(cfg, running, stop_flag))
        {
            Ok(h) => {
                *self.thread.lock().unwrap() = Some(h);
                Ok(())
            }
            Err(e) => {
                self.running.store(false, Ordering::SeqCst);
                Err(e.into())
            }
        }
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
        self.running.store(false, Ordering::SeqCst);
    }

    /// Synchronous stop for app quit (uniform API; `stop` already joins here).
    pub fn stop_blocking(&self) {
        self.stop();
    }

    pub fn restart(&self, cfg: &Config) -> anyhow::Result<()> {
        self.stop(); // joins the worker, which already ran stop()+destroy()
        // Margin for the OS to release the RAOP/AirPlay UDP ports + the mDNS
        // registration that destroy() tore down, so the new start() can rebind.
        std::thread::sleep(Duration::from_millis(200));
        self.start(cfg)
    }

    /// Always-on-top. TODO(L-polish): `_NET_WM_STATE_ABOVE` via XSendEvent to the
    /// root window. No-op for v1.
    pub fn set_topmost(&self, _on: bool) {}
}
