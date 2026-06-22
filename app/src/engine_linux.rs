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
use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong};
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

// --- XVideo port query (libXv) -------------------------------------------------
// The sink choice is xvimagesink (XVideo hardware overlay — scales + colour-converts
// in the GPU, so 4K HEVC renders smoothly off-CPU) vs ximagesink (software XShm
// PutImage — fine for 1080p, but CPU-blits every 4K frame and stutters). Each
// xvimagesink instance grabs one XVideo "port"; the h265 path builds TWO video sinks
// at once (h264 + h265 renderers), so it needs >= 2 ports. GPU drivers usually expose
// ~16; some emulated/minimal X servers (e.g. WSLg) expose 1, where the 2nd xvimagesink
// can't get a port and that renderer fails to init. So we COUNT the real ports and
// fall back to ximagesink only when there genuinely aren't enough (logged) — instead
// of guessing by environment.
#[repr(C)]
struct XvAdaptorInfo {
    base_id: c_ulong,
    num_ports: c_ulong,
    type_: c_char,
    name: *mut c_char,
    num_formats: c_ulong,
    formats: *mut c_void, // XvFormat*
    num_adaptors: c_ulong,
}
#[link(name = "Xv")]
extern "C" {
    fn XvQueryAdaptors(
        display: *mut xlib::Display,
        window: xlib::Window,
        num_adaptors: *mut c_uint,
        adaptor_info: *mut *mut XvAdaptorInfo,
    ) -> c_int;
    fn XvFreeAdaptorInfo(adaptor_info: *mut XvAdaptorInfo);
}

/// Total XVideo ports on image-input adaptors — i.e. ports an `xvimagesink` can grab
/// (`XvShmPutImage` = image data input to the screen). Returns 0 on any error / no
/// XVideo, which the caller treats as "use ximagesink".
unsafe fn xv_image_ports(display: *mut xlib::Display, window: xlib::Window) -> u32 {
    const XV_INPUT_IMAGE: u8 = 0x01 | 0x10; // XvInputMask (1<<XvInput) | XvImageMask
    let mut n: c_uint = 0;
    let mut adaptors: *mut XvAdaptorInfo = std::ptr::null_mut();
    // XvQueryAdaptors returns Success (0) on success.
    if XvQueryAdaptors(display, window, &mut n, &mut adaptors) != 0 || adaptors.is_null() {
        return 0;
    }
    let mut ports: u64 = 0;
    for i in 0..n as isize {
        let a = &*adaptors.offset(i);
        if (a.type_ as u8 & XV_INPUT_IMAGE) == XV_INPUT_IMAGE {
            ports += a.num_ports as u64;
        }
    }
    XvFreeAdaptorInfo(adaptors);
    ports.min(u64::from(u16::MAX)) as u32
}

/// UxPlay option tail for Linux v1 (software path; HW decode = L5 selector).
/// `video_sink` is the GStreamer sink the caller picked from the real XVideo port
/// count (see `xv_image_ports`): xvimagesink on hardware with enough ports, else ximagesink.
fn build_options(cfg: &Config, video_sink: &str) -> String {
    let mut a: Vec<String> = Vec::new();
    if cfg.debug_logging {
        a.push("-d".into());
    }
    a.extend(["-nh", "-nohold", "-nc"].iter().map(|s| s.to_string()));
    a.extend(["-fps".into(), cfg.target_fps.to_string()]);
    a.extend(["-vsync".into(), "no".into()]);
    // h265 (HEVC) — this is what unlocks 4K: UxPlay advertises 3840x2160 only when
    // -h265 is on (uxplay.cpp gates the display size on h265_support); h264-only caps
    // the receiver at 1920x1080. h265 was OFF on Linux because this fork SIGABRT'd on
    // h265 RECONNECT — fixed in the fork's renderer reconnect lifecycle: mirror video
    // now always rebuilds the renderers fresh on reconnect (no NULL/dangling
    // renderer_type[] slot from codec-select), and the mirror RTP thread is joined
    // before the rebuild arms (no cross-thread UAF on renderer_type[]).
    if cfg.enable_h265 {
        a.push("-h265".into());
    }
    // Video sink chosen by the caller from the real XVideo port count.
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
    // DEBUG-only, and "video format is ... video WxH" is per-codec.
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

/// Ask the window manager to add/remove `_NET_WM_STATE_FULLSCREEN` on the host window
/// (EWMH). Standard WMs (KDE/GNOME/Xfwm/wlroots…) honour this; the xvimagesink overlay
/// then fills the fullscreen window and XVideo scales the frame to it. No-op without a
/// conforming WM (the window just stays at its fitted size — graceful).
unsafe fn request_fullscreen(display: *mut xlib::Display, window: xlib::Window, on: bool) {
    let net_wm_state =
        xlib::XInternAtom(display, b"_NET_WM_STATE\0".as_ptr() as *const c_char, 0);
    let fullscreen =
        xlib::XInternAtom(display, b"_NET_WM_STATE_FULLSCREEN\0".as_ptr() as *const c_char, 0);
    if net_wm_state == 0 || fullscreen == 0 {
        return;
    }
    let root = xlib::XDefaultRootWindow(display);
    let mut cm: xlib::XClientMessageEvent = std::mem::zeroed();
    cm.type_ = xlib::ClientMessage;
    cm.window = window;
    cm.message_type = net_wm_state;
    cm.format = 32;
    cm.data.set_long(0, if on { 1 } else { 0 }); // 1 = _NET_WM_STATE_ADD, 0 = _REMOVE
    cm.data.set_long(1, fullscreen as c_long);
    cm.data.set_long(3, 1); // source indication: normal application
    let mut ev = xlib::XEvent { client_message: cm };
    xlib::XSendEvent(
        display, root, 0,
        xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask,
        &mut ev,
    );
    xlib::XFlush(display);
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
        // Pick the video sink from the REAL XVideo port count: xvimagesink (hardware
        // overlay, smooth 4K) needs one port per renderer, and the h265 path runs two
        // sinks (h264 + h265) at once. Fall back to software ximagesink — loudly — when
        // there genuinely aren't enough ports (e.g. WSLg's single-port emulated XVideo).
        let xv_ports = if xv_available { xv_image_ports(display, window) } else { 0 };
        let needed_ports = 1 + u32::from(cfg.enable_h265); // (+coverart, off on Linux)
        let use_xv = xv_available && xv_ports >= needed_ports;
        let video_sink: &str = if use_xv { "xvimagesink" } else { "ximagesink" };
        if xv_available && !use_xv {
            eprintln!("[engine] XVideo: {} image port(s) available, need {} for the \
                       h264+h265 renderers -> falling back to software ximagesink \
                       (4K may stutter)", xv_ports, needed_ports);
        }
        eprintln!("[engine] X11 screen {}x{}, XVideo={} ({} ports, sink={}), {}",
            xlib::XDisplayWidth(display, screen), xlib::XDisplayHeight(display, screen),
            xv_available, xv_ports, video_sink,
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
        xlib::XSelectInput(display, window, xlib::StructureNotifyMask | xlib::KeyPressMask);
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
        let _ = ap.set_options(&build_options(&cfg, video_sink));
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
        // Fullscreen state (default from config; Alt+Enter toggles, Esc exits). When
        // fullscreen, the WM owns the window size, so the aspect-fit resize is skipped.
        let mut is_fullscreen = cfg.fullscreen;
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
                } else if ev.get_type() == xlib::KeyPress {
                    // Alt+Enter toggles fullscreen; Esc leaves it (parity with Windows).
                    const XK_RETURN: xlib::KeySym = 0xff0d;
                    const XK_ESCAPE: xlib::KeySym = 0xff1b;
                    let mut ke = ev.key;
                    let keysym = xlib::XLookupKeysym(&mut ke, 0);
                    let alt = (ke.state & xlib::Mod1Mask) != 0;
                    if (keysym == XK_RETURN && alt) || (keysym == XK_ESCAPE && is_fullscreen) {
                        is_fullscreen = if keysym == XK_ESCAPE { false } else { !is_fullscreen };
                        request_fullscreen(display, window, is_fullscreen);
                        if !is_fullscreen {
                            // Force the aspect-fit resize to re-run next iteration.
                            last_resize = last_resize.wrapping_sub(1);
                        }
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
                    if is_fullscreen {
                        request_fullscreen(display, window, true);
                    }
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
                if packed != 0 && !is_fullscreen {
                    let vw = (packed >> 32) as i64;
                    let vh = (packed & 0xffff_ffff) as i64;
                    if vw > 0 && vh > 0 {
                        let sw = xlib::XDisplayWidth(display, screen) as i64;
                        let sh = xlib::XDisplayHeight(display, screen) as i64;
                        let maxw = (sw * 85 / 100).max(160);
                        let maxh = (sh * 85 / 100).max(120);
                        // Present the source at (near) its NATIVE pixel size, only
                        // SHRINKING to fit ~85% of the screen — never blowing a modest
                        // ~720p mirror up to a huge window. That upscale is the HiDPI
                        // blur: video is fixed-pixel content, so enlarging past native
                        // always interpolates (mpv's hidpi-window-scale=no rationale).
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
