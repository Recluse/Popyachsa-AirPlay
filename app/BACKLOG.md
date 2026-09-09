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

**Status:** implemented, **not merged** — three live gates below are unrun.
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

## The engine log loses exactly the lines you need after a hang

**Status:** confirmed while diagnosing something else, not fixed.
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

## The AirPlay *video* protocol cannot work in the macOS bundle — no HLS plugins

**Status:** diagnosed, not fixed. Pre-existing; **the shipped 0.2.12 has it too**.
**Found:** 2026-09-09, when sending a video appeared to hang the receiver.

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

**4. `status.rs:75` can stop a healthy engine on client-controlled text.**
The marker is `contains("failed with error code")`, and `lib/raop.c` logs client
plists through the same hook. The correct anchor is
`starts_with("dnssd_register")` **combined** with the phrase — the two real lines are
`dnssd_register_raop failed with error code %d` and `dnssd_register_airplay …`, so a
naive `starts_with("failed with error code")` silently breaks the P1 match instead.

**5. Shim: `DNSServiceRefDeallocate()` frees after a timeout.**
`dnssd_shim.c` waits 3000 ms for the responder thread, then `CloseHandle` + `free`
unconditionally. If the thread has not exited, that is a use-after-free plus a leaked
socket. Pre-existing, unrelated to the adapter-selection work, lives in
[[airplay-where-things-live]]'s shim repo.

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
