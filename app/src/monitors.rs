//! Multi-monitor discovery + lookup.
//!
//! Cross-platform surface; enumeration is Windows-only for now
//! (`EnumDisplayMonitors`). Other platforms return an empty list — the Settings
//! "Display" dropdown then simply doesn't show, and the host window falls back
//! to default placement. Linux X11 (RandR/Xinerama) + Wayland enumeration = L3+.

/// A monitor rect in virtual-screen pixel coords. Platform-neutral (avoids the
/// Win32 `RECT` so the type is usable on every OS).
#[derive(Clone, Debug, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Debug)]
pub struct Monitor {
    /// Zero-based index in enumeration order. We persist this (not a native
    /// handle — those are not stable across reboots) as the config identifier.
    pub index: u32,
    /// True if this is the system's primary display.
    pub primary: bool,
    /// Monitor rect in virtual-screen coords.
    pub rect: Rect,
}

impl Monitor {
    pub fn width(&self) -> i32 { self.rect.right - self.rect.left }
    pub fn height(&self) -> i32 { self.rect.bottom - self.rect.top }
}

#[cfg(windows)]
mod sys {
    use super::{Monitor, Rect};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };

    struct EnumCtx { v: Vec<Monitor>, idx: u32 }

    unsafe extern "system" fn enum_cb(
        hmon: HMONITOR, _hdc: HDC, _rect: *mut RECT, lparam: LPARAM,
    ) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut EnumCtx);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
            let r = info.monitorInfo.rcMonitor;
            ctx.v.push(Monitor {
                index: ctx.idx,
                primary: (info.monitorInfo.dwFlags & 1) != 0, // MONITORINFOF_PRIMARY = 1
                rect: Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom },
            });
            ctx.idx += 1;
        }
        BOOL(1)
    }

    pub fn list() -> Vec<Monitor> {
        let mut ctx = EnumCtx { v: Vec::new(), idx: 0 };
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(enum_cb),
                                        LPARAM(&mut ctx as *mut _ as isize));
        }
        ctx.v
    }
}

#[cfg(not(windows))]
mod sys {
    use super::Monitor;
    /// TODO(L3+): X11 (RandR/Xinerama) + Wayland enumeration. Empty for now → the
    /// Settings display dropdown hides and the engine uses default placement.
    pub fn list() -> Vec<Monitor> { Vec::new() }
}

/// Snapshot the currently connected monitors (empty on platforms without
/// enumeration yet).
pub fn list() -> Vec<Monitor> { sys::list() }

/// Resolve a `Config::preferred_monitor` value to an actual `Monitor`, or fall
/// back to the primary (then the first), or `None` if none are enumerated.
pub fn resolve(preferred: Option<u32>) -> Option<Monitor> {
    let mons = list();
    if mons.is_empty() { return None; }
    if let Some(i) = preferred {
        if let Some(m) = mons.iter().find(|m| m.index == i) {
            return Some(m.clone());
        }
    }
    mons.iter().find(|m| m.primary).cloned()
        .or_else(|| mons.into_iter().next())
}
