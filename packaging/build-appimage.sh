#!/bin/bash
# Build a Linux AppImage for popyachsa-airplay (L8). Bundles the binary, the
# dlopen'd uxplay-core.so (via -l so its deps are pulled too), and GStreamer
# plugins (linuxdeploy-plugin-gstreamer). Built on Ubuntu 24.04 here -> glibc
# 2.39 floor (older glibc needs newer libplist/GLib built from source; TODO).
set -eo pipefail   # pipefail so a failed build in a `... | tail` pipeline still aborts
ROOT=/home/recluse/l3
APP=$ROOT/popyachsa-airplay
AI=$ROOT/appimage
APPDIR=$AI/AppDir
PROFILE="${PROFILE:-release}"            # release for shipping; debug for quick local
export PATH="$AI/tools:$PATH"
export GSTREAMER_INCLUDE_BAD_PLUGINS=1   # include the 'bad' set (codecs etc.)

echo "=== AppImage profile: $PROFILE ==="
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
cp "$APP/target/$PROFILE/popyachsa-airplay" "$APPDIR/usr/bin/popyachsa-airplay"
cp "$APP/icons/popyachsacraft-logo.png" "$AI/popyachsa-airplay.png"

# GPL-3 text, same as the .deb/.rpm/.app carry (audit #23): the AppImage bundles a
# GPL binary, so the licence has to travel with it. Hard failure, not best-effort —
# an image built without it is a licence violation, and the prune step below only
# touches usr/lib, so this survives to the packaged image.
COPYING="${COPYING:-$ROOT/packaging/shared/COPYING}"
[ -f "$COPYING" ] || { echo "missing COPYING at $COPYING (set COPYING=<path>)" >&2; exit 1; }
install -Dm644 "$COPYING" "$APPDIR/usr/share/doc/popyachsa-airplay/copyright"

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
# 1. Populate AppDir: deploy the binary + uxplay-core.so deps + GStreamer plugins.
#    NOTE: no `--output appimage` here — we PRUNE the host display libs (below)
#    before packaging, so the pack must be a separate appimagetool step.
"$AI/tools/linuxdeploy.AppImage" --appdir "$APPDIR" \
  -e "$APPDIR/usr/bin/popyachsa-airplay" \
  -l "$APP/target/$PROFILE/uxplay-core.so" \
  -d "$AI/popyachsa-airplay.desktop" \
  -i "$AI/popyachsa-airplay.png" \
  --plugin gstreamer 2>&1 | tail -45

# 2. PRUNE the host display stack. An AppImage must NOT ship the X11 / xcb /
#    xkbcommon / wayland client libs — they have to come from the user's system so
#    they match the host's libX11.so.6 / libxcb.so.1 / X server. linuxdeploy
#    excludes the cores (libX11.so, libxcb.so) but leaves their COMPANIONS
#    (libXext, libXrender, libxcb-render/shm/xkb, libxkbcommon, libwayland-*),
#    which are built against the BUILD host's libX11/libxcb. On a user with a
#    different libX11/libxcb version those are ABI-skewed -> memory corruption in
#    X init -> SIGSEGV in XOpenDisplay (e.g. the egui Settings subprocess crashes
#    while .deb installs work). These libs are present on every X/Wayland desktop,
#    so dropping them is safe and is the standard AppImage excludelist behaviour.
echo "=== pruning host display libs from the bundle ==="
# Also prune the host GPU video-accel CLIENT libs (libva*, libvdpau): like libX11
# and libpipewire, these must come from the user's system so they match the host's
# VA/VDPAU DRIVER (iHD/radeonsi/nvidia, GPU+kernel-specific, never bundleable). A
# bundled (build-host) libva that is OLDER than the user's driver fails the
# libva<->driver ABI check, so the `va` GStreamer plugin registers NO decoders and
# `-vd vah264dec` dies with "no element". Host libva matches the host driver -> the
# va plugin's vah264dec/vah265dec register and HW decode works. (We keep the bundled
# GStreamer va helper libgstva-1.0.so — that's our plugin's lib, not the host stack.)
( cd "$APPDIR/usr/lib" 2>/dev/null && rm -fv \
    libX11.so* libXau.so* libXcomposite.so* libXcursor.so* libXdamage.so* \
    libXdmcp.so* libXext.so* libXfixes.so* libXi.so* libXinerama.so* \
    libXrandr.so* libXrender.so* libXss.so* libXtst.so* libXv.so* \
    libxcb.so* libxcb-*.so* libxkbcommon.so* libxkbcommon-x11.so* libwayland-*.so* \
    libva.so* libva-drm.so* libva-x11.so* libva-glx.so* libvdpau.so* \
    2>/dev/null ) | sed 's/^/  pruned /' || true

# 3. Pack the pruned AppDir into the AppImage + zsync (embeds the delta-update
#    info from $UPDATE_INFORMATION so appimagetool emits the *.zsync too).
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$AI/tools/appimagetool.AppImage" \
  -u "$UPDATE_INFORMATION" \
  "$APPDIR" "$AI/Popyachsa_AirPlay-x86_64.AppImage" 2>&1 | tail -20

echo "=== result ==="
# Confirm the display stack is GONE from the packaged image (must list nothing).
echo "--- residual display libs in AppDir (expect none) ---"
ls "$APPDIR/usr/lib/" | grep -E "^libX|^libxcb|^libxkbcommon|^libwayland" || echo "  (clean — no host display libs bundled)"
# Both the AppImage and its zsync control file (publish them together so the
# delta update can find the .zsync alongside the AppImage).
ls -la "$AI"/*.AppImage "$AI"/*.zsync 2>&1
