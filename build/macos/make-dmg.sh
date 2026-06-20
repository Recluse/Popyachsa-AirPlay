#!/usr/bin/env bash
# make-dmg.sh — wrap dist/Popyachsa AirPlay.app into a drag-to-install DMG.
#
# Produces dist/Popyachsa-AirPlay-<version>.dmg with the .app + an /Applications
# symlink and a tidy icon layout. The .app is ad-hoc signed (not Developer ID), so
# a DOWNLOADED copy is quarantined -> first launch needs right-click -> Open.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APPDIR="$REPO/app"
VERSION="${VERSION:-$(grep -m1 '^version' "$APPDIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')}"
OUT="$REPO/build/macos/dist"
APP="$OUT/Popyachsa AirPlay.app"
VOL="Popyachsa AirPlay"
DMG="$OUT/Popyachsa-AirPlay-$VERSION.dmg"

[ -d "$APP" ] || { echo "no .app at $APP — run make-app.sh first"; exit 1; }

echo "==> staging DMG contents"
STAGE="$(mktemp -d)/Popyachsa AirPlay"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

# read-write DMG we can lay out, then compress to read-only.
RW="$(mktemp -d)/rw.dmg"
SIZE_MB=$(( $(du -sm "$STAGE" | cut -f1) + 60 ))
hdiutil create -volname "$VOL" -srcfolder "$STAGE" -fs HFS+ -format UDRW -size "${SIZE_MB}m" -ov "$RW" >/dev/null

echo "==> laying out the window"
DEV="$(hdiutil attach -readwrite -noverify -noautoopen "$RW" | grep -E '^/dev/' | head -1 | awk '{print $1}')"
MNT="/Volumes/$VOL"
# Best-effort Finder layout (icon view, sizes, positions). Never fail the build on it.
osascript <<OSA 2>/dev/null || echo "   (layout skipped — DMG still valid)"
tell application "Finder"
  tell disk "$VOL"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set the bounds of container window to {200, 120, 760, 480}
    set theViewOptions to the icon view options of container window
    set arrangement of theViewOptions to not arranged
    set icon size of theViewOptions to 128
    set position of item "Popyachsa AirPlay.app" of container window to {150, 190}
    set position of item "Applications" of container window to {410, 190}
    update without registering applications
    delay 1
    close
  end tell
end tell
OSA
sync
hdiutil detach "$DEV" >/dev/null 2>&1 || hdiutil detach "$DEV" -force >/dev/null 2>&1 || true

echo "==> compressing"
rm -f "$DMG"
hdiutil convert "$RW" -format UDZO -imagekey zlib-level=9 -o "$DMG" >/dev/null

echo "==> done: $DMG"
ls -lh "$DMG" | awk '{print "    size:", $5}'
shasum -a 256 "$DMG" | awk '{print "    sha256:", $1}'
