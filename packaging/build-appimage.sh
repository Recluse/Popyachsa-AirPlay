#!/bin/bash
# Build a Linux AppImage for popyachsa-airplay (L8). Bundles the binary, the
# dlopen'd uxplay-core.so (via -l so its deps are pulled too), and GStreamer
# plugins (linuxdeploy-plugin-gstreamer). Built on Ubuntu 24.04 here -> glibc
# 2.39 floor (older glibc needs newer libplist/GLib built from source; TODO).
set -eo pipefail   # pipefail so a failed build in a `... | tail` pipeline still aborts
# Layout (override via env). APP = the Cargo workspace (its target/$PROFILE/ holds
# the built binary + uxplay-core.so); AI = a scratch dir holding tools/ with
# linuxdeploy.AppImage + linuxdeploy-plugin-gstreamer.sh + appimageupdatetool
# (fetch these from the linuxdeploy / AppImageUpdate releases — see BUILD.md).
ROOT="${ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
APP="${APP:-$ROOT}"
AI="${AI:-$ROOT/.appimage-build}"
APPDIR=$AI/AppDir
mkdir -p "$AI/tools"
PROFILE="${PROFILE:-release}"            # release for shipping; debug for quick local
export PATH="$AI/tools:$PATH"
export GSTREAMER_INCLUDE_BAD_PLUGINS=1   # include the 'bad' set (codecs etc.)

echo "=== AppImage profile: $PROFILE ==="
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
cp "$APP/target/$PROFILE/popyachsa-airplay" "$APPDIR/usr/bin/popyachsa-airplay"
cp "$APP/app/icons/popyachsacraft-logo.png" "$AI/popyachsa-airplay.png"

# --- Self-update (delta) ---------------------------------------------------
# Embed zsync update-information so the built AppImage is delta-updatable: this
# makes appimagetool emit a *.AppImage.zsync next to the AppImage, and lets both
# our in-app updater and AppImageLauncher fetch only the changed blocks. The
# zsync URL points at a STABLE filename (no version) that each release overwrites.
export UPDATE_INFORMATION="zsync|https://airplay.popyachsa.com/download/Popyachsa_AirPlay-x86_64.AppImage.zsync"
export LDAI_UPDATE_INFORMATION="$UPDATE_INFORMATION"   # newer linuxdeploy env name

# Bundle appimageupdatetool for the in-app delta path (the app runs it with
# APPIMAGE_EXTRACT_AND_RUN=1, so no libfuse needed at runtime). Best-effort: if
# linuxdeploy/patchelf disturbs it, the app's full-download fallback still works.
# Pinned by sha256: this tool ships INSIDE the AppImage and performs the self-update,
# so a changed/MITM'd "continuous" asset must never slip in. Re-download if the cache
# is missing or stale, then verify — fail loudly on mismatch (re-vet + bump the hash
# deliberately when updating the tool).
AIUT_SHA=8d17a50e2f7502edacab48216d1b491de3669935858591ea0026cc2db375967c
AIUT="$AI/tools/appimageupdatetool-x86_64.AppImage"
if [ ! -f "$AIUT" ] || [ "$(sha256sum "$AIUT" | cut -c1-64)" != "$AIUT_SHA" ]; then
  curl -fsSL -o "$AIUT" \
    https://github.com/AppImage/AppImageUpdate/releases/download/continuous/appimageupdatetool-x86_64.AppImage
fi
GOT_SHA="$(sha256sum "$AIUT" | cut -c1-64)"
if [ "$GOT_SHA" != "$AIUT_SHA" ]; then
  echo "FATAL: appimageupdatetool sha256 mismatch (got $GOT_SHA, want $AIUT_SHA)." >&2
  echo "       upstream 'continuous' changed — re-vet it and update AIUT_SHA." >&2
  exit 1
fi
cp "$AIUT" "$APPDIR/usr/bin/appimageupdatetool"
chmod +x "$APPDIR/usr/bin/appimageupdatetool"
# ---------------------------------------------------------------------------

cat > "$AI/popyachsa-airplay.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Popyachsa AirPlay
Exec=popyachsa-airplay
Icon=popyachsa-airplay
Categories=AudioVideo;Player;
Terminal=false
EOF

cd "$AI"
# 1. Populate AppDir (binary + uxplay-core.so deps + GStreamer plugins). No
#    `--output appimage`: we prune host display libs before packaging.
"$AI/tools/linuxdeploy.AppImage" --appdir "$APPDIR" \
  -e "$APPDIR/usr/bin/popyachsa-airplay" \
  -l "$APP/target/$PROFILE/uxplay-core.so" \
  -d "$AI/popyachsa-airplay.desktop" \
  -i "$AI/popyachsa-airplay.png" \
  --plugin gstreamer 2>&1 | tail -45

# 2. PRUNE the host display stack (X11 / xcb / xkbcommon / wayland client libs).
#    These MUST come from the user's system so they match the host's libX11.so.6 /
#    libxcb.so.1 / X server. linuxdeploy excludes the cores (libX11.so, libxcb.so)
#    but leaves their companions (libXext, libXrender, libxcb-render/shm/xkb,
#    libxkbcommon, libwayland-*) built against the BUILD host's libX11/libxcb — on
#    a user with a different version those are ABI-skewed -> SIGSEGV in X init
#    (e.g. the egui Settings subprocess crashes on XOpenDisplay). They exist on
#    every X/Wayland desktop, so dropping them is safe (standard AppImage excludelist).
echo "=== pruning host display libs from the bundle ==="
( cd "$APPDIR/usr/lib" 2>/dev/null && rm -fv \
    libX11.so* libXau.so* libXcomposite.so* libXcursor.so* libXdamage.so* \
    libXdmcp.so* libXext.so* libXfixes.so* libXi.so* libXinerama.so* \
    libXrandr.so* libXrender.so* libXss.so* libXtst.so* libXv.so* \
    libxcb.so* libxcb-*.so* libxkbcommon.so* libxkbcommon-x11.so* libwayland-*.so* \
    2>/dev/null ) | sed 's/^/  pruned /' || true

# 3. Pack the pruned AppDir + zsync (delta-update info from $UPDATE_INFORMATION).
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$AI/tools/appimagetool.AppImage" \
  -u "$UPDATE_INFORMATION" \
  "$APPDIR" "$AI/Popyachsa_AirPlay-x86_64.AppImage" 2>&1 | tail -20

echo "=== result ==="
echo "--- residual display libs in AppDir (expect none) ---"
ls "$APPDIR/usr/lib/" | grep -E "^libX|^libxcb|^libxkbcommon|^libwayland" || echo "  (clean — no host display libs bundled)"
# Both the AppImage and its zsync control file (publish them together so the
# delta update can find the .zsync alongside the AppImage).
ls -la "$AI"/*.AppImage "$AI"/*.zsync 2>&1
