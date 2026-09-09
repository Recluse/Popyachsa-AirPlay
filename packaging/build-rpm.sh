#!/usr/bin/env bash
# Build the .rpm from prebuilt RELEASE artifacts. Run in a fedora container (needs
# rpm-build). rpm's auto find-requires maps the binary's sonames to Fedora packages;
# the dlopen'd GStreamer plugins + avahi are added by hand (rpm can't see those).
# NB: the binary here is Ubuntu-built — validate with `dnf install` on clean Fedora.
#
#   VERSION=0.2.7 BIN=.../popyachsa-airplay SO=.../uxplay-core.so \
#   PKGDIR=.../packaging/shared OUT=. bash build-rpm.sh
set -euo pipefail

# Mandatory, like BIN/SO/PKGDIR: the old `${VERSION:-0.2.7}` default meant a
# forgotten VERSION= silently stamped 0.2.7 into the spec AND the .rpm filename.
VERSION="${VERSION:?set VERSION=X.Y.Z (must match Cargo.toml)}"
BIN="${BIN:?set BIN}"; SO="${SO:?set SO}"; PKGDIR="${PKGDIR:?set PKGDIR}"; OUT="${OUT:-.}"
APPID=com.popyachsa.AirPlay

TOP="$(mktemp -d)"
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP/SPECS" "$TOP/RPMS"
BR="$TOP/buildroot"

install -Dm755 "$BIN" "$BR/usr/lib/popyachsa-airplay/popyachsa-airplay"
install -Dm755 "$SO"  "$BR/usr/lib/popyachsa-airplay/uxplay-core.so"
install -dm755 "$BR/usr/bin"
ln -s ../lib/popyachsa-airplay/popyachsa-airplay "$BR/usr/bin/popyachsa-airplay"
install -Dm644 "$PKGDIR/$APPID.desktop"      "$BR/usr/share/applications/$APPID.desktop"
install -Dm644 "$PKGDIR/$APPID.png"          "$BR/usr/share/icons/hicolor/256x256/apps/$APPID.png"
install -Dm644 "$PKGDIR/$APPID.metainfo.xml" "$BR/usr/share/metainfo/$APPID.metainfo.xml"
# GPL-3 §4: ship the licence text with the binary. %license below marks it so
# `rpm -qL` finds it and it survives a --nodocs install.
install -Dm644 "$PKGDIR/COPYING" "$BR/usr/share/licenses/popyachsa-airplay/COPYING"

cat > "$TOP/SPECS/p.spec" <<EOF
Name:    popyachsa-airplay
Version: $VERSION
# No %{?dist}: the binary is a portable cross-distro build, not Fedora-native.
Release: 1
Summary: AirPlay receiver — mirror iPhone, iPad or Mac to your screen
License: GPLv3+
URL:     https://airplay.popyachsa.com
# dlopen'd plugins + the daemon (rpm's auto find-requires can't see these):
Requires: gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-libav avahi
Recommends: gstreamer1-vaapi
AutoReq: yes
%description
A low-latency AirPlay receiver. Mirror your iPhone, iPad or Mac, or stream video,
photos and music, to this computer over the local Wi-Fi. Hardware H.264/H.265
decode via the system GStreamer, tray app, 16 languages.
%files
%license /usr/share/licenses/popyachsa-airplay/COPYING
/usr/lib/popyachsa-airplay/
/usr/bin/popyachsa-airplay
/usr/share/applications/$APPID.desktop
/usr/share/icons/hicolor/256x256/apps/$APPID.png
/usr/share/metainfo/$APPID.metainfo.xml
EOF

rpmbuild --define "_topdir $TOP" --noclean --buildroot "$BR" -bb "$TOP/SPECS/p.spec"
# xargs, not `-exec … \;`: with `\;` a failing cp does NOT make find exit
# non-zero, so an unwritable or full $OUT ended the script "successfully" with no
# rpm in it — and the EXIT trap then wipes $TOP. (`-exec … +` is not an option
# here: the + form requires {} to be the LAST argument, so it cannot take a
# destination.) xargs exits 123 on a failed child, and pipefail carries it out.
find "$TOP/RPMS" -name '*.rpm' -print0 | xargs -0 -I{} cp -v {} "$OUT/"
echo "=== delivered ==="
# Also proves an rpm of THIS version actually landed: the glob fails loudly if not.
ls -la "$OUT"/popyachsa-airplay-"$VERSION"-*.rpm
