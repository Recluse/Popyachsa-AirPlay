# UxPlay patches for Plan B

Patches applied on top of the local UxPlay fork at `C:\msys64\home\me\UxPlay`
(itself already carrying the rotation + Plan-A-overlay work from rc1).

## Files

| File | What |
|---|---|
| `0001-plan-b-core-dll.patch` | Unified diff vs `pre-b2-backup/` for `uxplay.cpp`, `renderers/video_renderer.c`, `CMakeLists.txt`. |
| `clean-full-vs-v1.73.6.diff` | Authoritative full fork diff vs `fc126fd` (apply with `git apply`). Carries the core-DLL ABI, L1 gmainloop fix, rotation, and the macOS custom-sink wiring. |
| `airplay_core.h` / `airplay_core.cpp` | NEW files (copied here for reference) — live at `lib/` in the fork. The flat C ABI of `uxplay-core.dll`. |
| `avsample_sink.m` | NEW file (copied here for reference) — lives at `renderers/` in the fork. **macOS only:** custom video sink — pulls NV12 from an `appsink` and renders into our own `AVSampleBufferDisplayLayer` (bypasses GStreamer's buggy applemedia sinks: low latency + clean resize, no UAF/deadlock). Built via `renderers/CMakeLists.txt` (OBJC + AV/CoreMedia/CoreVideo/QuartzCore frameworks, in the diff). |
| `pre-b2-backup/` | Pre-B2 copies of the three edited files (rollback source). |

**Not in the patch (tiny, apply by hand):** `lib/CMakeLists.txt` — after
`aux_source_directory(. play_src)` insert:
```cmake
list(FILTER play_src EXCLUDE REGEX "airplay_core\\.cpp$")
```
(keeps `airplay_core.cpp` out of the `airplay` static lib — it belongs to the
DLL target only).

## What B2 does

Turns the UxPlay engine into an embeddable shared library `uxplay-core.dll`
with a flat C ABI (no GLib/GST/C++ types crossing the boundary), so an
MSVC-built Rust host can drive it in-process:

* `uxplay.cpp`: former `main()` body → `extern "C" int airplay_run_blocking(argc,argv)`;
  added `airplay_request_shutdown()`, `airplay_set_host_window()` /
  `airplay_get_host_window()` shims; thin `main()` for the standalone exe kept.
* `video_renderer.c`: GstVideoOverlay block now prefers the in-process host HWND
  (`airplay_get_host_window()`) over the `UXPLAY_OVERLAY_HWND` env var.
* `lib/airplay_core.{h,cpp}`: the public C API + a worker-thread driver.
* `CMakeLists.txt`: `-DBUILD_CORE_DLL=ON` → `add_library(uxplay-core SHARED ...)`.

## Build

```bash
# MSYS2 UCRT64, with the Bonjour SDK shim headers/libs:
cd /c/msys64/home/me/UxPlay
PATH="/c/msys64/ucrt64/bin:$PATH" BONJOUR_SDK_HOME="C:/Work/uxplay/bonjour-sdk" \
  cmake -S . -B build -DBUILD_CORE_DLL=ON -DNO_MARCH_NATIVE=ON
PATH="/c/msys64/ucrt64/bin:$PATH" ninja -C build
# -> build/uxplay.exe (unchanged) + build/uxplay-core.dll + build/uxplay-core.dll.a
```

**`-DNO_MARCH_NATIVE=ON` is not optional for anything you ship.** Upstream defaults
to `-march=native`, which bakes the *build machine's* instruction set into the
binary. This recipe omitted it, and the consequence is in the released artifact:
disassembling the shipped 0.2.12 `uxplay-core.dll` finds **229 AVX instructions**
(`ymm` registers; no `zmm`, so AVX2 but not AVX-512). That DLL dies with an illegal
instruction on any CPU without AVX — which is not just pre-2011 hardware: the
Atom-derived Celeron/Pentium lines (Apollo Lake, Gemini Lake) have no AVX at all,
and cheap mini-PCs built on them are exactly what people park under a TV to run
this. The macOS and Linux build scripts already pass the flag; Windows was the
only path still building native, because it is the only one driven by hand from
this file rather than by a script.

## ⚠ Runtime gotcha — `dnssd.dll` load order (important for the Rust app too)

`C:\Windows\System32\dnssd.dll` exists (Apple Bonjour, 2015) and the Win32 DLL
search order finds it **before** PATH. UxPlay's `LoadLibraryA("dnssd.dll")` will
grab the System32 Bonjour one — which needs the (crashing) `Bonjour Service` and
returns **`-65563 kDNSServiceErr_ServiceNotRunning`**.

**Fix:** our `dnssd.dll` shim MUST sit in the **same directory as the host .exe**
(app-dir is searched before System32). For `uxplay.exe` this is automatic (shim
lives in `ucrt64/bin` next to it). For the B2 harness and the future
`popyachsa-airplay.exe`, the shim must be copied next to the executable.
`make-dist.ps1` already drops `dnssd.dll` in the dist root — keep that.

## B2 smoke test — PASSED 2026-05-28

`research/airplay_core_harness.c` (create → set name "AirPlay tester" →
set_window(NULL) → start → 15 s → stop → destroy). Result: shim used (embedded
mDNS), `_raop._tcp` + `_airplay._tcp` advertised as "AirPlay tester" on
192.168.255.111:5353, clean `exit 0`, no hang. (iPhone-render confirmation is a
separate manual step.)
