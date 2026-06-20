# UxPlay patches (reference export)

The engine is built from the **`third_party/uxplay`** submodule
([`Recluse/UxPlay`](https://github.com/Recluse/UxPlay), branch
`popyachsa-integration`) — a fork of [FDH2/UxPlay](https://github.com/FDH2/UxPlay).
This directory is a **reference copy** of our patches so reviewers can read them
without checking out the submodule. The submodule is the source of truth.

## Files

| File | What |
|---|---|
| `clean-full-vs-v1.73.6.diff` | Authoritative full fork diff vs upstream `fc126fd`. Carries the core-DLL ABI, the GLib main-loop fix, rotation, host-window render, and the macOS custom-sink wiring. Apply with `git apply`. |
| `airplay_core.h` / `airplay_core.cpp` | The flat C ABI of `uxplay-core` (8 exports) + a worker-thread driver. Live at `lib/` in the fork. |
| `avsample_sink.m` | **macOS only:** custom video sink — pulls NV12 from an `appsink` and renders into an `AVSampleBufferDisplayLayer` (bypasses GStreamer's buggy `applemedia` sinks: low latency, clean resize, no UAF/deadlock). Lives at `renderers/` in the fork. |

## What the patches do

Turn the UxPlay engine into an embeddable shared library `uxplay-core` with a flat
C ABI (no GLib/GST/C++ types crossing the boundary), so a GUI host can drive it
in-process:

* `uxplay.cpp`: former `main()` body → `extern "C" airplay_run_blocking(argc,argv)`;
  added `airplay_request_shutdown()`, host-window + log-forward + library-mode hooks;
  thin `main()` for the standalone exe kept.
* `renderers/video_renderer.c`: GstVideoOverlay prefers a caller-supplied native
  window handle (HWND / X11 XID / NSView) over creating its own.
* `lib/airplay_core.{h,cpp}`: the public C API + a worker-thread driver.
* `lib/raop_rtp_mirror.c` + `raop*.h`: decode the mirror-stream orientation byte →
  drive rotation so all four device orientations render upright.
* `CMakeLists.txt`: `-DBUILD_CORE_DLL=ON` builds `uxplay-core` as a shared library.

To build the engine, see [`../BUILD.md`](../BUILD.md).

## Runtime gotcha (Windows) — `dnssd.dll` load order

`C:\Windows\System32\dnssd.dll` (Apple Bonjour) is found by the Win32 DLL search
order **before** PATH, and needs the (often-crashing) Bonjour Service. The fix:
the bundled `dnssd.dll` shim must sit in the **same directory as the host .exe**
(the app dir is searched before System32). `make-dist.ps1` places it there.
