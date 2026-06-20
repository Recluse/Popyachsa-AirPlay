//! Embeds icons/app.ico into the exe as icon resource id "1" so Explorer, the
//! taskbar, and our host window (via LoadIconW(hinstance, 1) in engine.rs) all
//! show the app icon.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("icons/app.ico", "1");
        if let Err(e) = res.compile() {
            // Non-fatal: the app still builds, just without an embedded icon.
            println!("cargo:warning=winresource failed to embed icon: {e}");
        }
    }
    println!("cargo:rerun-if-changed=icons/app.ico");
}
