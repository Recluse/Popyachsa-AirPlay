# Building the macOS version (end-to-end)

How to produce the shippable **Popyachsa AirPlay.app** + **DMG** for macOS
(Apple Silicon). This is the full path: patched-UxPlay `uxplay-core.dylib` →
Rust tray app → self-contained `.app` (GStreamer bundled) → DMG.

> **TL;DR (on a Mac, after the one-time M0 setup below):**
> ```bash
> bash build/macos/build-core-arm64.sh   # 1. uxplay-core.dylib
> bash build/macos/make-app.sh           # 2. Popyachsa AirPlay.app (self-contained)
> bash build/macos/make-dmg.sh           # 3. Popyachsa-AirPlay-<ver>.dmg
> ```
> Output: `build/macos/dist/`.

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

## M0 — one-time toolchain

```bash
xcode-select --install                        # Xcode Command Line Tools (clang, codesign, …)
brew install rustup cmake ninja pkg-config openssl@3 libplist
rustup default stable
rustup target add aarch64-apple-darwin        # (+ x86_64-apple-darwin for universal2 later)

# Official UNIVERSAL GStreamer.framework (runtime + devel), verify SHA256, install to
# /Library/Frameworks (this is BOTH the link target and the bundling SOURCE):
#   https://gstreamer.freedesktop.org/data/pkg/osx/  ->  gstreamer-1.0-<ver>-universal.pkg
#                                                        gstreamer-1.0-devel-<ver>-universal.pkg
#   sudo installer -pkg <runtime>.pkg -target /
#   sudo installer -pkg <devel>.pkg   -target /
```

The custom `avlayer` sink does **not** use GStreamer's applemedia sinks, so the
**stable 1.28.4** framework is sufficient (validated live on 1.29.1).

## 1 — `uxplay-core.dylib`

Patched UxPlay fork `fc126fd` + our patches. See `README.md` for the source-tree
prep (`git apply clean-full-vs-v1.73.6.diff`, copy `airplay_core.{h,cpp}` →
`lib/`, `avsample_sink.m` → `renderers/`), then:

```bash
bash build/macos/build-core-arm64.sh    # -> third_party/uxplay/build-arm64/uxplay-core.dylib
```

## 2 — `Popyachsa AirPlay.app` (self-contained)

```bash
bash build/macos/make-app.sh
```

What it does:

- `cargo build --release` the tray app;
- assembles `Contents/{MacOS,Frameworks,Resources}`;
- **bundles a trimmed GStreamer runtime**: the exact plugin set a live mirror
  session loads (captured via `lsof`) + the macOS audio sink + safety parsers,
  plus the transitive `@rpath` dylib closure, preserving the framework's
  `lib/` + `lib/gstreamer-1.0/` layout (so the libs' built-in
  `@loader_path/../lib` rpaths resolve with no per-lib surgery);
- **strips** the dylib's build-time absolute rpath to `/Library/Frameworks` and
  adds `@loader_path/../Frameworks/GStreamer/lib` — the bundle becomes the *only*
  GStreamer source (true self-containment);
- writes `Info.plist` incl. **`NSLocalNetworkUsageDescription` + `NSBonjourServices`**
  (`_airplay._tcp` / `_raop._tcp`) — required or macOS 15 blocks mDNS discovery;
- `LSUIElement` (menu-bar app, no Dock icon);
- generates the icon, and **ad-hoc** codesigns (required for Apple Silicon to load
  the relocated Mach-Os — this is *not* Developer-ID signing).

The app sets `GST_PLUGIN_SYSTEM_PATH_1_0` / `GST_PLUGIN_SCANNER_1_0` /
`GST_REGISTRY_1_0` (a user-writable registry) at startup when it detects it is
running from a `.app` (`engine_macos::set_bundled_gst_env`).

**Verify self-containment** (no system GStreamer reached):

```bash
APP="build/macos/dist/Popyachsa AirPlay.app"
otool -l "$APP/Contents/MacOS/uxplay-core.dylib" | grep -A2 LC_RPATH | grep ' path '   # ONLY @loader_path/...
# launch, then:  lsof -p <pid> | grep -c '/Library/Frameworks/GStreamer'   # -> 0
#                lsof -p <pid> | grep -c 'Contents/Frameworks/GStreamer'    # -> many
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
    - bash build/macos/build-core-arm64.sh
    - bash build/macos/make-app.sh
    - bash build/macos/make-dmg.sh
  artifacts:
    paths: [build/macos/dist/*.dmg]
    expire_in: 30 days
```

(For Developer-ID builds, inject the `.p12` + an App Store Connect API key as
masked CI variables and replace the ad-hoc `codesign` + add a notarize step.)

---

## Remaining packaging work (M5–M11)

- **universal2**: currently arm64-only (brew openssl/libplist are arm64). Build
  fat openssl/libplist (`lipo`) + `-DCMAKE_OSX_ARCHITECTURES="arm64;x86_64"`.
- **Developer ID** codesign + **notarize** + staple (needs an Apple account).
- Trim further / verify the plugin set on a clean Mac (no GStreamer installed).
