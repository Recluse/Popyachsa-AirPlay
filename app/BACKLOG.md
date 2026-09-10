# Backlog

Non-blocking issues to revisit. Newest first.

## Release / distribution debt

**Status:** logged 2026-09-09. Each item is what has to happen before one of the
ordering rules in the release runbook can be retired. The rules stay until then.

* **Upload the macOS artifact to `dl.airplay.popyachsa.com`.** It has never been
  there (nor in the GitLab package registry, in any of 0.2.8 / 0.2.11 / 0.2.12).
  Until it is, `updates-macos.json`'s inverted `url`/`mirror_url` is the only
  reason macOS updates work, macOS has no real fallback host, and the release runbook forbids tidying the ordering.
* **Retire the legacy manifest signature** once pre-0.2.13 clients have aged out:
  client arm first, feed second (the release runbook). Until then every manifest must
  carry both signatures.
* **Ship the symlink-aware macOS updater one release before the symlinked
  bundle** (the release runbook). Rule expires once no 0.2.12-or-older macOS client
  is expected to update.
* **The shipped `dnssd.dll` was not built from the public shim source.** The
  0.2.12 binary is a debug variant that appends every mDNS registration to a
  hardcoded `C:\Work\uxplay\shim-registrations.log`; that code is not in
  `Recluse/AirPlay-DNS-SD-Shim`. Rebuild the next release's DLL from the public
  source (and check the log path is gone) so binary and source stop diverging.
* **`pick_local_ipv4()` and `net_interfaces.rs` disagree on link-local.** The
  shim skips `169.254/16`; the dropdown deliberately keeps it (direct-cable +
  mDNS is the case Bonjour exists for). So the UI can offer an adapter the shim
  would never pick. Harmless today only because the shim ignores our choice
  anyway — it becomes a real mismatch the moment it honours `interfaceIndex`.

## Network adapter selection

**Status:** **SHIPPED in 0.2.13** (2026-09-10), issue #1 closed. Three of the four
gates below passed; gate 2 is still unrun and is the one open question about this
feature. Kept here for that gate and for the indirect link noted under gate 4.
**Reported:** GitHub mirror issue [#1 "allow LAN Card selecting"](https://github.com/Recluse/Popyachsa-AirPlay/issues/1),
2026-08-06. (Issues land on the GitHub mirror, not the GitLab canonical repo — check
`gh issue list -R Recluse/Popyachsa-AirPlay` periodically.)

**Before:** listen and advertise on *everything*, with no way to say otherwise —
`netutils_init_socket()` hardcoded `INADDR_ANY`/`in6addr_any`, and both `DNSServiceRegister`
calls passed `interfaceIndex = 0`.

**What shipped:** `-bind <ipv4>` in the fork (an address, never a name — a name carries spaces,
and `split_args()` — then named `split_ws()` — would re-split it into an unknown option and
`exit(1)` the engine *inside the tray process*; since audit #4 that is a returned failure
rather than an `exit()`, but the engine still does not come up), plus `Config::bind_ip: Option<String>`, `src/net_interfaces.rs`, an adapter
dropdown in Settings, and the flag appended in all three `build_options()`. No C ABI change.
Fork files newly in the authoritative diff: `lib/netutils.{c,h}`, `lib/dnssd.{c,h}`,
`lib/airplay_video.c`.

Two non-obvious pieces, both load-bearing:

* **The statics must be reset on every engine start.** The Rust restart re-enters a
  never-unloaded image, so an option that is only ever *set* when its flag is present used to
  become a one-way switch. `-bind` cleared itself and called `netutils_set_bind_address(NULL)`
  unconditionally; the whole class (including the `-h265` case this entry logged — unticking
  H265 did not survive a config-watcher restart) is closed by audit #7's `reset_options()`,
  which runs first thing in `airplay_run_blocking()`. Keep it in step with the declaration
  block: a new option static without a line there reintroduces the bug.
* **The pin covers IPv6 too, and it has to.** The first design refused the v6 listener while
  pinned, reasoning that a pinned IPv4 adapter has no v6 counterpart. Measuring it killed that:
  a live iPhone 14 connects over IPv6 **link-local** (`Accepted IPv6 client` … `Local :
  fe80::…%15` in the engine log), so refusing v6 did not degrade the feature, it broke AirPlay
  outright. The v6 socket now binds to the link-local of the same adapter with its scope id;
  with no usable link-local it falls back to `in6addr_any` and the pin covers IPv4 only, logged.
  Note the earlier `assert(local_zone_id == remote_zone_id)` worry does not bite: client and
  server are in the same zone by construction once both are on the pinned adapter.
  Since neither listener is on loopback while pinned, `airplay_video.c` builds its HLS callback
  URL from `netutils_get_bind_host()` instead of the literal `localhost`.
  **The lesson worth keeping:** "the receiver is still visible while pinned" and "a client can
  still connect while pinned" are different claims, and only the first was ever checked.

**Deliberately not done:** pinning `find_mac()` — the MAC feeds `deviceid`, the `_raop`
instance name *and* the `pk` TXT record (`crypto.c:376-385` derives the keypair from it), so
switching adapters would force every client to re-pair for a cosmetic gain. Also out: IPv6
pinning, IPv6 rows in the dropdown, live re-bind on NIC events, and `IP_BOUND_IF`/
`SO_BINDTODEVICE` (three platform paths for one flag, and interface-binding breaks the HLS
loopback path harder than an address bind does).

**Gates before merge:**
1. ✅ **PASSED** — live iPhone mirroring while pinned, 2026-09-09. The log shows the whole
   chain: `bound to 192.168.255.5 and to that adapter's IPv6 link-local (scope 15, mDNS
   interface index 15)` → `Accepted IPv6 client` → `Begin streaming to GStreamer video
   pipeline`. This gate is also what disproved the original design: iOS connects over IPv6
   link-local, not IPv4, so the first version's refusal of the v6 listener made a pinned
   receiver unreachable. Re-pinning reconnects immediately.
2. ⬜ **NOT RUN** — AirPlay a YouTube video while pinned. The `airplay_video.c` HLS prefix
   change is reasoned from source, never executed; if GStreamer's HLS client treats a literal
   IP differently from `localhost`, video breaks while mirroring keeps working. Mirroring
   passing gate 1 says nothing about this — it is a different code path.
3. ✅ **PASSED** — pinned, then back to Automatic, without quitting the app. Both directions
   reconnect, which also exercises the `reset_options()` static-reset path on a config-watcher
   restart.
4. ✅ **PASSED** — Windows, 2026-09-09, on both mDNS paths. With Apple Bonjour running (the
   shim proxies to it) `dns-sd -B _airplay._tcp` showed the receiver on **if 8 only** while
   pinned, against if 8 / if 2 / if 26 unpinned — that is the fork's
   `resolve_bind_address()` now returning a real `IfIndex` instead of 0. With the Bonjour
   service **stopped**, the service still advertised, which only the embedded responder could
   have done, and the sockets were pinned the same way (`192.168.255.111` + the adapter's
   link-local). Engine ran with hardware d3d11 decoders.

   **One link in that chain is indirect, and it is the one covering this repo's own shim
   change.** The embedded announcement was proven to *happen*, and the embedded socket bind
   was proven *scoped*, but the embedded announcement was never browsed directly — with
   Bonjour stopped, `dns-sd -B` has no daemon to talk to, and installing an independent
   browser needed rights nobody wanted to grant at that hour. The scoping of the embedded
   announcement therefore rests on: the code path demonstrably executing, the bind landing on
   the right interface, `ipv4_for_ifindex` compiling clean, and the proxy path scoping to the
   same `if 8`. Strong, but measured around the claim rather than on it. Worth one direct
   browse before this ships.

**Known limits, all logged rather than fixed:**
* **Windows advertisement is not pinned — but the fix is small and now in reach.** Our
  `dnssd.dll` shim binds its mDNS socket to an address it picks *itself* via `pick_local_ipv4()`
  (a non-virtual-adapter name heuristic over `GetAdaptersAddresses`), and its `DNSServiceRegister`
  discards the caller's choice outright — literally `(void)interfaceIndex;`. So on Windows the
  sockets honour the pin while the announcement may advertise a different adapter.
  The shim is **open source at https://github.com/Recluse/AirPlay-DNS-SD-Shim**
  (`dnssd_shim.c`) — an earlier note in this file claiming the source existed only on the Windows
  box was simply wrong. The fix is therefore concrete: when `interfaceIndex` is non-zero, resolve
  that adapter's IPv4 and use it instead of `pick_local_ipv4()`'s guess. That, plus returning the
  real `IfIndex` from the fork's Windows `resolve_bind_address()` (it deliberately returns 0
  today, because a wrong index would land on Loopback Pseudo-Interface 1), closes the feature on
  the platform the original request came from.
* **Linux:** `avahi-daemon.conf`'s `allow-interfaces`/`deny-interfaces` can exclude the chosen
  interface inside the daemon *after* our index was accepted; registration returns NoError and
  nothing is announced. We pass `callBack = NULL`, which opts out of async error notification
  entirely, so this is undetectable from our side.
* **A pin that fails to resolve stays inactive until the engine is next started** — nothing
  watches network events. The common case is autostart-at-login racing DHCP; recovery is one
  Stop/Start from the tray. Release notes should say this plainly. Worse, the UI cannot tell:
  once the address comes back, the dropdown shows the adapter as selected while the engine is
  still listening everywhere, and re-saving the same value does not restart it
  (`old.bind_ip == new_cfg.bind_ip`). Only the startup log line distinguishes the two states.
  A real fix needs the engine to report back what it actually bound — a status-channel change,
  not a UI change.
* **Mid-session address change** (DHCP onto a new lease, VPN renumbering) leaves httpd bound to
  an address the host no longer owns while mDNS still answers with the new one: the receiver
  stays in the picker and refuses every connection. Rare on a receiver box; a re-check would be
  a new polling subsystem.
* The version pairing matters: a new app shipping `-bind` against an old
  `uxplay-core.dylib`/`.dll` hits `unknown option → exit(1)` in the worker thread and takes the
  tray down. They already ship together — keep it that way.

Diagnostic: `popyachsa-airplay --list-interfaces` prints exactly what the dropdown sees.
Default stays "Automatic", byte-identical to the pre-feature build.

## ~~The engine log loses exactly the lines you need after a hang~~

**Status:** **FIXED in 0.2.13** — and it was worse than this entry described. The
log did not lose the tail; it never contained one line of engine output at all,
on any platform. Windows because `redirect_stdio_to_log` `_dup2`'d stdout onto a
FILE* with no valid fd (a windows-subsystem process has no console, `_fileno`
gives -2, and the call fails silently); macOS and Linux because stdout is a file
there and libc makes it fully buffered while `uxplay.cpp`'s `log()` never
flushes. Both freopen'd and unbuffered now (`39854ec`, `5ee3240`, `0931108`);
measured on macOS as 0 engine lines before and 6 after, and on Windows as a full
79-line log surviving a force-kill. The analysis below is kept because it is how
the Windows half was found.
**Found:** 2026-09-09, investigating a receiver hang on macOS.

`redirect_stdio_to_log()` points the engine's stdout at a file, and stdio is
**block-buffered** to a file rather than line-buffered to a terminal. So when the
receiver wedges, the last few KB — the lines describing what it was doing when it
wedged — sit in an unflushed buffer and are never written. The log file ends
mid-sentence, sometimes mid-XML-tag.

That is precisely backwards: the log is least trustworthy exactly when it matters
most. Diagnosing the hang below required asking the owner to quit the app from the
tray so the buffer would flush on exit — which is not a step a bug reporter will
think of, and is impossible if the process has to be force-killed.

**Fix:** `setvbuf(stdout, NULL, _IOLBF, 0)` (line buffering) right after the
redirect, or `_IONBF` if the throughput cost is acceptable — the engine's normal
output is modest, and `-FPSdata` telemetry is the only high-rate producer. Measure
before choosing: this runs on the streaming path.

## ~~The AirPlay *video* protocol cannot work in the macOS bundle — no HLS plugins~~

**Status:** **FIXED for 0.2.14** — macOS by `fb9fe4b` (HLS plugins + GIO TLS
module bundled, TEXT off in the fork), Windows by `29ae912` (GIO TLS module
bundled). Pre-existing; **the shipped 0.2.12/0.2.13 have it**.
**Found:** 2026-09-09, when sending a video appeared to hang the receiver.

**What actually fixed it, per platform — corrected 2026-09-10.** Commit
`a2cd0e4` and fork commit `ebffb05` claimed the playlist-expander heap fix
"alone" made video play. That was wrong. The Windows session re-tested with one
variable at a time, a real iPhone, and cores that differ in exactly that hunk
(sha256 `dfce7e0c…62ce3` fixed, `eaca04fc…12ec8` buggy):

| run | core | clip | IDSE | PLAYING | subparse | result |
|---|---|---|---|---|---|---|
| 1 | buggy | plain | 0 | yes | yes | plays |
| 2 | buggy | CC on | 0 | yes | yes | plays, subtitles shown |
| 3 | fixed | plain | 0 | yes | yes | plays |
| 4 | fixed | CC on | 0 | yes | yes | plays, subtitles shown |
| 5 | buggy | plain, `lib/gio/modules/` removed | 0 | no | no | `Couldn't download fragments` |

(IDSE = "Internal data stream error".) Every run: `PREFIX="s/"`. So:

* **The heap fix is a no-op on real traffic.** `prefix_len` is 2, where the old
  size is exact. Correct for other prefixes; not why anything plays.
* **Windows: the fix was the GIO TLS backend.** Run 5 is the single-variable
  proof; the early symptom was `hlsdemux2 Couldn't download fragments`.
* **The "Internal data stream error" seen during the first debugging round did
  not reproduce** in any clean run, with or without TLS. Cause unknown; it was
  attributed to the heap bug without evidence.
* **The WebVTT hang is macOS-only.** The Windows bundle has `subparse`; the
  YouTube caption rendition (`DEFAULT=NO,AUTOSELECT=YES`) is auto-selected by
  decodebin3 in all four runs, CC on or off at the phone, and renders. macOS has
  no timed-text decoder in the bundle, so there TEXT on hangs. See "TEXT off"
  below for the consequence.

**Scope, precisely — this is narrower than "video is broken".** AirPlay carries
video two different ways, and only one of them is affected:

* **Screen mirroring** (`raop_rtp_mirror`, h264/h265 into our own sink) — works.
  Playing a video inside a mirrored screen is fine, and was verified working from
  the packaged app on 2026-09-09. Everything this path needs is bundled.
* **The AirPlay video protocol** (`on_video_play` with an `m3u8` URL, played
  through `playbin`) — cannot work. This is what a sender uses when it hands over
  a stream rather than mirroring the screen; the YouTube attempt that started this
  investigation took this path.

An early version of this entry said "video has never worked on macOS", which was
wrong and was corrected by the owner testing it. Both statements were tested from
the same packaged build, minutes apart: one attempt logged `on_video_play` and
stalled, the next logged `raop_rtp_mirror starting mirroring` and played.

**Cause.** `make-app.sh`'s `PLUGINS=(…)` array lists sixteen GStreamer plugins,
chosen as "the exact plugin set a live mirror session loads, captured via lsof".
A mirror session never touches HLS, so the video path's plugins were never in the
capture and are not in the bundle. Verified in both the freshly built app and the
**shipped 0.2.12 artifact** — identical sixteen, and `hls`, `soup`, `curl`,
`adaptivedemux2`, `dash` are absent from both. They are all present in the system
`GStreamer.framework`, so this is purely a bundling omission.

**Symptom.** The phone's request arrives and is answered — the log shows
`on_video_play: location = http://localhost:60846/master.m3u8` — and then
`on_video_rate = 0.00000` and silence. GStreamer cannot build a pipeline for a
playlist it has no demuxer for, so nothing happens and the receiver looks hung.
It is not hung: a `sample` of the live process shows the main thread in the normal
Cocoa event wait, and only mirror-pipeline threads alive.

**Not caused by the adapter-selection work.** The pin was OFF for that session
(`bind_ip` null, no `bound to` line), so `netutils_get_bind_host()` returned the
original literal `localhost` and the `airplay_video.c` callback-URL change was
inert. The obvious suspect is cleared.

**Fix is more than four names in the array.** `hls` + `adaptivedemux2` + a HTTP
source (`soup` or `curl`) is the start, but the playlist is served locally while
the *segments* come from an upstream CDN over HTTPS — so the TLS backend
(glib-networking's gio module) has to be bundled too, and that is not a GStreamer
plugin and is not covered by the script's existing dependency walk. Budget for
bundle growth and test with a real video before believing it works.

## GStreamer `int_range` CRITICAL spam on Linux (cosmetic)

**Status:** logged, not fixed (deliberately NOT silenced — may be fixable upstream in GStreamer).
**Reported:** 2026-06-21 (Recluse, Linux AppImage logs during an AirPlay session).

```
(AppRun.wrapped:NNNNN): GStreamer-CRITICAL **: gst_value_collect_int_range:
assertion 'collect_values[0].v_int < collect_values[1].v_int' failed
```

**What it is:** a GStreamer element builds an `GST_TYPE_INT_RANGE` caps field with
`low >= high` (a degenerate/empty range). GLib `CRITICAL` is a *log level*, not a crash —
playback continues; this is cosmetic log noise.

**Not our code:** every caps string in the UxPlay fork is a fixed literal
(`renderers/video_renderer.c`, `mux_renderer.c`, `audio_renderer.c`:
`video/x-h264,stream-format=(string)byte-stream,alignment=(string)au`, the h265/jpeg/audio
equivalents) — none use int-ranges. So the degenerate range comes from a bundled GStreamer
plugin's caps query/fixate, not from us.

**Next steps when we pick this up:**
1. Reproduce with `GST_DEBUG=*:3` (or `GLIB_CRITICAL=...`) + a `g_log` breakpoint /
   `GST_DEBUG=GST_CAPS:5` to capture which element + pad template emits the bad range.
2. Likely a decoder / `videoscale` / `videoconvert` / sink template caps on a specific
   plugin version in the AppImage bundle. Identify the element + version.
3. Fix options, in order of preference: (a) upstream patch to the offending plugin if it's
   a real bug; (b) pin/replace that plugin version in the AppImage bundle; (c) last resort,
   a `capssetter`/filter in the pipeline to avoid the degenerate negotiation.
4. Confirm it's gone from the engine log without suppressing real warnings.

See [[linux-1080p-h265-gating]] (same logs; unrelated root cause).

## Engine code that can still kill the host tray

**Status:** identified 2026-09-10 during the audit round, deliberately NOT fixed —
each one needs a design decision, not a guard.
**Why it matters:** the engine runs in-process on a worker thread inside the tray, so
every `exit()`/`abort()` below takes the user's whole application with it. The audit
round closed the *reachable-from-Settings* ones (bad option strings, failed sink
creation, `-hls 4`); these are what is left.

**1. `exit(1)` on allocation failure, deep in per-packet paths.**
`lib/raop_rtp_mirror.c:444`, `lib/airplay_video.c:497`, `lib/http_handlers.h:900`,
`lib/pairing.c:250`, `lib/srp.c:214`. Unlike the two `uxplay.cpp` sites (already
converted), none of these has an error channel to return into — the callers are
callbacks with `void` returns or no failure contract. Converting them means giving
those paths a way to fail the session, which is a real change to the fork.

**2. `airplay_core_start()` returns 0 for a run that dies milliseconds later.**
`lib/airplay_core.cpp:154` discards `airplay_run_blocking()`'s return value, and the
header exposes no worker-exit callback and no liveness query. So a start failure
reaches the Rust host **only** through the log-forward hook, which is why
`status.rs` has to pattern-match log lines at all. The clean fix is an optional
worker-exit symbol looked up through `libloading` (optional, so a host paired with
an older dylib still loads) — it touches `lib/airplay_core.{h,cpp}` and
`airplay-lib/src/lib.rs` together. A timeout-based liveness marker was considered
and rejected: the first-run GStreamer plugin scan takes seconds, and stopping a
healthy engine on a false alarm is worse than missing a dead one.

**3. `g_assert(renderer_type[i]->pipeline)` still aborts.**
`renderers/audio_renderer.c:253` and `renderers/video_renderer.c:411`. Both fire only
when even the fallback fails to build — a GStreamer install missing `coreelements`
or `playback`. Pre-existing upstream, and not reachable in our macOS bundle
(`playback` is in `make-app.sh`'s `PLUGINS`), but it leaves the "engine code must
never kill the host" invariant incomplete. Fixing means propagating a NULL slot
through the rest of `audio_renderer_init` and its consumers.

**4. ~~`status.rs` could stop a healthy engine on client-controlled text.~~**
**FIXED 2026-09-10** (`700b943`). All four `contains` markers are anchored at line
start, and the dnssd one additionally requires the `dnssd_register` prefix both
real lines carry — a bare `starts_with("failed with error code")` would have
looked like a fix while breaking the match it was meant to preserve. Anchoring is
safe because `process_metadata` only appends a value in the branch that just
appended its DMAP label, and the whole blob is logged as one multi-line message,
so only the first field's label can lead it. Six regression cases, each checked
to be flagged fatal by the previous logic.

**5. ~~Shim: `DNSServiceRefDeallocate()` freed after a timeout.~~**
**FIXED in shim 1.1.1** (`47bd0af`). Ownership is handed off through an
interlocked `released` field rather than guessed: the responder thread and the
deallocator both exchange a 1 into it, and whichever reads back a 1 arrived
second and frees. Exactly one ever frees, and a genuinely stuck thread costs one
leaked struct instead of a use-after-free.
**NOT YET SHIPPED** — 0.2.13 carries 1.1.0. The next Windows release must rebuild
the shim, and its log line should read 1.1.1.

## macOS update: the extractor refuses to write *through* a symlink

**Status:** shipped as the deliberate ceiling of the path-escape fix (2026-09-10);
marked with a `ponytail:` comment in `src/update_macos.rs`.

The unzip step keeps the set of symlinks it has created and rejects any entry whose
path crosses one, which is what makes lexical path resolution provably equal to the
kernel's and closes the chained-symlink escape (`a -> .`, then `a/b -> ..`). The cost:
a zip containing a *real* framework layout — the `Headers -> Versions/Current/Headers`
chains inside a `Versions/A` bundle — is refused outright.

Today nothing legitimate trips it: the bundle has zero symlinks (`GST_SYMLINK`
defaults to 0) and the one link `make-app.sh:95` would create has nothing stored
under it. But `make-app.sh:85` already contemplates repackaging GStreamer as a proper
versioned framework, and doing that would break macOS updates until this is replaced
with real link-following (resolve each target against the tree as built, then verify
the landing path, instead of refusing the crossing).

## Release artifacts live at constant URLs while the feed is being replaced

**Status:** identified 2026-09-10, not fixed — the fix moves URLs that shipped
clients follow.

`.gitlab-ci.yml` publishes the Windows zip/setup and the Linux AppImage under
filenames that do not carry the version. "Artifact first, feed second" does not buy
consistency: the artifact is replaced *under the URL the old feed already points at*,
so from the first rsync until both feed rsyncs land — and permanently if a later step
fails — the live feed advertises a sha256 that no longer matches the file behind it.

The fix is versioned filenames, which the macOS publishing script already does. Two
co-requisites before it can ship: the download buttons on the landing page are
hand-written in the landing repo's `index.html` (`ci/update_landing.py` only touches
`CHANGELOG.html` and the schema.org `softwareVersion`), and the AppImage's `.zsync`
carries the filename in its own header. Shipping the rename without both points every
client at a 404.

## A failed GStreamer init tells the host *that* it failed, never *why*

**Status:** identified 2026-09-10 while verifying the shipped 0.2.13 engine, not
fixed — the fix is a signature change, and the reachable half is already covered.

`gstreamer_init()` now uses `gst_init_check()` and returns false instead of
letting `gst_init()` terminate the process (that was the audit fix). But it
reports the reason with `g_print`, not through the logger, and it has no choice:
`renderers/audio_renderer.c`'s static `logger` is assigned in
`audio_renderer_init()`, which has not run yet. Upstream's `check_plugins()`
prints its "Required gstreamer plugin not found" messages the same way for the
same reason.

`g_print` writes to stdout. The host's log-forward hook is not stdout, so the
reason never reaches `status.rs`, and on macOS — where the tray is launched from
Finder and its stdout goes nowhere — it is lost entirely.

What the host *does* get is the caller's `LOGE("stopping: …")` at
`uxplay.cpp:3600`, which travels the hook and matches the fatal-start marker. So
the user is told the engine failed to start; they are not told that GStreamer is
missing a plugin. Given macOS bundles GStreamer, this is only reachable on a
broken Linux or Windows install.

The fix is to give `gstreamer_init()` a way to hand the message back — an
out-parameter or a returned string — and have `uxplay.cpp` LOGE it. One caller,
but it changes `audio_renderer.h`, and doing it properly means the same
treatment for `check_plugins()`'s five messages, which are the more useful ones.

Same defect class as [[airplay-where-things-live]]'s shim story: the code was
correct and the channel was wrong.

## Three platforms, three different compiler policies — Linux shipped its renderers at -O0

**Status:** measured 2026-09-10, not changed. A decision, not a one-liner: fixing
the optimisation also decides what happens to `assert()`.
**Found:** while checking whether an `assert` in the fork could ever fire.

The engine is configured three different ways and nobody had compared them:

| platform | `CMAKE_BUILD_TYPE` | `uxplay.cpp` (C++) | `lib/*.c` | `renderers/*.c` | `assert()` |
|---|---|---|---|---|---|
| macOS (`build-core-*.sh`) | `Release` | `-O3 -DNDEBUG` | `-O3 -DNDEBUG` | `-O3 -DNDEBUG` | compiled out |
| Linux (`Dockerfile.u24` recipe, 0.2.13 as shipped) | *(empty)* | **none** | `-O2` | **none** | live |
| Windows (`uxplay-patches/README.md` recipe) | *(empty)* | **none** | `-O2` | **none** | live |

Read straight out of the Linux container's `build.ninja` for the 0.2.13 build:
`uxplay.cpp.o` gets `FLAGS = -std=gnu++11 -fPIC`, `video_renderer.c.o` gets
`FLAGS = -fPIC`, `raop.c.o` gets `-Wall -O2`. The only `-O2` in the whole file
comes from `lib/CMakeLists.txt`, which sets `CMAKE_C_FLAGS` in *its own directory
scope*: `lib/` gets it, `renderers/` (a sibling) and `uxplay.cpp` (C++, a
different flags variable entirely) do not. With no build type, CMake adds
nothing of its own.

Windows, checked by the Windows session in its 0.2.14 tree (`UxPlay-0214/
build.ninja`, same CMakeLists, same empty build type): identical shape.
`uxplay.cpp` → `-std=gnu++11`, no `-O`; `renderers/video_renderer.c` has no
`FLAGS` line at all; every `lib/*.c` carries `-O2`. It also names one more file
this table had missed, and it applies to Linux too: `lib/airplay_core.cpp`, the
DLL/.so entry point, is C++ and gets no `-O` either — `lib/CMakeLists.txt`'s
`-O2` goes into `CMAKE_C_FLAGS`, and a `.cpp` reads `CMAKE_CXX_FLAGS`. The exact
tree that built the shipped 0.2.13 DLL could not be found, but the flags are a
function of CMakeLists + build type and nothing else, and both were the same.

So the Linux and Windows 0.2.13 engines shipped with the protocol library
optimised and everything else — `uxplay.cpp`, `airplay_core.cpp`, and the whole
`renderers/` directory, which is the per-frame GStreamer glue on the mirror path
(`video_renderer_render_buffer()` pushes every mirror frame into appsrc) — at
`-O0`. The audio/video decode itself lives in GStreamer, so this is not a
"video is slow" defect; it is a "we never looked" defect with an unmeasured cost
on the packet path.

The second column matters as much as the first. Windows and Linux ship with
`assert()` live; macOS ships with it compiled out. The Windows session showed
today what live asserts buy: in `adjust_yt_condensed_playlist` a `PREFIX` of any
length but 2 would abort with a clear SIGABRT rather than serve heap bytes — and
the absence of such reports is itself evidence about what senders emit. On
macOS the same input would have gone out silently.

**Options for 0.2.14, in order of how much they change:**

1. **`-DCMAKE_BUILD_TYPE=Release` on Linux and Windows**, matching macOS. One
   flag in two recipes. Gets `-O3` everywhere — and compiles asserts out on the
   two platforms that currently have them, which changes *how* things fail there.
2. **`Release`, but keep asserts**: additionally pass
   `-DCMAKE_C_FLAGS_RELEASE=-O2 -DCMAKE_CXX_FLAGS_RELEASE=-O2` so CMake's
   default `-O3 -DNDEBUG` is replaced. Optimised and asserting on all three,
   including macOS if the scripts are changed too. Most consistent; also the
   most novel, so it needs a real run on each platform before it ships.
3. **Leave it.** Record the fact and ship 0.2.14 as 0.2.13 was built. Nothing
   gets worse; nothing gets measured either.

Whichever is picked, `-DNO_MARCH_NATIVE=ON` stays: `Release` does not imply it,
and the `-march=native` incident is the one this file already documents.

Not done here: measuring the actual cost. A mirror session at 4K/h265 on the
Linux AppImage, `-O0` vs `-O2` for `renderers/`, is the number that would settle
whether option 1/2 is urgent or merely correct.

## TEXT off in the HLS branch is a macOS fix that costs Windows its subtitles

**Status:** identified 2026-09-10 by the Windows 2×2 run, not changed. Ships in
0.2.14 as is.

Fork `f700f24` clears `GST_PLAY_FLAG_TEXT` on the HLS playbin unconditionally,
because on macOS a selected WebVTT rendition hangs preroll (no timed-text
decoder in the bundle). On Windows the bundle has `subparse`, the rendition is
auto-selected regardless of the phone's CC toggle, and it rendered in all four
runs on the pre-`f700f24` core. With `f700f24` those subtitles are gone.

Two ways out, in order of preference:

1. **Bundle `subparse` on macOS and re-test with a subtitled clip.** If the hang
   goes away, TEXT stays on everywhere and the flag change is reverted. Cost:
   `subparse` is small; the +13 MB figure in the fork comment is pango/font
   stack, which `subparse` alone does not pull — verify with the dependency walk
   before believing either number. Needs a live device on macOS.
2. **Make the clear `#ifdef __APPLE__`.** One line. Checked 2026-09-11 while
   building 0.2.14: the AppImage bundles `libgstsubparse.so`, and the .deb/.rpm
   depend on `gstreamer1.0-plugins-base` / `gstreamer1-plugins-base`, which
   ship it. So Linux would render, not hang, and option 2 is safe on all
   three platforms as built today.

Either way, a receiver without a subtitle toggle rendering captions the sender
did not ask for is a product question the owner has not answered.

**Found while checking:** the Linux AppImage had the same defect Windows 0.2.13
had — HLS plugins bundled, no GIO TLS backend (`libgiognutls.so` is not a
GStreamer plugin, so `linuxdeploy --plugin gstreamer` never sees it), and the
.deb/.rpm did not depend on `glib-networking`. Fixed in `build-appimage.sh`
(module deployed under `usr/lib/gio/modules`, `GIO_EXTRA_MODULES` set by an
AppRun hook, build fails if absent) and in both package scripts, for 0.2.14.
Nobody had cast AirPlay video to the Linux build; it would have died with
"Couldn't download fragments".

## Upstream PRs sent (2026-09-10)

Two of the day's fork fixes were rewritten in upstream form — against
`FDH2/UxPlay` master `366dd5e`, no library-mode code, `exit(1)` where upstream
uses it — re-verified on a pristine master build, audited by Codex, and opened
from the owner's account:

* **#568** — Reject trailing and malformed command-line values instead of
  crashing on them. `uxplay.cpp`, +39/−7, seven defects in `parse_arguments()`.
  Branch `Recluse/UxPlay:reject-malformed-cli-values` (local `pr2-on-master`).
* **#569** — Unref the system clock once, not once per audio format.
  `renderers/audio_renderer.c`, +11/−1.
  Branch `Recluse/UxPlay:unref-system-clock-once` (local `pr3-on-master`).

**Withdrawn before sending, and why it matters here:** the
`adjust_yt_condensed_playlist` buffer fix. Upstream issue #563 (2026-08-26)
reported the same function; the maintainer replied the sizing defect did not
exist — correct for `PREFIX="s/"`, the only value senders emit, where the
discrepancy `prefix_len − 2` is zero — and then rewrote the function
(`575c562`, 2026-08-28) with the exact formula. Two consequences for us: the fix
in this fork is correct but a no-op on real YouTube traffic (measured: the real
playlist from #563 through the old code gives `slack=0`, identical bytes), so
the fork commit `ebffb05`'s claim that it alone made video play was
unsupported — and the Windows 2×2 re-test then refuted it (table in the video
section above); and our fork is 54 commits behind master and needs rebasing to
pick that rewrite up.

Also not sent: `-march=native` (upstream already prints a loud warning; ours to
read) and the WebVTT/TEXT hang (needs a trimmed GStreamer; the trimming is ours).

Upstream's merged external PRs (#558, #559, #562) set the expected shape: an
imperative title; symptom, cause, fix, before/after in the body; no sign-off.
