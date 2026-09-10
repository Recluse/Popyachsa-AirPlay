#!/usr/bin/env bash
# make-app.sh — assemble a self-contained "Popyachsa AirPlay.app" on macOS.
#
# Produces dist/Popyachsa AirPlay.app with a TRIMMED, RELOCATED GStreamer runtime
# bundled inside, so the app runs on a clean Mac with NO GStreamer.framework
# installed. Ad-hoc codesigned (runnable on Apple Silicon) but NOT Developer-ID
# signed/notarized — Gatekeeper needs a right-click->Open on first launch.
#
# Prereqs (this Mac): the built dylibs (build-core-arm64.sh AND, for a shippable
# universal2 build, build-core-x86_64.sh) + Rust toolchain + the official
# GStreamer.framework in /Library/Frameworks (the bundling SOURCE).
# See BUILD-MACOS.md.
set -euo pipefail

# ---- inputs / config ---------------------------------------------------------
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
APPDIR="$REPO/app"
VERSION="${VERSION:-$(grep -m1 '^version' "$APPDIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')}"
TARGET="${TARGET:-aarch64-apple-darwin}"
DYLIB="${DYLIB:-$HOME/uxplay-mac-build/UxPlay/build-arm64/uxplay-core.dylib}"
FRAMEWORK="/Library/Frameworks/GStreamer.framework"
GST_LIB="$FRAMEWORK/Versions/1.0/lib"
GST_PLUGINS="$GST_LIB/gstreamer-1.0"
GST_SCANNER="$FRAMEWORK/Versions/1.0/libexec/gstreamer-1.0/gst-plugin-scanner"
ICON_PNG="$REPO/packaging/shared/com.popyachsa.AirPlay.png"
COPYING="$REPO/packaging/shared/COPYING"

# universal2: point these at a PREBUILT x86_64 dylib + Rust binary and the
# corresponding arm64 halves get lipo'd together in step 2b. Unset -> arm64-only
# .app (fine for local dev; NOT what ships). See build-core-x86_64.sh.
X86_DYLIB="${X86_DYLIB:-$HOME/uxplay-mac-build/UxPlay/build-x86_64/uxplay-core.dylib}"
X86_BIN="${X86_BIN:-$APPDIR/target/x86_64-apple-darwin/release/popyachsa-airplay}"

APP_NAME="Popyachsa AirPlay"
BUNDLE_ID="com.popyachsa.AirPlay"
OUT="$REPO/build/macos/dist"
APP="$OUT/$APP_NAME.app"

# GStreamer plugins to bundle.
#
# ⚠ HOW NOT TO MAINTAIN THIS LIST: the first two blocks were captured by lsof'ing
# a live SCREEN MIRRORING session. That method looks authoritative and is not — it
# can only ever see the ONE protocol that happened to be running. AirPlay carries
# video two ways, and a mirror session never loads a single plugin of the second
# one, so the whole HLS block below was silently missing from 0.2.12 and 0.2.13:
# casting a video (YouTube & co) left the receiver looking hung. Add plugins for
# the protocol you are supporting, from the code path that uses them, and say what
# breaks without each — do not re-derive the list from a running process.
PLUGINS=(
  # SCREEN MIRRORING (raop_rtp_mirror -> h264/h265 -> our avlayer appsink sink)
  # + the audio path. Verified in the field.
  app applemedia audioconvert audioresample autodetect coreelements
  level libav videoconvertscale videofilter videoparsersbad volume
  osxaudio typefindfunctions audioparsers playback

  # AirPlay VIDEO PROTOCOL (on_video_play -> playbin3 on an m3u8, uxplay -hls).
  # None of these are reachable from a mirror session; all four are required, and
  # the failure modes are ugly (silent hang, or an error only g_print'd to
  # engine.log). Measured against Apple's reference HLS streams, in this bundle:
  #   adaptivedemux2  hlsdemux2 (rank 257 -- beats the legacy `hls` plugin's
  #                   hlsdemux at 256, which is why `hls` is NOT here; it also
  #                   carries dashdemux2/mssdemux2, so `dash` is not either).
  #                   Without it: "urisourcebin: your GStreamer installation is
  #                   missing a plug-in", nothing plays.
  #   soup            souphttpsrc (rank 256; `curl`'s curlhttpsrc is 128 and would
  #                   never be picked, so `curl` is dead weight). Serves BOTH the
  #                   engine's own http://localhost/master.m3u8 and the CDN
  #                   segments. Without it: "No URI handler implemented for http".
  #                   NB the plugin does not LINK libsoup, it dlopens it by leaf
  #                   name -- see the hand-seeded files in step 3.
  #   mpegtsdemux     tsdemux, for HLS whose segments are MPEG-TS. Without it:
  #                   "decodebin3 ... missing a plug-in".
  #   isomp4          qtdemux, for HLS whose segments are fMP4/CMAF. Both
  #                   containers occur in the wild; without it that half of the
  #                   senders get "Missing element: Quicktime demuxer".
  # NOT here on purpose: `subparse`/`pango`. A master playlist advertising WebVTT
  # subtitles used to hang playbin at "buffering 0%" forever; the fix is one line
  # in the fork (video_renderer.c clears GST_PLAY_FLAG_TEXT -- a receiver has no
  # subtitle UI), which costs 0 MB instead of pango's ~13 MB font stack.
  adaptivedemux2 soup mpegtsdemux isomp4
)

echo "==> Popyachsa AirPlay.app  v$VERSION  ($TARGET)"
[ -f "$DYLIB" ] || { echo "missing dylib: $DYLIB (run build-core-arm64.sh)"; exit 1; }
[ -d "$FRAMEWORK" ] || { echo "missing $FRAMEWORK (install official GStreamer.framework)"; exit 1; }

# ---- 1. release binary -------------------------------------------------------
echo "==> cargo build --release"
( cd "$APPDIR" && PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build --release --target "$TARGET" )
BIN="$APPDIR/target/$TARGET/release/popyachsa-airplay"
# An x86_64 engine slice is the signal that this is a release (universal) build,
# so build the matching Rust half too — a fat dylib next to a thin arm64 exe is
# still an app that will not launch on Intel. Needs: rustup target add x86_64-apple-darwin.
if [ -f "$X86_DYLIB" ]; then
  echo "==> cargo build --release --target x86_64-apple-darwin"
  ( cd "$APPDIR" && PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build --release --target x86_64-apple-darwin )
fi

# ---- 2. skeleton -------------------------------------------------------------
echo "==> assembling bundle"
rm -rf "$APP"
C="$APP/Contents"
# The GStreamer tree lives in Contents/Resources and is reached through a SYMLINK
# at Contents/Frameworks/GStreamer.
#
# Why: codesign treats every directory directly under Contents/Frameworks as a
# nested bundle and refuses any plain sub-directory inside one — "bundle format
# unrecognized ... In subcomponent: .../GStreamer/lib/gstreamer-1.0" — which is
# what silently failed the seal on every release up to 0.2.12 (the old script
# discarded the error). A symlink is sealed as one resource and never descended,
# while dyld resolves @rpath/@loader_path straight through it, so the paths
# engine_macos.rs builds are unchanged.
#
# ⚠ RELEASE ORDERING: the first bundle that ships this symlink must NOT be the
# same release that teaches the in-app updater to recreate symlinks. The updater
# shipped in 0.2.12 writes a zip symlink entry as a 22-byte regular file, leaving
# the engine unloadable. Ship the symlink-aware updater first, let it reach the
# field, and only then ship a symlinked bundle. See the release runbook
# ponytail: symlink instead of repackaging GStreamer as a real .framework —
# upgrade to a proper Versions/A framework bundle if notarization ever objects.
GST_ROOT="$C/Resources/GStreamer"
DEST_LIB="$GST_ROOT/lib"
DEST_PLUGINS="$DEST_LIB/gstreamer-1.0"
DEST_LIBEXEC="$GST_ROOT/libexec/gstreamer-1.0"
mkdir -p "$C/MacOS" "$C/Frameworks" "$DEST_PLUGINS" "$DEST_LIBEXEC" "$C/Resources"
# GST_SYMLINK=1 adds it. Default OFF: the symlink may only ship AFTER a release
# whose updater can recreate symlinks has reached the field (see the ⚠ above).
# Flip this to 1 for the first release after that, and update the release runbook
if [ "${GST_SYMLINK:-0}" = "1" ]; then
  ln -s ../Resources/GStreamer "$C/Frameworks/GStreamer"
  echo "==> Contents/Frameworks/GStreamer symlink: ON"
else
  echo "==> Contents/Frameworks/GStreamer symlink: off (transitional release)"
fi
cp "$BIN" "$C/MacOS/popyachsa-airplay"
cp "$DYLIB" "$C/MacOS/uxplay-core.dylib"
chmod +w "$C/MacOS/uxplay-core.dylib"

# ---- 2b. universal2 (lipo the x86_64 halves in) ------------------------------
# Done HERE, before the rpath surgery and the dependency walk, so install_name_tool
# rewrites BOTH slices in one pass (it edits every arch of a fat Mach-O) and the
# BFS below sees the same @rpath names either slice would ask for — GStreamer's
# own framework is already universal.
# Only the two files we build are thin; everything copied out of
# GStreamer.framework is fat already.
fatten() {                       # $1 = file inside the bundle, $2 = x86_64 twin
  [ -f "$2" ] || { echo "   WARN: no x86_64 slice at $2 — shipping arm64-only"; return 0; }
  # Two statements, NOT `lipo ... && mv ...`: the left side of an && is exempt
  # from set -e, and the chmod after it returns 0, so a failed lipo would leave a
  # thin binary behind and the build would carry on and sign it.
  lipo -create "$1" "$2" -output "$1.fat"
  mv "$1.fat" "$1"
  chmod +w "$1"
}
fatten "$C/MacOS/popyachsa-airplay" "$X86_BIN"
fatten "$C/MacOS/uxplay-core.dylib" "$X86_DYLIB"
lipo -archs "$C/MacOS/popyachsa-airplay" | sed 's/^/   app archs: /'
lipo -archs "$C/MacOS/uxplay-core.dylib" | sed 's/^/   core archs: /'

# ---- 3. bundle the GStreamer runtime (plugins + transitive lib closure) ------
# All framework libs/plugins use @rpath install names and carry an
# @loader_path/../lib rpath, so preserving the lib/ + lib/gstreamer-1.0/ layout
# means they find each other with NO per-lib surgery.
echo "==> bundling ${#PLUGINS[@]} plugins + their dependency closure"
queue=()
for p in "${PLUGINS[@]}"; do
  src="$GST_PLUGINS/libgst$p.dylib"
  # A plugin named here and absent from the framework is a BUILD FAILURE, not a
  # warning. This gate is the one that had to catch the HLS omission and did not:
  # the missing plugins were never named, so nothing warned — but a warning would
  # not have helped either, since it scrolls past in a hundred lines of output and
  # the bundle ships anyway. If it is in this list, the app needs it.
  [ -f "$src" ] || { echo "   FATAL: plugin libgst$p.dylib not in $GST_PLUGINS"; exit 1; }
  cp -p "$src" "$DEST_PLUGINS/"; queue+=( "$src" )
done
# seed with uxplay-core's own @rpath deps too
queue+=( "$C/MacOS/uxplay-core.dylib" )

# Two runtime-loaded files that NOTHING links, so the @rpath BFS below can never
# reach them and the PLUGINS loop above cannot carry them either (it only ever
# copies lib/gstreamer-1.0/libgst<name>.dylib). Both are mandatory for the HLS
# path; both are seeded into the queue, not just copied, so their own closures
# (libssl/libcrypto/libpsl/libnghttp2) come along.
#
#  libsoup-3.0.0.dylib — `otool -L libgstsoup.dylib` lists NO soup dependency:
#    the plugin g_module_open()s it by bare leaf name (gstsouploader.c), and dyld
#    resolves a leaf-name dlopen through the CALLING image's LC_RPATH, i.e. the
#    plugin's own @loader_path/../lib -> DEST_LIB. libgstadaptivedemux2.dylib
#    contains the same loader, so this is required even if `soup` is ever dropped.
#  gio/modules/libgioopenssl.so — glib-networking's GIO TLS backend. NOT a
#    GStreamer plugin and NOT in lib/gstreamer-1.0/. Without it every https://
#    HLS segment fetch fails with "TLS support is not available" surfacing as
#    "Internal data stream error ... reason error (-5)". It must keep the
#    framework's lib/gio/modules/ layout: its own @loader_path/../../../lib rpath
#    then lands on DEST_LIB, and glib locates the module dir from libgio's own
#    image path. No CA bundle is needed — it reads the macOS keychain
#    (links Security.framework).
mkdir -p "$DEST_LIB/gio/modules"
for extra in "libsoup-3.0.0.dylib" "gio/modules/libgioopenssl.so"; do
  [ -f "$GST_LIB/$extra" ] || { echo "missing $GST_LIB/$extra — HLS video would ship broken"; exit 1; }
  cp -p "$GST_LIB/$extra" "$DEST_LIB/$extra"
  chmod +w "$DEST_LIB/$extra"
  queue+=( "$GST_LIB/$extra" )
done

# BFS the @rpath dependency closure into DEST_LIB (flat, like the framework).
while [ ${#queue[@]} -gt 0 ]; do
  src="${queue[0]}"; queue=( "${queue[@]:1}" )
  # Captured into a variable, NOT piped in through `< <(...)`: a process
  # substitution's exit status never reaches the enclosing while, so a failed
  # otool would read as "no dependencies" and ship a silently truncated closure.
  # An assignment does propagate it (with pipefail, through the pipe as well).
  deps="$(otool -L "$src" | tail -n +2 | awk '{print $1}')"
  while IFS= read -r dep; do
    case "$dep" in
      @rpath/*)
        db="${dep#@rpath/}"
        [ -f "$DEST_LIB/$db" ] && continue
        if [ -f "$GST_LIB/$db" ]; then cp -p "$GST_LIB/$db" "$DEST_LIB/$db"; chmod +w "$DEST_LIB/$db"; queue+=( "$GST_LIB/$db" )
        else echo "   WARN: @rpath dep $db of $(basename "$src") not in $GST_LIB — NOT bundled"; fi
        ;;
    esac
  done <<< "$deps"
done
echo "   bundled $(ls "$DEST_LIB"/*.dylib | wc -l | tr -d ' ') libs, $(ls "$DEST_PLUGINS"/*.dylib | wc -l | tr -d ' ') plugins"

# gst-plugin-scanner (run out-of-process by GStreamer) + its lib closure.
if [ -f "$GST_SCANNER" ]; then
  cp -p "$GST_SCANNER" "$DEST_LIBEXEC/"; chmod +w "$DEST_LIBEXEC/gst-plugin-scanner"
  # scanner links @rpath libs too — they're already in DEST_LIB; just add its rpath.
  install_name_tool -add_rpath "@loader_path/../../lib" "$DEST_LIBEXEC/gst-plugin-scanner" 2>/dev/null || true
fi

# ---- 4. point uxplay-core.dylib at the bundled libs --------------------------
# Strip the build-time ABSOLUTE rpath to the system /Library framework, so the
# bundled libs are the ONLY source — true self-containment (and testable on this
# Mac, where /Library/Frameworks/GStreamer would otherwise win the search order).
otool -l "$C/MacOS/uxplay-core.dylib" | grep -A2 LC_RPATH | grep ' path ' | awk '{print $2}' | while read -r rp; do
  case "$rp" in
    *GStreamer.framework*|/Library/*|/opt/*)
      install_name_tool -delete_rpath "$rp" "$C/MacOS/uxplay-core.dylib" 2>/dev/null || true ;;
  esac
done
# BOTH layouts, unconditionally: Resources is where the tree always is, Frameworks
# is the optional symlink (step 2's GST_SYMLINK). dyld tries each rpath in order
# and ignores one that resolves to nothing, so shipping both costs a failed stat
# and removes an entire class of "the bundle layout changed and the rpath didn't"
# breakage. engine_macos::set_bundled_gst_env probes the same two paths.
install_name_tool -add_rpath "@loader_path/../Resources/GStreamer/lib" "$C/MacOS/uxplay-core.dylib"
install_name_tool -add_rpath "@loader_path/../Frameworks/GStreamer/lib" "$C/MacOS/uxplay-core.dylib"

# ---- 5. Info.plist (incl. local-network privacy keys for mDNS) ---------------
ICNS="AppIcon.icns"
cat > "$C/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>popyachsa-airplay</string>
  <key>CFBundleIconFile</key><string>$ICNS</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSUIElement</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Popyachsa AirPlay receives AirPlay (screen mirroring + audio) from devices on your local network.</string>
  <key>NSBonjourServices</key>
  <array><string>_airplay._tcp</string><string>_raop._tcp</string></array>
</dict>
</plist>
PLIST

# ---- 6. icon (png -> icns) ---------------------------------------------------
if [ -f "$ICON_PNG" ]; then
  ISET="$(mktemp -d)/AppIcon.iconset"; mkdir -p "$ISET"
  for s in 16 32 64 128 256 512; do
    sips -z $s $s        "$ICON_PNG" --out "$ISET/icon_${s}x${s}.png"     >/dev/null 2>&1 || true
    sips -z $((s*2)) $((s*2)) "$ICON_PNG" --out "$ISET/icon_${s}x${s}@2x.png" >/dev/null 2>&1 || true
  done
  iconutil -c icns "$ISET" -o "$C/Resources/$ICNS" 2>/dev/null || echo "   WARN: iconutil failed (icon optional)"
fi

# ---- 6b. licence (GPL-3 §4: recipients must get a copy of the licence) -------
[ -f "$COPYING" ] || { echo "missing $COPYING — GPL-3 text must ship with the binaries"; exit 1; }
cp "$COPYING" "$C/Resources/COPYING"

# ---- 7. ad-hoc codesign (needed for Apple Silicon to load modified Mach-Os) --
# NOT Developer ID / notarized — Gatekeeper still requires right-click->Open.
echo "==> ad-hoc codesign"

# AppleDouble ._* siblings carry no code and are not in any seal, so codesign
# --verify rejects them as unsealed content the moment the bundle IS sealed.
# They arrive from cp -p across volumes and from the updater's unzip
# (update_macos.rs strips nothing). Kill them before signing, not after.
find "$APP" -name '._*' -delete

# Sign INSIDE-OUT by hand instead of --deep (Apple deprecated --deep for signing
# anyway): every nested Mach-O first, then the bundle. The main executable is
# deliberately EXCLUDED from the first pass — codesign resolves
# Contents/MacOS/<CFBundleExecutable> back to the enclosing bundle and would try
# to seal the whole .app before its nested code exists. The bundle pass signs it.
#
# No 2>/dev/null and no || true anywhere below: a bundle that did not get sealed
# must fail the build, not print "done" — that suppression is why every release
# up to 0.2.12 shipped with Sealed Resources=none. Swap `-` for a Developer ID
# identity here (see BUILD-MACOS.md); this is the order notarization needs too.
# `-o -name '*.dylib' -o -name '*.so'`: the set is "every Mach-O", not "every file
# with +x". cp -p carries the source mode over from GStreamer.framework, so a 0644
# dylib there would land here unsigned and be skipped without a word — and the GIO
# TLS module is a `.so`, which today is 0755 and would be caught by luck, not by
# this predicate meaning what it says.
find "$APP" -type f \( -perm +111 -o -name '*.dylib' -o -name '*.so' \) ! -path "$C/MacOS/popyachsa-airplay" \
  -exec codesign --force --sign - {} +
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"
echo "   sealed ok"

echo "==> done: $APP"
du -sh "$APP" | cut -f1 | sed 's/^/    size: /'
# Static, and it actually answers the question. The old hint here was
#   DYLD_PRINT_LIBRARIES=1 ... | grep -i /Library/Frameworks
# which is wrong twice: `-i /Library/Frameworks` also matches every
# /System/Library/Frameworks line, so it reports ~219 "leaks" on a perfectly
# self-contained bundle; and a run that does not start the engine never loads
# GStreamer at all, so a clean result proved nothing either.
echo "==> self-containment + universal2"
leaks=0
thin=0
count=0
# `-name '*.so'` too: the GIO TLS module is a Mach-O with a .so extension, and a
# gate that silently skips it is worse than no gate.
while IFS= read -r f; do
  case "$(file -b "$f")" in
    Mach-O*|*"universal binary"*)
      count=$((count + 1))
      if otool -L "$f" 2>/dev/null | grep -q '^	/Library/Frameworks'; then
        echo "   LEAK: ${f#$APP/} still links the system GStreamer:"
        otool -L "$f" | grep '^	/Library/Frameworks' | sed 's/^/     /'
        leaks=$((leaks + 1))
      fi
      # Every shipped Mach-O must carry BOTH slices: one arm64-only file anywhere
      # in the closure is an app that dies on Intel at dlopen time, with the
      # failure hidden inside GStreamer's plugin loader.
      archs="$(lipo -archs "$f" 2>/dev/null || echo unknown)"
      case " $archs " in
        *" x86_64 "*) case " $archs " in *" arm64 "*) ;; *) echo "   THIN: ${f#$APP/} is '$archs'"; thin=$((thin + 1));; esac ;;
        *) echo "   THIN: ${f#$APP/} is '$archs'"; thin=$((thin + 1)) ;;
      esac ;;
  esac
done < <(find "$APP" -type f \( -perm +111 -o -name '*.dylib' -o -name '*.so' \))
[ "$leaks" -eq 0 ] || { echo "   $leaks file(s) would break on a machine without GStreamer installed"; exit 1; }
# Only a UNIVERSAL build has to be universal. `fatten()` warns and continues when
# X86_DYLIB is absent, and the header calls an arm64-only bundle "fine for local
# dev", so failing here would make every dev build exit 1 for doing exactly what
# the script says it may do. Same signal step 2b uses to decide it is a release.
if [ -f "$X86_DYLIB" ]; then
  [ "$thin" -eq 0 ] || { echo "   $thin file(s) are not universal — this bundle would break on Intel"; exit 1; }
  echo "   $count Mach-O files: no /Library/Frameworks dependencies, all arm64+x86_64"
else
  echo "   $count Mach-O files: no /Library/Frameworks dependencies"
  echo "   (arm64-only dev build — universal2 gate skipped, $thin thin file(s))"
fi
