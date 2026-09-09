# UxPlay fork — clean diff, split by feature (for upstream PRs)

**Base:** upstream `FDH2/UxPlay` `master` @ `fc126fd` = **v1.73.6 +1 commit**
(the fork's `origin` *is* upstream; HEAD == upstream).
**Our changes:** uncommitted working-tree edits — **8 modified files + 2 new files**,
captured verbatim in **`clean-full-vs-v1.73.6.diff`** (527 lines).

```
git -C /c/msys64/home/me/UxPlay diff HEAD        # reproduces the diff (minus the 2 new files)
```

Goal: turn this single blob into **4 independent feature branches off upstream
`master`**, each a small reviewable PR with no "Popyachsa"/Windows-app/ debug
cruft. Below = exactly which hunks belong to each, what to clean, novelty vs
upstream, and the matching issue.

---

## Feature 1 — Automatic mirror rotation  *(PR "A1", easiest high-value, cross-platform)*

**What:** the iPhone always sends mirror frames in native **portrait** and signals
the current orientation as metadata. We decode `packet[5]` of the mirror
codec-data packet into a rotation hint and drive an always-present
`videoflip name=rotator` so the picture auto-rotates live as the phone turns.

**Novelty:** upstream only has the **static** `-r` flag (`append_videoflip`,
set once at launch) and treats `packet[5]` as part of the payload-type only
(`raop_rtp_mirror.c` upstream comment). **No automatic orientation tracking
exists upstream** → this is a real new feature, not a dup.

**Hunks:**
| File | What |
|---|---|
| `lib/raop.h` | `video_report_size(...)` gains `int rotation_hint` |
| `lib/raop_handlers.h` (hunk 1) | advertise rotation support: `rotation_node 0→1`, `features 14→14\|256` (bit 8) |
| `lib/raop_rtp_mirror.c` | extract `rotation_hint = packet[5]`, pass to callback |
| `renderers/video_renderer.h` | signature change |
| `renderers/video_renderer.c` | `last_rotation_hint`, `rotator` field, the `video_renderer_size()` switch (0x00/0x04/0x07/0x05/0x06), the always-on `videoflip name=rotator`, grab-rotator-by-name; `#include <stdio.h>` |
| `uxplay.cpp` | `video_report_size` thunk passes `rotation_hint` |

**Pre-PR cleanup (must):**
- **Remove** the `ROTATION-PROBE` 128-byte hex-dump block in `raop_rtp_mirror.c`
  (debug only).
- **Remove** the `PATCH-DIAG: unhandled SET_PARAMETER` logging block in
  `raop_handlers.h` (hunk 2) — debug only, unrelated to the fix.
- **Gate/mark** the `0x05/0x06 → 180°` mapping as *unconfirmed* (already commented
  TBD; keep the comment, maybe default unknown→aspect-heuristic only).

**Related issue:** **#346** (closed) "video renderer paused when … screen
orientation changed" — reference it; our feature is the clean way to handle
live orientation change. No *open* rotation issue.

---

## Feature 2 — Embeddable library mode (`libuxplay-core`)  *(PR "A2", open an issue first — API design)*

**What:** build the engine as a shared lib with a flat C ABI so any GUI can drive
it in-process (no shelling out to `uxplay.exe`). `main()` body → `extern "C"
airplay_run_blocking(argc,argv)`; `cleanup()` returns (doesn't `exit()`) in
library mode; a log-forward hook; `airplay_request_shutdown()`.

**Novelty:** nothing like it upstream (grep confirmed). The most valuable
contribution; needs maintainer buy-in on the API → **open an issue before the PR.**

**Hunks:**
| File | What |
|---|---|
| `CMakeLists.txt` | `option(BUILD_CORE_DLL …)` + `uxplay-core` SHARED target (additive, OFF by default) |
| `lib/CMakeLists.txt` | exclude `airplay_core.cpp` from the `airplay` static lib |
| `lib/airplay_core.h` / `lib/airplay_core.cpp` | **NEW** — the flat C ABI + worker-thread driver |
| `uxplay.cpp` | log-forward typedef/globals + forwarding in `log()`; `airplay_set_log_forward`; `main()`→`airplay_run_blocking` refactor; `return 1` on the 3 error-paths; `return 0` at end; `airplay_request_shutdown`; `airplay_set_library_mode` + `cleanup()` library-mode early-return; the `main()` wrapper restructure (keeps `gst_macos_main`) |

**Pre-PR cleanup:** strip the `airplay_set_host_window`/`airplay_get_host_window`
pair OUT of this PR → it belongs to Feature 3 (keep A2 generic, no window
assumptions).

**Related issue:** none open. (Embedding has been asked informally over time;
worth a quick search of discussions to link.)

---

## Feature 3 — Render into a caller-provided window (GstVideoOverlay)  *(PR "A3", depends on A2)*

**What:** let an embedder pass a native window handle so the videosink renders
into it (`gst_video_overlay_set_window_handle` + `handle_events(TRUE)`) instead
of creating its own window. Generic across `xv`/`gl`/`d3d11` sinks.

**Hunks:**
| File | What |
|---|---|
| `uxplay.cpp` | `airplay_set_host_window` / `airplay_get_host_window` (the `g_host_window_handle` static) |
| `renderers/video_renderer.c` | `#include <gst/video/videooverlay.h>`; `extern airplay_get_host_window`; the overlay block (prefer host HWND, else `UXPLAY_OVERLAY_HWND` env, bind sink, `handle_events`) |

**Novelty:** none upstream. Self-contained (in the exe the getter returns NULL →
falls back to env → sink makes its own window, so it's safe standalone).

**Pre-PR cleanup:** rename the Windows-flavoured `HWND`/`UXPLAY_OVERLAY_HWND`
wording to generic "window handle"; keep the env-var as an optional convenience
but frame the API as cross-platform (`guintptr` handle).

---

## Feature 4 — Low-latency HLS tuning  *(PR "A4", smallest — code + docs)*

**What:** for AirPlay HLS (source is an iPhone on the LAN, not the internet),
disable `playbin` DOWNLOAD + BUFFERING and set `buffer-duration=0 buffer-size=0`
to cut startup/live-edge latency.

**Hunks:**
| File | What |
|---|---|
| `renderers/video_renderer.c` | the media-mode `flags &= ~DOWNLOAD/~BUFFERING` + `buffer-duration/size=0` block |

**Pre-PR cleanup:** the two "lag-reduction B v2/v3" comments (leaky-queue and
do-timestamp) document **reverted dead-ends** — the code there is back to
upstream default. Drop those comments (or condense to a one-line "do NOT set
do-timestamp on appsrc, uxplay sets PTS manually" note, which is genuinely useful).

---

## Shared-file caution (for branch cutting)

Three features edit `renderers/video_renderer.c` and three edit `uxplay.cpp`.
In the **single** diff hunk `@@ -399,+466 @@` of `video_renderer.c`, three
features' insertions are adjacent (latency do-timestamp comment → F1 grab-rotator
→ F3 overlay block). When cutting branches each off **clean upstream master**,
each feature applies fine independently (insertions at the same anchor). They
only need rebasing if *stacked*. So: cut each branch from `master`, not on top of
each other — except **A3 depends on A2** (needs `airplay_run_blocking`'s TU and
the lib build to exercise it, though the source applies standalone).

**PR / branch order:** A1 (rotation) → A4 (latency) → issue+A2 (libcore) → A3 (overlay).

---

## Upstream-issue cross-check (the "did we already fix something?" answer)

Open issues today: **12** (`#522 #519 #518 #514 #480 #465 #462 #450 #448 #436
#358 #331`). Honest verdict: **our work is mostly net-new features, not fixes for
currently-open issues.** Closest touchpoints:
- **#518** "Nothing shows after starting and connecting" — *maybe* helped by the
  overlay/renderer work, but no platform detail; don't claim it.
- **#480** "Revisiting WSL — minor issues" — Windows-adjacent; our Windows stack
  (shim + d3d11 + overlay) is tangential at best.
- **#436** "screensaver inhibit on Windows/Wayland" — **NOT** ours.
- **#346** (closed) orientation-change → reference from the A1 PR.
The rest (ALSA volume, avdec freeze, kmssink, RPi colours/HEVC, audio crackling,
`-vrtp`, `sync=true` flag) are unrelated.

The separate **dnssd Windows shim** (Bonjour-free mDNS) addresses the Windows
Bonjour-crash class of problems — no open issue for it now, but it's the most
broadly useful standalone repo (see OPENSOURCE-PLAN §B/Repo 2).

---

## Next concrete step
Cut the 4 branches off `upstream/master` in a **clean clone/worktree** (don't
disturb the live working tree the app builds from), apply each feature's hunks
(per the tables above), do the cleanup, `git apply --check`. Then push to our
fork and open A1 first.
