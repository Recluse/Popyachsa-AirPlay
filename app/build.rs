//! Embeds icons/app.ico into the exe as icon resource id "1" so Explorer, the
//! taskbar, and our host window (via LoadIconW(hinstance, 1) in engine.rs) all
//! show the app icon.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("icons/app.ico", "1");
        // Embed an explicit asInvoker manifest. CRITICAL for `updater.exe`: Windows'
        // UAC "installer detection" heuristic auto-elevates any exe whose NAME contains
        // update/setup/install/patch UNLESS it carries an explicit requestedExecutionLevel.
        // Without it, updater.exe pops a UAC prompt every run and — if the user elevates —
        // writes admin-owned files into the per-user install dir, which then blocks every
        // future (asInvoker) self-update. This is THE reason "Windows wouldn't update".
        res.set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#,
        );
        if let Err(e) = res.compile() {
            // Non-fatal: the app still builds, just without the embedded icon/manifest.
            println!("cargo:warning=winresource failed to embed resources: {e}");
        }
    }
    println!("cargo:rerun-if-changed=icons/app.ico");
}
