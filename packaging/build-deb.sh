#!/usr/bin/env bash
# Build the Debian/Ubuntu .deb from prebuilt RELEASE artifacts. Run inside the
# popyachsa-build:u24 container (Debian-based -> dpkg-deb + dpkg-shlibdeps present).
# The binary's glibc floor = the build distro (u24 = 24.04), so this .deb targets
# Ubuntu 24.04+ / Debian 12+. Layout + rationale: see README.md.
#
#   VERSION=0.2.7 BIN=.../popyachsa-airplay SO=.../uxplay-core.so \
#   PKGDIR=.../packaging/shared OUT=. bash build-deb.sh
set -euo pipefail

VERSION="${VERSION:-0.2.7}"
BIN="${BIN:?set BIN=path to release popyachsa-airplay}"
SO="${SO:?set SO=path to release uxplay-core.so}"
PKGDIR="${PKGDIR:?set PKGDIR=path to packaging/shared}"
OUT="${OUT:-.}"
APPID=com.popyachsa.AirPlay

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

# FHS tree: binary + dlopen'd core co-located, /usr/bin symlink (current_exe
# resolves through it -> core_so_path finds the .so next to the binary).
install -Dm755 "$BIN" "$ROOT/usr/lib/popyachsa-airplay/popyachsa-airplay"
install -Dm755 "$SO"  "$ROOT/usr/lib/popyachsa-airplay/uxplay-core.so"
install -dm755 "$ROOT/usr/bin"
ln -s ../lib/popyachsa-airplay/popyachsa-airplay "$ROOT/usr/bin/popyachsa-airplay"
install -Dm644 "$PKGDIR/$APPID.desktop"      "$ROOT/usr/share/applications/$APPID.desktop"
install -Dm644 "$PKGDIR/$APPID.png"          "$ROOT/usr/share/icons/hicolor/256x256/apps/$APPID.png"
install -Dm644 "$PKGDIR/$APPID.metainfo.xml" "$ROOT/usr/share/metainfo/$APPID.metainfo.xml"

# Linked-library deps via dpkg-shlibdeps (libgstreamer/gtk/avahi/xdo/… with the
# correct t64 names for the build distro). Needs a minimal debian/control stub.
mkdir -p "$ROOT/debian"
printf 'Source: popyachsa-airplay\nPackage: popyachsa-airplay\nArchitecture: amd64\n' > "$ROOT/debian/control"
# Fail loudly if dpkg-shlibdeps errors: silently swallowing it would ship a .deb
# with EMPTY linked-lib Depends (no libgstreamer/gtk/avahi/…) that installs on a
# minimal box and then fails at runtime with a missing .so.
if ! ( cd "$ROOT" && dpkg-shlibdeps -O usr/lib/popyachsa-airplay/popyachsa-airplay \
        usr/lib/popyachsa-airplay/uxplay-core.so ) > "$ROOT/.shlibs" 2>"$ROOT/.shlibs.err"; then
  echo "FATAL: dpkg-shlibdeps failed:" >&2; cat "$ROOT/.shlibs.err" >&2; exit 1
fi
SHLIBS="$(sed 's/^shlibs:Depends=//' "$ROOT/.shlibs")"
if [ -z "$SHLIBS" ]; then
  echo "FATAL: dpkg-shlibdeps produced no Depends (under-declared shared libs)" >&2; exit 1
fi
rm -rf "$ROOT/debian" "$ROOT/.shlibs" "$ROOT/.shlibs.err"

# Runtime deps dpkg-shlibdeps can't see: GStreamer plugins are dlopen'd, avahi is a
# service. The codec set lives in plugins-{base,good,bad} + libav.
RUNTIME="gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav, avahi-daemon"
DEPS="$RUNTIME${SHLIBS:+, $SHLIBS}"
INSTALLED="$(du -sk "$ROOT/usr" | cut -f1)"

mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<EOF
Package: popyachsa-airplay
Version: $VERSION
Architecture: amd64
Maintainer: Recluse <me@recluse.lol>
Installed-Size: $INSTALLED
Depends: $DEPS
Recommends: gstreamer1.0-vaapi
Section: video
Priority: optional
Homepage: https://airplay.popyachsa.com
Description: AirPlay receiver — mirror iPhone, iPad or Mac to your screen
 A low-latency AirPlay receiver. Mirror your iPhone, iPad or Mac, or stream
 video, photos and music, to this computer over the local Wi-Fi. Hardware
 H.264/H.265 decode via the system GStreamer (VA-API / NVDEC), tray app,
 16 languages.
EOF

DEB="$OUT/popyachsa-airplay_${VERSION}_amd64.deb"
dpkg-deb --build --root-owner-group "$ROOT" "$DEB"
echo "=== built ==="
ls -la "$DEB"
echo "=== control ==="
dpkg-deb -I "$DEB"
