//! Safe wrapper over `uxplay-core.dll` (Plan B AirPlay engine).
//!
//! ```no_run
//! use airplay_lib::AirPlay;
//! let mut ap = AirPlay::load("uxplay-core.dll")?;
//! ap.set_device_name("Popyachsa TV")?;
//! ap.set_window(my_hwnd as *mut _)?;          // render into OUR window
//! ap.set_options("-vs d3d11videosink -vd nvh264dec -fps 60 -nh -nohold")?;
//! ap.start()?;
//! // ... pump the window's message loop ...
//! ap.stop();                                  // (also runs on Drop)
//! # anyhow::Ok(())
//! ```

use std::ffi::{c_void, CString};

use airplay_lib_sys::{AirplayCore, Api, LogCb};
use anyhow::{anyhow, bail, Result};

/// Owns the loaded engine + its handle. `stop()`+`destroy()` run on `Drop`.
pub struct AirPlay {
    api: Api,
    handle: *mut AirplayCore,
}

// SAFETY: `AirPlay` is a single exclusive owner of its engine handle — there is no
// shared/aliased access to `handle`. Moving the whole `AirPlay` to another thread
// (e.g. to call `stop()`, which joins the engine's internal worker, off the UI
// thread) is sound as long as only one thread owns it at a time, which Rust's move
// semantics guarantee. The engine's own worker thread lives inside the C library
// and never touches this Rust struct. Not `Sync`: concurrent `&self`/`&mut self`
// from multiple threads is NOT allowed.
unsafe impl Send for AirPlay {}

impl AirPlay {
    /// Load `uxplay-core.dll` from `dll_path` and create an engine instance.
    pub fn load(dll_path: &str) -> Result<AirPlay> {
        unsafe {
            let api = Api::load(dll_path).map_err(|e| anyhow!("loading {dll_path}: {e}"))?;
            let handle = (api.create)();
            if handle.is_null() {
                bail!("airplay_core_create() returned null");
            }
            Ok(AirPlay { api, handle })
        }
    }

    /// Set the renderer host window (HWND). Pass the window WE own; the engine
    /// renders into it instead of creating its own. Call before [`start`].
    pub fn set_window(&mut self, hwnd: *mut c_void) -> Result<()> {
        let rc = unsafe { (self.api.set_window)(self.handle, hwnd) };
        if rc != 0 {
            bail!("airplay_core_set_window failed ({rc})");
        }
        Ok(())
    }

    /// Name shown in the iPhone/Mac AirPlay picker.
    pub fn set_device_name(&mut self, name: &str) -> Result<()> {
        let c = CString::new(name)?;
        let rc = unsafe { (self.api.set_device_name)(self.handle, c.as_ptr()) };
        if rc != 0 {
            bail!("airplay_core_set_device_name failed ({rc})");
        }
        Ok(())
    }

    /// Extra UxPlay-style argv tail, e.g. `-vs d3d11videosink -vd nvh264dec`.
    pub fn set_options(&mut self, options: &str) -> Result<()> {
        let c = CString::new(options)?;
        let rc = unsafe { (self.api.set_options)(self.handle, c.as_ptr()) };
        if rc != 0 {
            bail!("airplay_core_set_options failed ({rc})");
        }
        Ok(())
    }

    /// Register a log callback that receives every UxPlay log line (used to
    /// detect connection markers). Call before [`start`]. `user` is passed back
    /// to `cb` verbatim; it must stay valid for the engine's lifetime.
    pub fn set_log_callback(&mut self, cb: LogCb, user: *mut c_void) {
        unsafe { (self.api.set_log_callback)(self.handle, cb, user) };
    }

    /// Start the engine on its own worker thread; returns immediately.
    pub fn start(&mut self) -> Result<()> {
        let rc = unsafe { (self.api.start)(self.handle) };
        if rc != 0 {
            bail!("airplay_core_start failed ({rc})");
        }
        Ok(())
    }

    /// Stop the engine and join its worker. Idempotent.
    pub fn stop(&mut self) {
        unsafe { (self.api.stop)(self.handle) };
    }
}

impl Drop for AirPlay {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.handle) };
    }
}
