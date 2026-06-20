#!/usr/bin/env bash
# make-app.sh — assemble a self-contained "Popyachsa AirPlay.app" on macOS.
#
# Produces dist/Popyachsa AirPlay.app with a TRIMMED, RELOCATED GStreamer runtime
# bundled inside, so the app runs on a clean Mac with NO GStreamer.framework
# installed. Ad-hoc codesigned (runnable on Apple Silicon) but NOT Developer-ID
# signed/notarized — Gatekeeper needs a right-click->Open on first launch.
#
# Prereqs (this Mac): the built dylib (build-core-arm64.sh) + Rust toolchain +
# the official GStreamer.framework in /Library/Frameworks (the bundling SOURCE).
# See BUILD-MACOS.md.
set -euo pipefail

# ---- inputs / config ---------------------------------------------------------
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APPDIR="$REPO/app"
VERSION="${VERSION:-$(grep -m1 '^version' "$APPDIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')}"
TARGET="${TARGET:-aarch64-apple-darwin}"
# Engine dylib built from the third_party/uxplay submodule (see BUILD.md). Override $DYLIB if elsewhere.
DYLIB="${DYLIB:-$REPO/third_party/uxplay/build/uxplay-core.dylib}"
FRAMEWORK="/Library/Frameworks/GStreamer.framework"
GST_LIB="$FRAMEWORK/Versions/1.0/lib"
GST_PLUGINS="$GST_LIB/gstreamer-1.0"
GST_SCANNER="$FRAMEWORK/Versions/1.0/libexec/gstreamer-1.0/gst-plugin-scanner"
ICON_PNG="$REPO/packaging/shared/com.popyachsa.AirPlay.png"

APP_NAME="Popyachsa AirPlay"
BUNDLE_ID="com.popyachsa.AirPlay"
OUT="$REPO/build/macos/dist"
APP="$OUT/$APP_NAME.app"

# GStreamer plugins to bundle. The first block is the EXACT set captured from a
# live mirror session (lsof on the running app); the second is a safety margin
# (audio sink + typefinding/parsing/playback that can load lazily).
PLUGINS=(
  app applemedia audioconvert audioresample autodetect coreelements
  level libav videoconvertscale videofilter videoparsersbad volume
  osxaudio typefindfunctions audioparsers playback
)

echo "==> Popyachsa AirPlay.app  v$VERSION  ($TARGET)"
[ -f "$DYLIB" ] || { echo "missing dylib: $DYLIB (run build-core-arm64.sh)"; exit 1; }
[ -d "$FRAMEWORK" ] || { echo "missing $FRAMEWORK (install official GStreamer.framework)"; exit 1; }

# ---- 1. release binary -------------------------------------------------------
# Cargo workspace: build output lands in the workspace-root target/ dir.
echo "==> cargo build --release"
( cd "$REPO" && PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build --release -p popyachsa-airplay --target "$TARGET" )
BIN="$REPO/target/$TARGET/release/popyachsa-airplay"

# ---- 2. skeleton -------------------------------------------------------------
echo "==> assembling bundle"
rm -rf "$APP"
C="$APP/Contents"
DEST_LIB="$C/Frameworks/GStreamer/lib"
DEST_PLUGINS="$DEST_LIB/gstreamer-1.0"
DEST_LIBEXEC="$C/Frameworks/GStreamer/libexec/gstreamer-1.0"
mkdir -p "$C/MacOS" "$DEST_PLUGINS" "$DEST_LIBEXEC" "$C/Resources"
cp "$BIN" "$C/MacOS/popyachsa-airplay"
cp "$DYLIB" "$C/MacOS/uxplay-core.dylib"
chmod +w "$C/MacOS/uxplay-core.dylib"

# ---- 3. bundle the GStreamer runtime (plugins + transitive lib closure) ------
# All framework libs/plugins use @rpath install names and carry an
# @loader_path/../lib rpath, so preserving the lib/ + lib/gstreamer-1.0/ layout
# means they find each other with NO per-lib surgery.
echo "==> bundling ${#PLUGINS[@]} plugins + their dependency closure"
queue=()
for p in "${PLUGINS[@]}"; do
  src="$GST_PLUGINS/libgst$p.dylib"
  if [ -f "$src" ]; then cp -p "$src" "$DEST_PLUGINS/"; queue+=( "$src" )
  else echo "   WARN: plugin libgst$p.dylib not found"; fi
done
# seed with uxplay-core's own @rpath deps too
queue+=( "$C/MacOS/uxplay-core.dylib" )

# BFS the @rpath dependency closure into DEST_LIB (flat, like the framework).
while [ ${#queue[@]} -gt 0 ]; do
  src="${queue[0]}"; queue=( "${queue[@]:1}" )
  while IFS= read -r dep; do
    case "$dep" in
      @rpath/*)
        db="${dep#@rpath/}"
        [ -f "$DEST_LIB/$db" ] && continue
        if [ -f "$GST_LIB/$db" ]; then cp -p "$GST_LIB/$db" "$DEST_LIB/$db"; chmod +w "$DEST_LIB/$db"; queue+=( "$GST_LIB/$db" ); fi
        ;;
    esac
  done < <(otool -L "$src" | tail -n +2 | awk '{print $1}')
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

# ---- 7. ad-hoc codesign (needed for Apple Silicon to load modified Mach-Os) --
# NOT Developer ID / notarized — Gatekeeper still requires right-click->Open.
echo "==> ad-hoc codesign"
codesign --force --sign - "$C/MacOS/uxplay-core.dylib" 2>/dev/null || true
codesign --force --deep --sign - "$APP" 2>/dev/null || true

echo "==> done: $APP"
du -sh "$APP" | cut -f1 | sed 's/^/    size: /'
echo "    verify self-containment:  DYLD_PRINT_LIBRARIES=1 '$APP/Contents/MacOS/popyachsa-airplay' 2>&1 | grep -i /Library/Frameworks  # (should be empty)"
