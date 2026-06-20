//! Autostart toggle — start the app at sign-in.
//!
//! * **Windows:** the per-user `HKCU\…\Run` key (writable without admin, so the
//!   Settings checkbox flips without a UAC prompt).
//! * **Linux / other Unix:** an XDG autostart desktop entry at
//!   `~/.config/autostart/<APP_ID>.desktop` (`X-GNOME-Autostart-enabled=true`).
//!   *(macOS proper wants a LaunchAgent plist — that's M4; the XDG path is a
//!   harmless no-op-ish placeholder there until then.)*
//!
//! [`sync`] reconciles the config's autostart flag with the OS state on launch.

use crate::config::APP_ID;

#[cfg(windows)]
mod imp {
    use super::APP_ID;
    use anyhow::{anyhow, Result};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    fn to_wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn autostart_command() -> Result<String> {
        let exe = std::env::current_exe()?;
        Ok(format!("\"{}\"", exe.display()))
    }

    fn open_run(write: bool) -> Result<HKEY> {
        let mut hkey = HKEY::default();
        let access = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
        let path = to_wide_z(RUN_KEY);
        let r = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, access, &mut hkey)
        };
        if r != ERROR_SUCCESS {
            return Err(anyhow!("RegOpenKeyExW({RUN_KEY}) failed: {:?}", r));
        }
        Ok(hkey)
    }

    pub fn is_enabled() -> bool {
        let Ok(hkey) = open_run(false) else { return false };
        let name = to_wide_z(APP_ID);
        let mut typ = 0u32;
        let mut size = 0u32;
        let r = unsafe {
            RegQueryValueExW(hkey, PCWSTR(name.as_ptr()), None,
                Some(&mut typ as *mut u32 as *mut _), None, Some(&mut size))
        };
        unsafe { let _ = RegCloseKey(hkey); }
        r == ERROR_SUCCESS && size > 0
    }

    pub fn set(enabled: bool) -> Result<()> {
        let hkey = open_run(true)?;
        let name = to_wide_z(APP_ID);
        let r = if enabled {
            let cmd = autostart_command()?;
            let value = to_wide_z(&cmd);
            unsafe {
                RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_SZ,
                    Some(std::slice::from_raw_parts(
                        value.as_ptr() as *const u8, value.len() * std::mem::size_of::<u16>())))
            }
        } else {
            let r = unsafe { RegDeleteValueW(hkey, PCWSTR(name.as_ptr())) };
            if r == WIN32_ERROR(ERROR_FILE_NOT_FOUND.0) { ERROR_SUCCESS } else { r }
        };
        unsafe { let _ = RegCloseKey(hkey); }
        if r == ERROR_SUCCESS { Ok(()) } else { Err(anyhow!("RegSet/Delete failed: {:?}", r)) }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::APP_ID;
    use anyhow::{anyhow, Result};
    use std::path::PathBuf;

    fn desktop_path() -> Result<PathBuf> {
        let dir = dirs::config_dir().ok_or_else(|| anyhow!("no XDG config dir"))?;
        Ok(dir.join("autostart").join(format!("{APP_ID}.desktop")))
    }

    /// Quote an exe path for a Desktop Entry `Exec=` value (the spec requires
    /// quoting paths with spaces/reserved chars; inside double-quotes escape
    /// `"`, backtick, `$`, `\`). Without this an AppImage under `~/Applications`
    /// or `/opt/Popyachsa AirPlay/` breaks autostart.
    fn exec_quote(path: &std::path::Path) -> String {
        let mut out = String::from("\"");
        for c in path.to_string_lossy().chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }

    pub fn is_enabled() -> bool {
        desktop_path().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn set(enabled: bool) -> Result<()> {
        let p = desktop_path()?;
        if enabled {
            let exe = std::env::current_exe()?;
            std::fs::create_dir_all(p.parent().unwrap())?;
            let body = format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=Popyachsa AirPlay\n\
                 Exec={}\n\
                 Terminal=false\n\
                 X-GNOME-Autostart-enabled=true\n",
                exec_quote(&exe)
            );
            std::fs::write(&p, body)?;
        } else if p.exists() {
            std::fs::remove_file(&p)?;
        }
        Ok(())
    }
}

pub use imp::{is_enabled, set};

/// Reconcile the OS autostart state with the desired (config) flag on launch.
/// When enabled we ALWAYS (re)write the entry so the stored command tracks the
/// current exe path after a move/reinstall — `is_enabled()` only checks existence,
/// not that the recorded path still points at the running binary.
pub fn sync(desired: bool) {
    let r = if desired { set(true) } else if is_enabled() { set(false) } else { Ok(()) };
    if let Err(e) = r {
        eprintln!("[autostart] {e}");
    }
}
