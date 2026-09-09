# Building the macOS version (end-to-end)

How to produce the shippable **universal2 Popyachsa AirPlay.app** + **DMG** for
macOS (Apple Silicon *and* Intel). This is the full path: patched-UxPlay
`uxplay-core.dylib` (both arches) → Rust tray app (both arches) → `lipo` →
self-contained `.app` (GStreamer bundled) → DMG.

> **TL;DR (on a Mac, after the one-time toolchain setup below):**
> ```bash
> bash build/macos/build-x86-deps.sh     # 0. x86_64 static openssl+libplist (once)
> bash build/macos/build-core-arm64.sh   # 1a. uxplay-core.dylib  (arm64)
> bash build/macos/build-core-x86_64.sh  # 1b. uxplay-core.dylib  (x86_64)
> bash build/macos/make-app.sh           # 2. universal .app (lipo + seal)
> bash build/macos/make-dmg.sh           # 3. Popyachsa-AirPlay-<ver>.dmg
> ```
> Output: `build/macos/dist/`.
>
> (Step 4, publishing the zip and the signed update feed, runs from a script that
> is not in this repo: it holds the maintainer's host addresses and deploy keys.
> Everything needed to VERIFY a published build is here — see the signature rules
> in `app/tools/make-update.py`.)
>
> Skipping step 0/1b gives a working **arm64-only** app for local dev, with a
> warning from `make-app.sh` — never ship that; it will not launch on Intel.

---

## ⚠️ Why this does NOT build on a Linux runner

macOS artifacts need macOS tooling that has no native Linux equivalent:

- the dylib's custom sink (`avsample_sink.m`) links **AVFoundation / CoreMedia /
  CoreVideo / AppKit**;
- the runtime is the official **GStreamer.framework** — a 3.8 GB *native macOS*
  framework bundle;
- `.app` assembly, `install_name_tool`/rpath relocation, and `hdiutil` DMGs are
  macOS-only;
- code-signing/notarization are Apple-only.

On a **Linux** GitLab runner the only route is **osxcross** cross-compilation
(macOS SDK + cctools + a staged GStreamer.framework + `rcodesign` + a Linux DMG
tool). It is heavy, fragile, legally awkward (the SDK), and unvalidated here — so
it is **not** the supported path.

**Supported paths**

1. **Local build on a Mac** (this guide) — what we ship today. Run the three
   scripts, attach the DMG to a GitLab Release by hand (below).
2. **A macOS CI runner** (GitLab SaaS macOS, or a self-hosted Mac with
   `gitlab-runner`) — runs exactly these three scripts. The recommended CI path
   when a Mac runner is available; a ready-to-drop job is in *CI* below.

---

## One-time toolchain

```bash
xcode-select --install                        # Xcode Command Line Tools (clang, codesign, …)
brew install rustup cmake ninja pkg-config openssl@3 libplist
rustup default stable
rustup target add aarch64-apple-darwin x86_64-apple-darwin   # both — the release is universal2

# Official UNIVERSAL GStreamer.framework (runtime + devel), verify SHA256, install to
# /Library/Frameworks (this is BOTH the link target and the bundling SOURCE):
#   https://gstreamer.freedesktop.org/data/pkg/osx/  ->  gstreamer-1.0-<ver>-universal.pkg
#                                                        gstreamer-1.0-devel-<ver>-universal.pkg
#   sudo installer -pkg <runtime>.pkg -target /
#   sudo installer -pkg <devel>.pkg   -target /
```

The custom `avlayer` sink does **not** use GStreamer's applemedia sinks, so the
**stable 1.28.4** framework is sufficient (validated live on 1.29.1).

## 0 — x86_64 static dependencies (once)

```bash
bash build/macos/build-x86-deps.sh      # -> ~/uxplay-mac-build/x86deps/prefix
```

Homebrew on Apple Silicon has **arm64-only** bottles, so there is no x86_64
openssl or libplist to link the Intel slice against. This builds both from
source, static, into a contained prefix (`--prefix=~/uxplay-mac-build/x86deps/prefix`),
hash-checking the tarballs. Everything else the dylib needs — GStreamer.framework
and the system frameworks — is already universal. Re-run only when a pinned
version in the script changes.

## 1 — `uxplay-core.dylib` (both arches)

Patched UxPlay fork `fc126fd` + our patches. See `README.md` for the source-tree
prep (`git apply clean-full-vs-v1.73.6.diff`, copy `airplay_core.{h,cpp}` →
`lib/`, `avsample_sink.m` → `renderers/`), then:

```bash
bash build/macos/build-core-arm64.sh    # -> ~/uxplay-mac-build/UxPlay/build-arm64/uxplay-core.dylib
bash build/macos/build-core-x86_64.sh   # -> ~/uxplay-mac-build/UxPlay/build-x86_64/uxplay-core.dylib
```

Two separate build trees rather than `CMAKE_OSX_ARCHITECTURES="arm64;x86_64"`:
the two arches need *different* dependency prefixes (Homebrew vs. the contained
static prefix), which one cmake configure cannot express. `build-core-x86_64.sh`
ends by *asserting* `lipo -archs` is exactly `x86_64` (it used to only print it).
The x86_64 configure also passes `-DCMAKE_IGNORE_PATH=/opt/homebrew/lib`, without
which `find_library` hands back Homebrew's **arm64** dylibs and the link fails
late, or worse, succeeds — plus `-U LIBCRYPTO -U LIBPLIST`, because those are
`find_library` cache entries and a cached hit is never re-searched, so neither
the prefix nor the ignore-path above would take effect in a build tree that was
configured once without them.

**If the build suddenly fails with `openssl/sha.h file not found`** in `pairing.c`,
`crypto.c` and `srp.c` — nothing is wrong with the sources. Homebrew bumped
`openssl@3` and deleted the old Cellar directory, while CMake still has the old
versioned path in its cache (the paths come from `pkg_check_modules`, which does not
re-run once cached). Both build trees need the openssl entries dropped so they are
found again:

```bash
for B in build-arm64 build-x86_64; do
  cmake -S ~/uxplay-mac-build/UxPlay -B ~/uxplay-mac-build/UxPlay/$B \
        -U 'OPENSSL_*' -U '__pkg_config_checked_OPENSSL' -U 'pkgcfg_lib_OPENSSL_*'
done
```

Everything else in the cache (`BUILD_CORE_DLL`, the arch, the x86 contained prefix)
survives. Note the x86_64 slice takes its *static* `libcrypto.a` from
`~/uxplay-mac-build/x86deps/prefix` while its *headers* come from Homebrew, so after
a bump those are one patch release apart until the contained prefix is rebuilt —
fine within OpenSSL 3.x, where patch releases are ABI-stable.

## 2 — `Popyachsa AirPlay.app` (universal, self-contained)

```bash
bash build/macos/make-app.sh
```

What it does:

- `cargo build --release` the tray app — for `x86_64-apple-darwin` as well when
  an Intel engine slice exists, then `lipo -create`s both halves so the exe and
  the dylib are fat. It prints `app archs:` / `core archs:`; if either says only
  `arm64`, the Intel build is missing and the result is not shippable;
- assembles `Contents/{MacOS,Frameworks,Resources}`;
- **bundles a trimmed GStreamer runtime**: the exact plugin set a live mirror
  session loads (captured via `lsof`) + the macOS audio sink + safety parsers,
  plus the transitive `@rpath` dylib closure, preserving the framework's
  `lib/` + `lib/gstreamer-1.0/` layout (so the libs' built-in
  `@loader_path/../lib` rpaths resolve with no per-lib surgery);
- **strips** the dylib's build-time absolute rpath to `/Library/Frameworks` and
  adds `@loader_path/../Resources/GStreamer/lib` — the bundle becomes the *only*
  GStreamer source (true self-containment);
- writes `Info.plist` incl. **`NSLocalNetworkUsageDescription` + `NSBonjourServices`**
  (`_airplay._tcp` / `_raop._tcp`) — required or macOS 15 blocks mDNS discovery;
- `LSUIElement` (menu-bar app, no Dock icon);
- installs the full **GPL-3 text** into `Contents/Resources/COPYING` (GPL-3 §4);
- generates the icon, and **ad-hoc** codesigns (required for Apple Silicon to load
  the relocated Mach-Os — this is *not* Developer-ID signing), then
  `codesign --verify --strict`s the result and **fails the build** if it does not
  seal.

**Why the GStreamer tree sits in `Contents/Resources/GStreamer` with a symlink at
`Contents/Frameworks/GStreamer`.** codesign treats every directory directly under
`Contents/Frameworks` as a nested bundle and rejects any plain sub-directory in
one — `bundle format unrecognized … In subcomponent: …/GStreamer/lib/gstreamer-1.0`.
With the error suppressed (as it was through 0.2.12) that produced bundles with
`Sealed Resources=none` and no `_CodeSignature`. A symlink is sealed as a single
resource and never descended, while dyld resolves `@rpath`/`@loader_path` straight
through it — so every path in `engine_macos::set_bundled_gst_env` is unchanged.

The rpath (`@loader_path/../Frameworks/GStreamer/lib`, installed by `make-app.sh`)
and that function's path must stay in step.

> ⚠ **The symlink gates the release order.** The in-app updater shipped in 0.2.12
> writes a zip symlink entry as a 22-byte regular file, so the first bundle
> containing a symlink would leave every already-installed client with an
> unloadable engine. Ship the symlink-aware updater first and let it reach the
> field; only the release AFTER that may contain a symlinked bundle. See
> the release runbook

The app sets `GST_PLUGIN_SYSTEM_PATH_1_0` / `GST_PLUGIN_SCANNER_1_0` /
`GST_REGISTRY_1_0` (a user-writable registry) at startup when it detects it is
running from a `.app` (`engine_macos::set_bundled_gst_env`).

**Verify self-containment** (no system GStreamer reached):

```bash
APP="build/macos/dist/Popyachsa AirPlay.app"
otool -l "$APP/Contents/MacOS/uxplay-core.dylib" | grep -A2 LC_RPATH | grep ' path '   # ONLY @loader_path/...
# launch, then:  lsof -p <pid> | grep -c '/Library/Frameworks/GStreamer'   # -> 0
#                lsof -p <pid> | grep -c 'Contents/Resources/GStreamer'     # -> many
#                              (lsof reports the REAL path, not the Frameworks symlink)
```

**Verify universal + sealed** (both are invisible on this Mac until a user is not
on one):

```bash
lipo -archs "$APP/Contents/MacOS/popyachsa-airplay"   # x86_64 arm64
lipo -archs "$APP/Contents/MacOS/uxplay-core.dylib"   # x86_64 arm64
codesign --verify --strict "$APP" && codesign -dv "$APP" 2>&1 | grep Sealed  # NOT "none"
```

## 3 — DMG

```bash
bash build/macos/make-dmg.sh            # -> dist/Popyachsa-AirPlay-<version>.dmg
```

Drag-to-`/Applications` layout, compressed (UDZO). Prints size + sha256.

---

## Signing / Gatekeeper

There is **no Developer ID certificate**, so the DMG is **unsigned** (ad-hoc
only). First launch of a *downloaded* copy: **right-click → Open** once (or
`xattr -dr com.apple.quarantine "/Applications/Popyachsa AirPlay.app"`). A
locally-built copy is not quarantined and just runs.

The in-app self-updater (`update_macos.rs`) is independent of Apple signing: it
verifies an **Ed25519-signed** `updates-macos.json` + a SHA-256 over the zip
before swapping the `.app`. To enable it, publish `updates-macos.json` (see
`MACOS-UPDATE.md`) pointing at a zipped `.app`.

Gatekeeper-clean distribution later = Developer ID codesign + notarize + staple
(M7+); the scripts have the ad-hoc `codesign` call to swap for a real identity.

---

## Publishing the DMG (no macOS CI runner)

The DMG is built locally; don't commit the binary to git. Attach it to a GitLab
Release:

```bash
VER=$(grep -m1 '^version' app/Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
DMG="build/macos/dist/Popyachsa-AirPlay-$VER.dmg"
# upload to the project's package registry, then link it in a release:
glab release create "macos-v$VER" "$DMG" --name "macOS v$VER" --notes "…"
# or: GitLab UI -> Deploy -> Releases -> New release -> attach $DMG
```

## CI (when a macOS runner exists)

Add to `.gitlab-ci.yml` (runner tagged `macos`, GStreamer.framework pre-installed
on the runner image):

```yaml
build-macos:
  stage: build
  tags: [macos]
  rules:
    - if: $CI_COMMIT_TAG =~ /^macos-v/
  script:
    - bash build/macos/build-x86-deps.sh
    - bash build/macos/build-core-arm64.sh
    - bash build/macos/build-core-x86_64.sh
    - bash build/macos/make-app.sh
    - bash build/macos/make-dmg.sh
  artifacts:
    paths: [build/macos/dist/*.dmg, build/macos/dist/*.zip,
            build/macos/dist/updates-macos.json]
    expire_in: 30 days
```

(For Developer-ID builds, inject the `.p12` + an App Store Connect API key as
masked CI variables and replace the ad-hoc `codesign` + add a notarize step.)

---

## Remaining packaging work (M5–M11)

- **universal2**: done — `build-x86-deps.sh` + `build-core-x86_64.sh` + the `lipo`
  step in `make-app.sh`. the macOS publishing script refuses to publish unless EVERY
  Mach-O in the bundle carries both slices — the bundled GStreamer libs, plugins
  and the scanner included, not just the two binaries we build ourselves.
- **Developer ID** codesign + **notarize** + staple (needs an Apple account).
- Trim further / verify the plugin set on a clean Mac (no GStreamer installed).
