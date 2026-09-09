# A1 — Automatic mirror-orientation tracking (detailed)

*Prep doc for the first upstream PR to FDH2/UxPlay. Nothing pushed yet.*

---

## 1. The problem

In AirPlay **screen mirroring**, the iPhone/iPad always encodes frames in its
**native portrait** raster and signals the *current* device orientation as
metadata — it does **not** pre-rotate the pixels. The receiver is expected to
rotate the decoded frame to match.

UxPlay today does **not** do this. It only has the **static** user flag
`-r {90,180,270}` → `append_videoflip()` inserts a single fixed `videoflip` at
launch. So:
- Rotate the phone while mirroring → the picture does **not** follow; landscape
  comes out sideways / upside-down.
- Worse, **both landscape-left and landscape-right report the *same* width×height**
  in the codec packet (e.g. 1920×884 either way), so you can't even distinguish
  them from the dimensions — you need the orientation metadata.

Related (closed) upstream issue: **#346** "video renderer paused when … screen
orientation changed".

## 2. The protocol insight

The mirror **codec-data packet** (type-1 / SPS-PPS header packet) carries the
logical width/height *and* an orientation byte. Upstream already reads
`packet[4]`+`packet[5]` but treats them only as the **payload-type** discriminator
(see the upstream comment in `raop_rtp_mirror.c`). Empirically, **`packet[5]`
also encodes the device orientation**:

| `packet[5]` | Orientation | videoflip `video-direction` (GstVideoOrientationMethod) | Confidence |
|---|---|---|---|
| `0x00` | portrait (upright) | `0` IDENTITY | ✅ confirmed |
| `0x04` | landscape, 90° CW (home/camera to the **right**) | `1` 90R | ✅ confirmed |
| `0x07` | landscape, 90° CCW (home/camera to the **left**) | `3` 90L | ✅ confirmed |
| `0x05` | upside-down? | `2` 180 | ⚠ **unconfirmed** |
| `0x06` | upside-down? | `2` 180 | ⚠ **unconfirmed** |
| other | unknown | aspect heuristic fallback | — |

(0x00/0x04/0x07 verified on device; 0x05/0x06→180 is a guess — needs an iPhone
that allows portrait-upside-down, or an iPad.)

## 3. How the fix works (data flow)

```
raop_rtp_mirror_thread (lib/raop_rtp_mirror.c)
  rotation_hint = packet[5]
   └─► callbacks.video_report_size(..., rotation_hint)        # raop.h: signature +arg
        └─► uxplay.cpp video_report_size() thunk              # passes it through
             └─► video_renderer_size(..., rotation_hint)      # renderers/video_renderer.c
                   • track last_rotation_hint (separate from w×h —
                     both landscapes share dims, so a dims-only gate misses
                     landscape↔landscape flips)
                   • on (dims_changed || rot_changed):
                       map packet[5] → method, then
                       g_object_set(rotator, "video-direction", method)
```

The pipeline gains an **always-present** `videoflip name=rotator
video-direction=identity` (h264/h265 mirror pipelines only; not jpeg), grabbed by
name after build so `video_renderer_size()` can retarget it live.

## 4. Exact change set (A1 only)

| File | Change | Keep for PR? |
|---|---|---|
| `lib/raop.h` | `video_report_size(...)` gains `int rotation_hint` | ✅ |
| `lib/raop_rtp_mirror.c` | extract `rotation_hint = packet[5]`, pass to callback | ✅ |
| `renderers/video_renderer.h` | signature change | ✅ |
| `renderers/video_renderer.c` | `last_rotation_hint`; `rotator` field; `video_renderer_size()` switch; always-on `videoflip name=rotator`; grab-by-name; `#include <stdio.h>` | ✅ |
| `uxplay.cpp` | thunk passes `rotation_hint` | ✅ |
| `lib/raop_handlers.h` (hunk 1) | advertise rotation: `rotation_node 0→1`, `features 14→14\|256` (bit 8) | ⚠ **see Q2** |

## 5. Pre-PR cleanup (strip the debug scaffolding)
- **Remove** the `ROTATION-PROBE` 128-byte hex-dump block in `raop_rtp_mirror.c`
  (was used to find the byte; not for production).
- **Remove** the `PATCH-DIAG: unhandled SET_PARAMETER` logging block in
  `raop_handlers.h` (hunk 2) — unrelated diagnostic.
- Keep one concise `LOGGER_DEBUG` line on rotation change (not INFO spam).
- Mark `0x05/0x06→180` clearly as unconfirmed in code (it already is) or route
  unknowns to the aspect fallback only.

## 6. Open design questions — decide before the PR

**Q1 — Interaction with the static `-r` flag (double videoflip).**
`append_videoflip()` (the `-r` flag) runs at `video_renderer.c:409`, and we append
`videoflip name=rotator` right after (`:415`). So if a user passes `-r`, the
pipeline has **two** videoflips — the fixed one *and* our dynamic one → stacked
rotation. Options for the PR:
- (a) only insert the dynamic rotator when **no** `-r` is given (auto XOR manual);
- (b) make the dynamic rotator *compose* with `-r` (apply `-r` as a base offset);
- (c) replace `-r` semantics with "manual override locks auto off".
→ **Recommend (a)**: auto by default, `-r` opts into the old fixed behaviour.

**Q2 — Does advertising rotation-support change what the phone sends?**
The `raop_handlers.h` change sets `rotation=true` + feature **bit 8** in the
`/info` display caps. Need to confirm this does **not** make the iPhone
*pre-rotate* frames (which would make our videoflip double-rotate). If auto-rotation
works *without* the caps change, **drop the caps change from the PR** (smaller,
safer). If it's required, document *why*. **Test both ways on device.**

**Q3 — Confirm `0x05/0x06`.** Need an upside-down capture (iPad, or iPhone with
upside-down enabled) to verify the 180° mapping; otherwise ship 0x00/0x04/0x07
only and treat the rest as the aspect fallback.

**Q4 — Default on vs. opt-in.** Maintainer may prefer a flag (e.g. `-rotate auto`).
Frame the PR as "auto by default, `-r` still forces fixed" (Q1a) and let the
maintainer ask for a gate.

## 7. PR framing (draft)
- **Title:** "Mirroring: follow iPhone screen orientation automatically (decode
  the codec-packet orientation byte)."
- **Body:** problem (frames are always portrait + orientation as metadata; both
  landscapes share dims) → the `packet[5]` finding (table) → the dynamic
  `videoflip` approach → behaviour vs `-r` (Q1) → tested orientations → note the
  0x05/0x06 caveat (Q3). Reference #346. Cross-platform (videoflip is generic).
- **Test plan to include:** start mirroring; rotate through portrait → landscape-R
  → landscape-L → portrait, and a direct landscape-R↔landscape-L flip (the case
  the dims-only gate missed); confirm each renders upright. Note untested
  upside-down.

## 8. Why this is genuinely upstreamable
- Net-new capability (no auto-orientation upstream — only the static `-r`).
- Small, localized, cross-platform (no Windows/d3d11/Popyachsa coupling).
- Fixes a real, reported pain (#346) and the everyday "landscape is sideways" UX.
