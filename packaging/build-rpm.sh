#!/usr/bin/env bash
# Build the .rpm from prebuilt RELEASE artifacts. Run in a fedora container (needs
# rpm-build). rpm's auto find-requires maps the binary's sonames to Fedora packages;
# the dlopen'd GStreamer plugins + avahi are added by hand (rpm can't see those).
# NB: the binary here is Ubuntu-built — validate with `dnf install` on clean Fedora.
#
#   VERSION=0.2.7 BIN=.../popyachsa-airplay SO=.../uxplay-core.so \
#   PKGDIR=.../packaging/shared OUT=. bash build-rpm.sh
set -euo pipefail

VERSION="${VERSION:-0.2.7}"
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
/usr/lib/popyachsa-airplay/
/usr/bin/popyachsa-airplay
/usr/share/applications/$APPID.desktop
/usr/share/icons/hicolor/256x256/apps/$APPID.png
/usr/share/metainfo/$APPID.metainfo.xml
EOF

rpmbuild --define "_topdir $TOP" --noclean --buildroot "$BR" -bb "$TOP/SPECS/p.spec"
find "$TOP/RPMS" -name '*.rpm' -exec cp -v {} "$OUT/" \;
