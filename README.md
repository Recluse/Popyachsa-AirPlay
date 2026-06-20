# Popyachsa AirPlay

A low-latency **AirPlay receiver** for **Windows, Linux and macOS**. Mirror your
iPhone, iPad or Mac — or stream video, photos and music — straight to your computer
over the local Wi-Fi. Hardware-accelerated, open source, no accounts, no telemetry.

> **Windowed AirPlay on a Mac** — unlike Apple's built-in AirPlay Receiver, which
> is fullscreen-only, this gives you a resizable, movable window (also on Win/Linux).

🌐 **[airplay.popyachsa.com](https://airplay.popyachsa.com)** — downloads & docs

## Features

- **Low-latency mirroring** — hardware H.264/H.265 decode (Direct3D 11 / VA-API /
  NVDEC / VideoToolbox, per OS), orientation tracked down to the codec byte.
- **Instant media** — YouTube, Photos, Safari, Apple TV+ play via direct HLS.
- **Works where Bonjour fails** (Windows) — a bundled
  [mDNS shim](https://github.com/Recluse/AirPlay-DNS-SD-Shim) handles multi-NIC /
  WireGuard hosts where Apple's Bonjour service crashes.
- **Native & lightweight** — a tiny tray app; fullscreen, multi-monitor, aspect-locked
  drag/resize.
- **16 languages**, follows your system locale.
- **Signed auto-updates** — Ed25519-signed update feed; a compromised download host
  can't push a malicious build. Delta updates on Linux (zsync AppImage).

## Install

| OS | Get it |
|----|--------|
| **Windows 10/11** | Installer (.exe) or portable .zip — [downloads](https://airplay.popyachsa.com) |
| **Linux** | AppImage, `.deb`, `.rpm`, or the signed **apt/dnf repos** (`apt`/`dnf upgrade`) |
| **macOS** (Apple Silicon + Intel) | `.dmg` — first launch: right-click → Open (ad-hoc signed) |

## Build from source

See **[BUILD.md](BUILD.md)** — per-OS recipes for the engine (`uxplay-core`) + the
Rust app. TL;DR: `git clone --recurse-submodules`, build the engine with CMake
(`-DBUILD_CORE_DLL=ON`), then `cargo build --release`.

## How it works

```
  iPhone / iPad / Mac  ── AirPlay ──►  uxplay-core  ──►  airplay-lib (Rust FFI)
                                       (patched UxPlay,    ──►  popyachsa-airplay
                                        flat C ABI, GPL-3)       (tray + host window + UI)
```

- **`uxplay-core`** — the [UxPlay](https://github.com/FDH2/UxPlay) engine as a shared
  library with a flat C ABI ([our fork](https://github.com/Recluse/UxPlay); patches:
  rotation, library-mode, host-window render). Built separately, `dlopen`'d at runtime.
- **`airplay-lib-sys` / `airplay-lib`** — raw + safe Rust bindings.
- **`popyachsa-airplay`** — the tray app, per-OS host window, settings/about UI (egui),
  i18n, config, autostart, signed self-updater.

## Licence

**GPL-3.0-or-later.** The AirPlay engine is a fork of UxPlay (GPL-3), so everything
linking it inherits the licence. See [LICENSE](LICENSE) and the credit chain in
[NOTICE](NOTICE) (UxPlay → RPiPlay → shairplay/openairplay; mjansson/mdns; GStreamer;
GLib; OpenSSL). The standalone Windows mDNS shim is published separately under MIT
at [Recluse/AirPlay-DNS-SD-Shim](https://github.com/Recluse/AirPlay-DNS-SD-Shim).

Built on [UxPlay](https://github.com/FDH2/UxPlay) by Recluse.
