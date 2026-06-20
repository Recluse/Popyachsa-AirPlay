# macOS build infra

Reproduces `uxplay-core.dylib` on macOS (Apple Silicon). See
[`BUILD-MACOS.md`](BUILD-MACOS.md) for the full end-to-end `.app` + DMG build.

## Toolchain (one-time)

```bash
xcode-select --install                       # Xcode Command Line Tools (clang)
brew install rustup cmake ninja pkg-config openssl@3 libplist
rustup default stable
rustup target add aarch64-apple-darwin x86_64-apple-darwin
# Official UNIVERSAL GStreamer.framework 1.28.4 (runtime + devel), verify SHA256,
# then install to /Library/Frameworks (needs root):
#   https://gstreamer.freedesktop.org/data/pkg/osx/1.28.4/
#     gstreamer-1.0-1.28.4-universal.pkg
#     gstreamer-1.0-devel-1.28.4-universal.pkg
#   sudo installer -pkg <runtime>.pkg -target /
#   sudo installer -pkg <devel>.pkg   -target /
# Verify fat: lipo -info /Library/Frameworks/GStreamer.framework/GStreamer  → x86_64 arm64
```

## Build the core dylib

Source tree = FDH2/UxPlay `fc126fd` (v1.73.6+1) + `../../uxplay-patches/clean-full-vs-v1.73.6.diff`
+ `../../uxplay-patches/airplay_core.{h,cpp}` copied into `lib/`
+ `../../uxplay-patches/avsample_sink.m` copied into `renderers/`:

```bash
git clone https://github.com/FDH2/UxPlay.git && cd UxPlay && git checkout fc126fd
git apply /path/to/uxplay-patches/clean-full-vs-v1.73.6.diff
cp /path/to/uxplay-patches/airplay_core.{h,cpp} lib/
cp /path/to/uxplay-patches/avsample_sink.m       renderers/
SRC=$PWD bash /path/to/build/macos/build-core-arm64.sh     # -> build-arm64/uxplay-core.dylib
```

Notes:
- `-DGST_MACOS=OFF` keeps the `gst_macos_main` `main()` wrapper out of the dylib
  (tao owns the NSApplication run loop). The dylib's `main()` is dead code anyway.
- The clean-full diff ALREADY carries the gmainloop fix, the macOS overlay bind +
  the **custom `avlayer` video sink** wiring (`video_renderer.c` appsink path,
  `renderers/CMakeLists.txt` OBJC + AV/CoreMedia/CoreVideo/QuartzCore frameworks), so
  the only extra step is copying the new `avsample_sink.m` source in (above).
- **Custom sink:** `avsample_sink.m` renders decoded NV12 (pulled from an `appsink`)
  into our OWN `AVSampleBufferDisplayLayer` hosted in the app's `NSView`. It bypasses
  GStreamer's broken applemedia sinks entirely (`avsamplebufferlayersink` UAF on the
  rotation caps-change, `osxvideosink` teardown deadlock) → low latency + clean resize.
  Because it does NOT use the applemedia plugin, it does **not** require the GStreamer
  1.29.x dev build that the buggy `avsamplebufferlayersink` needed — the stable
  **1.28.4** should work (validated live on 1.29.1; re-verify on 1.28.4 before release).
- Universal2: brew openssl/libplist `.a` are arm64-only — the x86_64 slice needs fat
  openssl/libplist (build from source → lipo); deferred to packaging.

Verify:
```bash
nm -gU build-arm64/uxplay-core.dylib | grep airplay_core_   # 8 exports (+2 bonus)
nm build-arm64/uxplay-core.dylib | grep -c gst_macos_main   # 0
clang -O2 -o m1_smoke m1_smoke.c && ./m1_smoke build-arm64/uxplay-core.dylib   # PASS
```
