# Building Popyachsa AirPlay from source

Popyachsa AirPlay is two pieces:

1. **`uxplay-core`** — the AirPlay engine: a fork of [UxPlay](https://github.com/FDH2/UxPlay)
   built as a shared library (`uxplay-core.dll` / `.so` / `.dylib`) exposing a flat
   C ABI (8 functions). Built with CMake (`-DBUILD_CORE_DLL=ON`). **GPL-3.0.**
2. **`popyachsa-airplay`** — the Rust tray app (this workspace). It `dlopen`s the
   engine at runtime (via the `airplay-lib` → `airplay-lib-sys` crates), so the two
   are built independently and the engine lives **next to the app binary** at runtime.

The engine source is referenced as a git **submodule** at `third_party/uxplay`
(our fork of FDH2/UxPlay carrying the rotation + library-mode + host-window patches;
the same patches are exported under [`uxplay-patches/`](uxplay-patches/) for review).

```
git clone --recurse-submodules https://github.com/Recluse/Popyachsa-AirPlay
# or, after a plain clone:
git submodule update --init --recursive
```

The 8 ABI exports: `airplay_core_create`, `_set_device_name`, `_set_log_callback`,
`_set_window`, `_set_options`, `_start`, `_stop`, `_destroy`.

---

## Windows

**Toolchain**
- [MSYS2](https://www.msys2.org/), **UCRT64** environment, with:
  `pacman -S mingw-w64-ucrt-x86_64-{gcc,cmake,ninja,pkgconf,gstreamer,gst-plugins-base,gst-plugins-good,gst-plugins-bad,gst-plugins-ugly,gst-libav,openssl,libplist}`
- [Rust](https://rustup.rs/) (MSVC toolchain).
- For the installer: [NSIS](https://nsis.sourceforge.io/) (`winget install NSIS.NSIS`).
- The dnssd shim (`dnssd.dll`) — build the separate
  [`AirPlay-DNS-SD-Shim`](https://github.com/Recluse/AirPlay-DNS-SD-Shim) repo, or grab its release.

**1. Build the engine DLL** (from the MSYS2 UCRT64 shell):
```bash
cd third_party/uxplay
cmake -S . -B build -G Ninja -DBUILD_CORE_DLL=ON
ninja -C build uxplay-core            # -> build/uxplay-core.dll
```
> The optional Bonjour-proxy path of the shim needs Apple's Bonjour SDK headers
> (`BONJOUR_SDK_HOME=...`); the embedded-mdns path needs nothing from Apple.

**2. Build the app:**
```bash
cargo build --release                 # -> target/release/popyachsa-airplay.exe + updater.exe
```

**3. Bundle + installer** (PowerShell, in `app/`):
```powershell
.\make-dist.ps1                       # bundles exe + updater + uxplay-core.dll + dnssd.dll
                                       # + the GStreamer runtime/plugins -> dist\ + dist\PopyachsaAirPlay.zip
cd installer
makensis /DVERSION=X.Y.Z popyachsa-airplay.nsi   # -> ..\dist\PopyachsaAirPlay-Setup.exe
```
The installer is **per-user** (`%LocalAppData%`, no administrator rights) — this is
required: the in-app auto-updater writes into the install dir without elevation, so a
Program Files install would break self-update. `popyachsa-airplay.nsi` enforces this
(it refuses a Program Files target).

The only non-obvious step is the GStreamer bundle: `make-dist.ps1` walks the DLL
import graph from `uxplay-core.dll` + every plugin and copies the closure, so the
zip runs on a machine with no MSYS2/GStreamer installed.

---

## Linux

**Toolchain** (Ubuntu 24.04 / Debian 12 shown; glibc floor = your build distro):
```bash
sudo apt install build-essential cmake ninja-build pkg-config \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libavahi-compat-libdnssd-dev \
  libssl-dev libplist-dev libx11-dev libxv-dev libgtk-3-dev
# runtime: gstreamer1.0-plugins-{base,good,bad}, gstreamer1.0-libav, avahi-daemon
```
mDNS is the system **Avahi** — no shim needed.

**1. Build the engine .so:**
```bash
cd third_party/uxplay
cmake -S . -B build -G Ninja -DBUILD_CORE_DLL=ON -DCMAKE_POSITION_INDEPENDENT_CODE=ON
ninja -C build uxplay-core            # -> build/uxplay-core.so
```
> `-DCMAKE_POSITION_INDEPENDENT_CODE=ON` is **required** — without it the static
> `renderers`/`airplay` libs fail to link into the shared object (`R_X86_64_PC32 …
> recompile with -fPIC`).

**2. Build the app + co-locate the engine:**
```bash
cargo build --release
cp third_party/uxplay/build/uxplay-core.so target/release/
```

**3. Package** (scripts under [`packaging/`](packaging/)):
- **AppImage** (delta-updatable): `PROFILE=release bash packaging/build-appimage.sh`
- **.deb**: `VERSION=X.Y.Z BIN=target/release/popyachsa-airplay SO=target/release/uxplay-core.so PKGDIR=packaging/shared OUT=. bash packaging/build-deb.sh`
- **.rpm** (in a Fedora container): `… bash packaging/build-rpm.sh`
- **tarball**: a flat `tar czf` of the binary + `uxplay-core.so` + `packaging/shared/com.popyachsa.AirPlay.{desktop,png,metainfo.xml}`

`dpkg-shlibdeps` auto-derives the `libc6 (>=)` floor + GStreamer/GTK deps for the
.deb; the .rpm uses `AutoReq` + a hand-listed plugin/avahi `Requires`.

---

## macOS

**Toolchain**
- Xcode command-line tools; Homebrew GStreamer:
  `brew install gstreamer cmake ninja pkg-config`
- Rust (rustup). mDNS is native (Apple) — no shim.

**1. Build the engine .dylib:**
```bash
cd third_party/uxplay
cmake -S . -B build -G Ninja -DBUILD_CORE_DLL=ON
ninja -C build uxplay-core            # -> build/uxplay-core.dylib
```
> macOS renders into an `NSView*` and uses a custom `AVSampleBufferDisplayLayer`
> video sink (`renderers/avsample_sink.m`) to avoid GStreamer's `applemedia` sink
> bugs. The GStreamer pipeline must run on a thread with a Cocoa run loop
> (`gst_macos_main`) — the C ABI honours this.

**2. Build the app + assemble the `.app`:**
```bash
cargo build --release
bash build/macos/make-app.sh          # self-contained Popyachsa AirPlay.app (bundles dylibs + GStreamer)
bash build/macos/make-dmg.sh          # -> Popyachsa-AirPlay-X.Y.Z.dmg
```
For the in-app self-updater, also `zip -ry Popyachsa-AirPlay-X.Y.Z.zip "Popyachsa AirPlay.app"`.

**Distribution gotcha:** the `.app` is ad-hoc signed, so a *downloaded* copy is
Gatekeeper-quarantined — first launch needs right-click → Open. A clean experience
needs an Apple **Developer ID** certificate + notarization (paid).

---

## Running from source (any OS)

```bash
cargo run --release      # needs uxplay-core.{dll,so,dylib} next to the binary
                         # (target/release/), or on the loader search path
```

A tray icon appears; the receiver advertises on the LAN; the video window pops up
when a device connects. See [`README.md`](README.md) for usage.
