//! Raw FFI bindings to `uxplay-core.dll` — the embeddable UxPlay AirPlay engine
//! (Plan B). The DLL exports a flat C ABI (see `lib/airplay_core.h` in the
//! UxPlay fork); we load it at runtime and resolve the 8 functions by name.
//!
//! Loading at runtime (instead of linking a `.dll.a`/`.lib`) is intentional: it
//! lets an MSVC-built host drive the MinGW-built DLL without any import-library
//! or compiler-ABI friction. The function pointers point into the loaded DLL;
//! we keep the `Library` alive inside [`Api`] so they stay valid.

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};

/// Opaque engine handle (`airplay_core_t *`).
pub type AirplayCore = c_void;

pub type LogCb = extern "C" fn(level: c_int, msg: *const c_char, user: *mut c_void);

type FnCreate = unsafe extern "C" fn() -> *mut AirplayCore;
type FnSetWindow = unsafe extern "C" fn(*mut AirplayCore, *mut c_void) -> c_int;
type FnSetStr = unsafe extern "C" fn(*mut AirplayCore, *const c_char) -> c_int;
type FnSetLogCb = unsafe extern "C" fn(*mut AirplayCore, LogCb, *mut c_void);
type FnStart = unsafe extern "C" fn(*mut AirplayCore) -> c_int;
type FnVoid = unsafe extern "C" fn(*mut AirplayCore);

/// All resolved entry points + the owning `Library` (keeps the DLL mapped).
pub struct Api {
    _lib: libloading::Library,
    pub create: FnCreate,
    pub set_window: FnSetWindow,
    pub set_device_name: FnSetStr,
    pub set_options: FnSetStr,
    pub set_log_callback: FnSetLogCb,
    pub start: FnStart,
    pub stop: FnVoid,
    pub destroy: FnVoid,
}

impl Api {
    /// Load `uxplay-core.dll` from `path` and resolve all exports.
    ///
    /// # Safety
    /// Loads an arbitrary DLL and trusts its exports to match these signatures.
    pub unsafe fn load(path: &str) -> Result<Api, libloading::Error> {
        let lib = libloading::Library::new(path)?;
        // `*symbol` copies out the raw function pointer; it stays valid as long
        // as `_lib` keeps the DLL loaded (same struct lifetime).
        let create = *lib.get::<FnCreate>(b"airplay_core_create\0")?;
        let set_window = *lib.get::<FnSetWindow>(b"airplay_core_set_window\0")?;
        let set_device_name = *lib.get::<FnSetStr>(b"airplay_core_set_device_name\0")?;
        let set_options = *lib.get::<FnSetStr>(b"airplay_core_set_options\0")?;
        let set_log_callback = *lib.get::<FnSetLogCb>(b"airplay_core_set_log_callback\0")?;
        let start = *lib.get::<FnStart>(b"airplay_core_start\0")?;
        let stop = *lib.get::<FnVoid>(b"airplay_core_stop\0")?;
        let destroy = *lib.get::<FnVoid>(b"airplay_core_destroy\0")?;
        Ok(Api {
            _lib: lib,
            create,
            set_window,
            set_device_name,
            set_options,
            set_log_callback,
            start,
            stop,
            destroy,
        })
    }
}
