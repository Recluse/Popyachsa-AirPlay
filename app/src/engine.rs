//! In-process AirPlay engine + host window (Plan B, stage B4).
//!
//! Replaces the old `uxplay.rs` subprocess model. Instead of spawning
//! `uxplay.exe` and fighting its `d3d11videosink` window across a process
//! boundary, we now:
//!
//! 1. spawn a dedicated **host-window thread** that creates a top-level window
//!    WE own (`CreateWindowExW` + our [`wndproc`]),
//! 2. load `uxplay-core.dll` (via `airplay-lib`) and hand it our HWND, so the
//!    engine renders the AirPlay mirror straight into our window in-process,
//!    and
//! 3. run that window's Win32 message pump on the same thread.
//!
//! The tray (main) thread controls the engine through [`Engine`]: `start`,
//! `stop`, `restart`, `set_topmost`. Stop is a `WM_CLOSE` PostMessage +
//! thread join; the pump exits, the engine is stopped and dropped on its own
//! thread (important — GStreamer/d3d11 want consistent thread affinity).
//!
//! Window *chrome* (borderless / drag / aspect-resize / fullscreen / snap) is
//! stage B5; B4 is a plain resizable window that proves the integration.

use std::ffi::{c_void, CStr};
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use airplay_lib::AirPlay;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetStockObject, MonitorFromWindow, BLACK_BRUSH, HBRUSH, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetFocus, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, KillTimer, LoadCursorW, LoadIconW, PostMessageW, PostQuitMessage,
    RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow, SetTimer, HTBOTTOM, HTBOTTOMLEFT,
    HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IDC_SIZENESW,
    IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCCALCSIZE,
    WM_NCLBUTTONDOWN, WM_SIZING, WMSZ_BOTTOM, WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT,
    WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWLP_USERDATA, GWL_STYLE, HWND_NOTOPMOST, HWND_TOPMOST, HWND_TOP, IDC_ARROW, MSG,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::config::Config;
use crate::monitors;
use crate::status::Status;

// Posted from the log callback (engine thread) to the host-window thread so all
// window ops stay on the owning thread.
const WM_APP_CONNECTED: u32 = WM_APP + 1;
const WM_APP_DISCONNECTED: u32 = WM_APP + 2;
const WM_APP_RESIZE: u32 = WM_APP + 3; // new video size -> fit window to aspect

// Resize grab border thickness (px) for edge-resize over the borderless window.
const RESIZE_BORDER: i32 = 8;

// Connection markers in UxPlay's log stream. NOTE: this UxPlay build does NOT
// emit "raop_handler_teardown"; the reliable "last device gone" signal is
// "Open connections: 0" (verified from the live log). "Open connections: 1"
// etc. must NOT match, so we key on the exact ": 0" suffix.
const MARK_CONNECTED: &str = "Begin streaming";
const MARK_TEARDOWN: &str = "Open connections: 0";

// Watchdog timer: UxPlay's reconnect loop re-inits the video pipeline after a
// disconnect and the sink re-shows our window (at PLAYING, AFTER the bind — so
// re-hiding at bind-time is too early). A periodic timer enforces "hidden while
// no device is connected" regardless of when the sink re-shows.
const HIDE_TIMER_ID: usize = 1;

// Fullscreen keys handled in the message pump (focus-scoped, so they only fire
// when our video window has focus). VK_F = 0x46, VK_ESCAPE = 0x1B, VK_RETURN =
// 0x0D (Alt+Enter arrives as WM_SYSKEYDOWN/VK_RETURN).
const VK_F: u32 = 0x46;
const VK_ESC: u32 = 0x1B;
const VK_ENTER: u32 = 0x0D;

// Shared with the C log callback (engine is single-instance).
static CB_HWND: AtomicIsize = AtomicIsize::new(0);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static STATUS_TX: Mutex<Option<Sender<Status>>> = Mutex::new(None);
// Current video frame size (w<<32 | h), 0 = unknown. Parsed from the engine's
// "video_renderer_size: WxH" log line; used to fit the window to the content
// aspect (no black bars) and to lock aspect during manual resize.
static ASPECT_WH: AtomicU64 = AtomicU64::new(0);

/// Main installs a sender so connection transitions reach the tray icon.
pub fn install_status_sender(tx: Sender<Status>) {
    // Poison-tolerant: a panic in another holder must not wedge status delivery.
    *STATUS_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
}

fn send_status(s: Status) {
    if let Some(tx) = STATUS_TX.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(s);
    }
}

/// Engine log directory: `logs/` *next to the exe* (relative, so it works in any
/// install folder and in the portable zip). The engine's stdout+stderr (UxPlay,
/// the dnssd shim, GStreamer) are redirected here at startup — see
/// `main::redirect_stdio_to_log`.
pub fn engine_log_dir() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("logs")
}

/// Receives every UxPlay log line (on the engine's thread). Detects connect /
/// disconnect markers and posts to the host-window thread to show/hide + update
/// the tray. Posting (not acting directly) keeps window ops on the owning thread.
/// (File logging is handled by the stdout/stderr redirect, which also captures
/// the dnssd shim + GStreamer output that never reaches this callback.)
extern "C" fn engine_log_cb(_level: c_int, msg: *const c_char, _user: *mut c_void) {
    // The C engine calls this on its own thread — a panic must never unwind across
    // the FFI boundary into C (UB). Contain it (the body is panic-free today; cheap
    // insurance for future edits / debug builds where panics unwind, not abort).
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    if msg.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let hwnd_raw = CB_HWND.load(Ordering::SeqCst);
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut c_void);
    if text.contains(MARK_CONNECTED) {
        if !CONNECTED.swap(true, Ordering::SeqCst) {
            unsafe { let _ = PostMessageW(Some(hwnd), WM_APP_CONNECTED, WPARAM(0), LPARAM(0)); }
        }
    } else if text.contains(MARK_TEARDOWN) && CONNECTED.swap(false, Ordering::SeqCst) {
        unsafe { let _ = PostMessageW(Some(hwnd), WM_APP_DISCONNECTED, WPARAM(0), LPARAM(0)); }
    }
    // Video frame size -> fit the window to the content aspect (no black bars).
    // UxPlay logs "begin video stream wxh = WxH; ..." at every stream start
    // (the "video_renderer_size:" line only fires on rotation).
    if let Some(rest) = text.split("video stream wxh = ").nth(1) {
        if let Some((w, h)) = parse_wxh(rest) {
            let packed = ((w as u64) << 32) | h as u64;
            if ASPECT_WH.swap(packed, Ordering::SeqCst) != packed {
                unsafe { let _ = PostMessageW(Some(hwnd), WM_APP_RESIZE, WPARAM(0), LPARAM(0)); }
            }
        }
    }
    })); // end catch_unwind
}

const HOST_CLASS: windows::core::PCWSTR = w!("PopyachsaAirPlayHostWindow");
const HOST_TITLE: windows::core::PCWSTR = w!("Popyachsa AirPlay");

/// Resolve `uxplay-core.dll`: next to our exe (the dist layout), else fall back to
/// a bare name and let the loader search path (PATH / the app dir) find it.
fn core_dll_path() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let c = dir.join("uxplay-core.dll");
            if c.exists() {
                return c.to_string_lossy().into_owned();
            }
        }
    }
    "uxplay-core.dll".to_string()
}

/// Build the UxPlay option tail for the DLL (no `-n`: device name is passed via
/// `set_device_name`; no `-fs`: we own fullscreen ourselves in B5). Mirrors the
/// tuned flags from the old `uxplay.rs::build_argv`, but tells the videosink
/// NOT to handle fullscreen itself (`fullscreen-toggle-mode=none`).
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
    // Hardware H.264 decoder (the render sink stays d3d11videosink regardless;
    // GStreamer bridges decoder->sink memory). D3D11/D3D12 work on AMD/Intel too.
    let h264_dec = match cfg.video_decoder.as_str() {
        "d3d12" => "d3d12h264dec",
        "nvidia" => "nvh264dec",
        _ => "d3d11h264dec",
    };
    a.extend(["-vd".into(), h264_dec.to_string()]);
    // We own the window + fullscreen now → fullscreen-toggle-mode=none so the
    // sink never double-toggles against our WndProc (research finding).
    // The -vs value is ONE argv token with spaces — quote it so the DLL's
    // quote-aware splitter keeps it whole (see airplay_core.cpp split_args).
    a.extend([
        "-vs".into(),
        "\"d3d11videosink fullscreen-toggle-mode=none \
          processing-deadline=0 qos=TRUE max-lateness=20000000\""
            .into(),
    ]);
    if !cfg.audio_sink.is_empty() {
        a.extend(["-as".into(), cfg.audio_sink.clone()]);
    }
    a.push("-FPSdata".into());
    if !cfg.custom_flags.trim().is_empty() {
        for tok in cfg.custom_flags.split_whitespace() {
            a.push(tok.to_string());
        }
    }
    a.join(" ")
}

/// Per-window state stored in GWLP_USERDATA so the WndProc can toggle
/// fullscreen. (B5.1: fullscreen only; B5.2 adds borderless/aspect fields.)
struct WinState {
    is_fullscreen: bool,
    want_fullscreen: bool,   // from cfg; applied when the window is shown on connect
    borderless: bool,        // from cfg; strip the frame/title bar in windowed mode
    saved_style: isize,      // GWL_STYLE before going fullscreen
    saved_rect: RECT,        // window rect before going fullscreen
    last_lbtn_down_ms: u32,  // GetTickCount of the last LMB-down (dbl-click synth)
}

/// Own fullscreen toggle (Raymond Chen recipe). The sink is launched with
/// `fullscreen-toggle-mode=none`, so WE are the only one toggling — no fight.
/// Borderless-fullscreen onto the window's current monitor; restores the exact
/// previous windowed rect + style on the way out.
fn toggle_fullscreen(hwnd: HWND, st: &mut WinState) {
    unsafe {
        if !st.is_fullscreen {
            st.saved_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let mut wr = RECT::default();
            let _ = GetWindowRect(hwnd, &mut wr);
            st.saved_rect = wr;

            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return;
            }
            // Strip the frame (keep visible) and cover the monitor.
            let fs_style = (st.saved_style & !(WS_OVERLAPPEDWINDOW.0 as isize)) | (WS_VISIBLE.0 as isize);
            SetWindowLongPtrW(hwnd, GWL_STYLE, fs_style);
            let r = mi.rcMonitor;
            let _ = SetWindowPos(
                hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top,
                SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
            );
            st.is_fullscreen = true;
        } else {
            SetWindowLongPtrW(hwnd, GWL_STYLE, st.saved_style);
            let r = st.saved_rect;
            let _ = SetWindowPos(
                hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top,
                SWP_FRAMECHANGED | SWP_NOOWNERZORDER | SWP_NOZORDER,
            );
            st.is_fullscreen = false;
            // Fit to the video aspect so the windowed view has no black bars.
            fit_window_to_aspect(hwnd, st);
        }
    }
}

/// WndProc for the host window. B5.1: teardown + Alt+Enter fullscreen.
extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Mouse over the video lands HERE: d3d11videosink forwards the GSTD3D11
        // child's clicks to us via a cross-thread SendMessage = a NONQUEUED
        // message delivered straight to this WndProc (never our GetMessage pump).
        // Keep these handlers fast — the GStreamer thread is blocked inside that
        // SendMessage until we return.
        WM_LBUTTONDOWN => unsafe {
            // The GSTD3D11 child has no CS_DBLCLKS, so WM_LBUTTONDBLCLK is never
            // produced — synthesize it: two downs within GetDoubleClickTime()
            // toggle fullscreen; a lone down (windowed) starts a drag-from-
            // anywhere move-loop. (The harmless zero-move drag from the 1st click
            // of a pair is fine.)
            let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;
            if !st.is_null() {
                let now = GetTickCount();
                let dbl = now.wrapping_sub((*st).last_lbtn_down_ms) <= GetDoubleClickTime();
                (*st).last_lbtn_down_ms = now;
                if dbl {
                    toggle_fullscreen(hwnd, &mut *st);
                } else if !(*st).is_fullscreen {
                    // Near an edge -> start the OS resize loop; interior -> move.
                    let x = (lparam.0 & 0xffff) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let ht = hit_edge(x, y, cr.right, cr.bottom);
                    let _ = ReleaseCapture();
                    let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(ht as usize)), Some(LPARAM(0)));
                }
            }
            LRESULT(0)
        },
        // Show a resize cursor near the borderless window edges (the child
        // window would otherwise keep the arrow). Forwarded WM_MOUSEMOVE.
        WM_MOUSEMOVE => unsafe {
            let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;
            if !st.is_null() && (*st).borderless && !(*st).is_fullscreen {
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                let mut cr = RECT::default();
                let _ = GetClientRect(hwnd, &mut cr);
                let idc = match hit_edge(x, y, cr.right, cr.bottom) {
                    HTLEFT | HTRIGHT => IDC_SIZEWE,
                    HTTOP | HTBOTTOM => IDC_SIZENS,
                    HTTOPLEFT | HTBOTTOMRIGHT => IDC_SIZENWSE,
                    HTTOPRIGHT | HTBOTTOMLEFT => IDC_SIZENESW,
                    _ => IDC_ARROW,
                };
                if let Ok(cur) = LoadCursorW(None, idc) {
                    let _ = SetCursor(Some(cur));
                }
            }
            LRESULT(0)
        },
        // Aspect-lock during manual resize: keep the window at the video aspect
        // so no black bars ever appear. Adjust the proposed RECT in place.
        WM_SIZING => unsafe {
            // Keep the resize cursor during the drag (the child window would
            // otherwise revert it to an arrow).
            let idc = match wparam.0 as u32 {
                WMSZ_LEFT | WMSZ_RIGHT => IDC_SIZEWE,
                WMSZ_TOP | WMSZ_BOTTOM => IDC_SIZENS,
                WMSZ_TOPLEFT | WMSZ_BOTTOMRIGHT => IDC_SIZENWSE,
                WMSZ_TOPRIGHT | WMSZ_BOTTOMLEFT => IDC_SIZENESW,
                _ => IDC_ARROW,
            };
            if let Ok(cur) = LoadCursorW(None, idc) {
                let _ = SetCursor(Some(cur));
            }
            let packed = ASPECT_WH.load(Ordering::SeqCst);
            let r = lparam.0 as *mut RECT;
            if packed != 0 && !r.is_null() {
                let vw = (packed >> 32) as i32;
                let vh = (packed & 0xffff_ffff) as i32;
                if vw > 0 && vh > 0 {
                    let w = (*r).right - (*r).left;
                    let h = (*r).bottom - (*r).top;
                    let edge = wparam.0 as u32;
                    if edge == WMSZ_TOP || edge == WMSZ_BOTTOM {
                        (*r).right = (*r).left + (h as i64 * vw as i64 / vh as i64) as i32;
                    } else {
                        // left/right/corners: width drives height
                        (*r).bottom = (*r).top + (w as i64 * vh as i64 / vw as i64) as i32;
                    }
                }
            }
            LRESULT(1)
        },
        // Borderless: when on, report the client area as the WHOLE window rect
        // (return 0 to the size-calc request) → no title bar / frame. Drag still
        // works via our WM_LBUTTONDOWN -> HTCAPTION handler.
        WM_NCCALCSIZE if wparam.0 != 0 => unsafe {
            let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;
            if !st.is_null() && (*st).borderless {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        },
        // (Alt+Enter is handled in the pump loop, not here — the sink subclasses
        // this HWND so keystrokes may not reach this proc.)
        // CONSUME WM_CLOSE: just end the pump. We must NOT let DefWindowProc
        // DestroyWindow here — the engine's d3d11videosink is still rendering
        // into this HWND, and destroying it out from under the sink crashes.
        // The thread stops the engine first, THEN destroys the window (below).
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Reliably bring our window to the foreground. A background thread can't just
/// `SetForegroundWindow` (Win32 blocks focus-stealing) — attach to the current
/// foreground thread's input first (the classic dance), then restore.
unsafe fn force_foreground(hwnd: HWND) {
    let fg = GetForegroundWindow();
    let fg_thread = GetWindowThreadProcessId(fg, None);
    let our_thread = GetCurrentThreadId();
    let attached = fg_thread != 0 && fg_thread != our_thread;
    if attached {
        let _ = AttachThreadInput(our_thread, fg_thread, true);
    }
    let _ = BringWindowToTop(hwnd);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
    if attached {
        let _ = AttachThreadInput(our_thread, fg_thread, false);
    }
}

/// Parse the leading "WxH" of an UxPlay `video_renderer_size: WxH rot=...` tail.
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

/// Which resize edge/corner a client-coord point is in (HTCAPTION = interior).
fn hit_edge(x: i32, y: i32, w: i32, h: i32) -> u32 {
    let b = RESIZE_BORDER;
    let (l, r, t, bot) = (x < b, x >= w - b, y < b, y >= h - b);
    if t && l { HTTOPLEFT }
    else if t && r { HTTOPRIGHT }
    else if bot && l { HTBOTTOMLEFT }
    else if bot && r { HTBOTTOMRIGHT }
    else if l { HTLEFT }
    else if r { HTRIGHT }
    else if t { HTTOP }
    else if bot { HTBOTTOM }
    else { HTCAPTION }
}

/// Resize the (windowed) window so it matches the current video aspect — no
/// black bars (borderless: window rect == client == content). Fits the aspect
/// within ~85% of the monitor work area and centers it.
unsafe fn fit_window_to_aspect(hwnd: HWND, st: &WinState) {
    if st.is_fullscreen {
        return;
    }
    let packed = ASPECT_WH.load(Ordering::SeqCst);
    if packed == 0 {
        return;
    }
    let vw = (packed >> 32) as i32;
    let vh = (packed & 0xffff_ffff) as i32;
    if vw <= 0 || vh <= 0 {
        return;
    }
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(mon, &mut mi).as_bool() {
        return;
    }
    let aw = mi.rcWork.right - mi.rcWork.left;
    let ah = mi.rcWork.bottom - mi.rcWork.top;
    let maxw = (aw as f64 * 0.85) as i32;
    let maxh = (ah as f64 * 0.85) as i32;
    // Fit vw:vh inside maxw x maxh.
    let mut w = maxw;
    let mut h = (w as i64 * vh as i64 / vw as i64) as i32;
    if h > maxh {
        h = maxh;
        w = (h as i64 * vw as i64 / vh as i64) as i32;
    }
    let x = mi.rcWork.left + (aw - w) / 2;
    let y = mi.rcWork.top + (ah - h) / 2;
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
}

fn set_topmost_hwnd(hwnd: HWND, on: bool) {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Body of the host-window thread: create window, start engine into it, pump.
fn run_host_window(cfg: Config, hwnd_out: Arc<AtomicIsize>, running: Arc<AtomicBool>) {
    unsafe {
        let hinstance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[engine] GetModuleHandleW: {e}");
                running.store(false, Ordering::SeqCst);
                return;
            }
        };

        // Register the class once per process; a second RegisterClassW returns 0
        // (already registered) which is harmless.
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: HOST_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // App icon (embedded as resource id 1 by build.rs) -> taskbar icon.
            hIcon: LoadIconW(Some(hinstance.into()), PCWSTR(1 as *const u16)).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            HOST_CLASS,
            HOST_TITLE,
            // Created HIDDEN (no WS_VISIBLE): the engine just advertises; the
            // window is shown only when a device actually connects.
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            720,
            None,
            None,
            Some(hinstance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[engine] CreateWindowExW: {e}");
                running.store(false, Ordering::SeqCst);
                return;
            }
        };
        hwnd_out.store(hwnd.0 as isize, Ordering::SeqCst);
        CB_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        CONNECTED.store(false, Ordering::SeqCst);
        if cfg.always_on_top {
            set_topmost_hwnd(hwnd, true);
        }

        // B5.1: place the windowed rect (centered) on the preferred monitor.
        if let Some(mon) = monitors::resolve(cfg.preferred_monitor) {
            let (ww, wh) = (1280, 720);
            let x = mon.rect.left + (mon.width() - ww).max(0) / 2;
            let y = mon.rect.top + (mon.height() - wh).max(0) / 2;
            let _ = SetWindowPos(hwnd, None, x, y, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
        }

        // Per-window state + the Alt+Enter fullscreen hotkey (fires regardless
        // of the GSTD3D11 child window's focus).
        let state = Box::into_raw(Box::new(WinState {
            is_fullscreen: false,
            want_fullscreen: cfg.fullscreen,
            borderless: cfg.borderless,
            saved_style: 0,
            saved_rect: RECT::default(),
            last_lbtn_down_ms: 0,
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        // Apply the borderless frame decision now (the WM_NCCALCSIZE handler
        // reads WinState.borderless; SWP_FRAMECHANGED forces a frame recalc).
        if cfg.borderless {
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0,
                                 SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
        // Alt+Enter is NOT a global hotkey: RegisterHotKey grabs it system-wide
        // for the whole time the engine runs (even with no video window shown),
        // breaking Alt+Enter everywhere else. It's handled instead as a normal
        // keystroke in the message pump below, so it only fires when our video
        // window has focus. (Fullscreen is applied when the window is shown on
        // connect, not now.)

        // Watchdog timer that keeps the window hidden while no device is
        // connected (defeats the sink re-showing it during reconnect re-init).
        let _ = SetTimer(Some(hwnd), HIDE_TIMER_ID, 300, None);

        // Bring up the engine, rendering into our window.
        let dll = core_dll_path();
        let mut ap = match AirPlay::load(&dll) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[engine] loading {dll}: {e}");
                hwnd_out.store(0, Ordering::SeqCst);
                running.store(false, Ordering::SeqCst);
                return;
            }
        };
        ap.set_log_callback(engine_log_cb, std::ptr::null_mut());
        let _ = ap.set_device_name(&cfg.device_name);
        let _ = ap.set_window(hwnd.0 as *mut c_void);
        let _ = ap.set_options(&build_options(&cfg));
        if let Err(e) = ap.start() {
            eprintln!("[engine] start: {e}");
        }
        running.store(true, Ordering::SeqCst);
        // Engine is up and advertising (no device yet) -> show the "Ready" state in
        // the tray. Without this, an autostarted engine leaves the icon on "Off"
        // until a device connects (the only later events are Connected/Ready-on-
        // disconnect/Off-on-teardown).
        send_status(Status::Ready);
        eprintln!("[engine] host window {:?} up; engine started", hwnd.0);

        // Win32 message pump — runs until WM_CLOSE -> PostQuitMessage.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Alt+Enter toggles fullscreen — handled HERE, as a normal keystroke
            // in the pump, NOT via RegisterHotKey (which would steal Alt+Enter
            // system-wide) and NOT in the WndProc (the d3d11videosink subclasses
            // our HWND). Plain Alt+Enter (Alt down, Ctrl up) arrives as
            // WM_SYSKEYDOWN/VK_RETURN. Keyboard input goes only to the focused
            // window of OUR thread, so this fires only when the video window has
            // focus. Swallow ONLY VK_RETURN — other Alt-combos (Alt+F4, Alt+Space)
            // must fall through to DefWindowProc.
            if msg.message == WM_SYSKEYDOWN && (msg.wParam.0 as u32) == VK_ENTER {
                toggle_fullscreen(hwnd, &mut *state);
                continue;
            }
            // F toggles / Esc exits fullscreen. Also AltGr+Enter (= Ctrl+Alt+Enter
            // on many EU layouts): holding Ctrl reclassifies the keystroke as
            // WM_KEYDOWN (not WM_SYSKEYDOWN), so catch VK_RETURN here too when Alt
            // is down. All focus-scoped (keyboard goes to our thread's focused
            // window). Plain Enter (no Alt) falls through untouched. NOTE: mouse
            // double-click can't be caught here — it goes to the GSTD3D11 child
            // (GStreamer-owned, cross-thread), so F / Esc / Alt+Enter cover
            // fullscreen instead.
            if msg.message == WM_KEYDOWN {
                let vk = msg.wParam.0 as u32;
                let alt_down = GetKeyState(VK_MENU.0 as i32) < 0;
                if vk == VK_F
                    || (vk == VK_ESC && (*state).is_fullscreen)
                    || (vk == VK_ENTER && alt_down)
                {
                    toggle_fullscreen(hwnd, &mut *state);
                    continue;
                }
            }
            // Device connected: show the window (+ go fullscreen if configured)
            // and turn the tray icon green.
            if msg.message == WM_APP_CONNECTED {
                let _ = ShowWindow(hwnd, SW_SHOW);
                if (*state).want_fullscreen && !(*state).is_fullscreen {
                    toggle_fullscreen(hwnd, &mut *state);
                }
                force_foreground(hwnd);
                send_status(Status::Connected);
                continue;
            }
            // Device disconnected: hide the window, back to "ready".
            if msg.message == WM_APP_DISCONNECTED {
                let _ = ShowWindow(hwnd, SW_HIDE);
                send_status(Status::Ready);
                continue;
            }
            // New video size: fit the (windowed) window to the content aspect.
            if msg.message == WM_APP_RESIZE {
                fit_window_to_aspect(hwnd, &*state);
                continue;
            }
            if msg.message == WM_TIMER && msg.wParam.0 == HIDE_TIMER_ID {
                // Keep the window hidden while idle (the sink re-shows it during
                // UxPlay's reconnect re-init).
                if !CONNECTED.load(Ordering::SeqCst) && IsWindowVisible(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // ORDER MATTERS: stop the engine FIRST so the d3d11videosink releases
        // this HWND, THEN destroy the window. Destroying it first (e.g. via a
        // WM_CLOSE->DefWindowProc->DestroyWindow) while a stream is live
        // segfaults the sink.
        eprintln!("[engine] pump exited; stopping engine, then destroying window");
        // Stop the log callback from posting to a window we're tearing down.
        CB_HWND.store(0, Ordering::SeqCst);
        CONNECTED.store(false, Ordering::SeqCst);
        let _ = KillTimer(Some(hwnd), HIDE_TIMER_ID);
        let st = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        if !st.is_null() {
            drop(Box::from_raw(st));
        }
        ap.stop();
        let _ = DestroyWindow(hwnd);
        // `ap` drops here -> airplay_core_destroy, on this same thread.
        running.store(false, Ordering::SeqCst);
        hwnd_out.store(0, Ordering::SeqCst);
        // The pump can exit on a SELF-close (user closed the video window, or a
        // sink-driven quit) — not just our own stop(). Tell the tray so it joins
        // the worker + rebuilds the menu (Start instead of Stop/Restart); without
        // this the menu desyncs and the JoinHandle leaks. Harmless on the
        // stop()-initiated path (the handler just rebuilds an already-correct menu).
        send_status(Status::Off);
    }
}

/// Tray-side handle to the in-process engine + its host window.
pub struct Engine {
    hwnd: Arc<AtomicIsize>,
    running: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            hwnd: Arc::new(AtomicIsize::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn start(&self, cfg: &Config) -> anyhow::Result<()> {
        // Claim the running slot SYNCHRONOUSLY: a racing/duplicate start (e.g.
        // autostart + a fast tray "Start" click) must become a no-op, not spawn
        // a second engine on the singleton DLL (two RAOP servers on one port,
        // two GStreamer graphs -> crash). The window thread clears the flag when
        // it exits.
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Join any finished thread handle first.
        if let Some(t) = self.thread.lock().unwrap().take() {
            let _ = t.join();
        }
        let cfg = cfg.clone();
        let hwnd = self.hwnd.clone();
        let running = self.running.clone();
        match std::thread::Builder::new()
            .name("airplay-host-window".into())
            .spawn(move || run_host_window(cfg, hwnd, running))
        {
            Ok(handle) => {
                *self.thread.lock().unwrap() = Some(handle);
                Ok(())
            }
            Err(e) => {
                self.running.store(false, Ordering::SeqCst);
                Err(e.into())
            }
        }
    }

    pub fn stop(&self) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(h as *mut c_void)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
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
        self.stop();
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.start(cfg)
    }

    /// Live always-on-top toggle (cross-thread `SetWindowPos` is allowed).
    pub fn set_topmost(&self, on: bool) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            set_topmost_hwnd(HWND(h as *mut c_void), on);
        }
    }
}
