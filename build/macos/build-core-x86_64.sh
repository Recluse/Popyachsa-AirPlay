#!/usr/bin/env bash
# Build uxplay-core.dylib for x86_64 (the Intel half of the universal2 release).
# Same source tree and same flags as build-core-arm64.sh; only the toolchain
# inputs differ. Run build-x86-deps.sh first.
#
# What is different from the arm64 build, and why:
#  * -DCMAKE_OSX_ARCHITECTURES=x86_64  -> cross-compile (this Mac is arm64).
#  * -DCMAKE_PREFIX_PATH -> the contained static x86_64 prefix from
#    build-x86-deps.sh. UxPlay resolves the crypto/plist LIBRARIES with plain
#    find_library (LIBCRYPTO / LIBPLIST), which reads this; Homebrew has no
#    x86_64 build of either on Apple Silicon.
#  * -DCMAKE_IGNORE_PATH=/opt/homebrew/lib -> the real guard. Without it that
#    same find_library returns Homebrew's ARM64 dylib and the link fails late
#    with "building for macOS-x86_64 but attempting to link ... arm64".
#  * The GStreamer.framework is universal, so it is used as-is — that is why
#    PKG_CONFIG_PATH still points at the same framework pkgconfig dir.
#
# Note: openssl HEADERS still come from Homebrew via pkg-config while the static
# libcrypto.a comes from the contained prefix, so after a Homebrew openssl bump
# the two are one patch release apart until build-x86-deps.sh is re-run. Fine
# inside OpenSSL 3.x (ABI-stable across patch releases); see BUILD-MACOS.md.
set -euo pipefail

SRC="${SRC:-$HOME/uxplay-mac-build/UxPlay}"
ARCH=x86_64
BUILD="$SRC/build-$ARCH"
X86_PREFIX="${X86_PREFIX:-$HOME/uxplay-mac-build/x86deps/prefix}"
FRAMEWORK=/Library/Frameworks/GStreamer.framework
GSTPC="$FRAMEWORK/Versions/1.0/lib/pkgconfig"

[ -f "$X86_PREFIX/lib/libcrypto.a" ] || {
  echo "missing x86_64 static deps at $X86_PREFIX (run build-x86-deps.sh)"; exit 1; }

# Identical to build-core-arm64.sh on purpose: pkg-config here supplies only the
# INCLUDE dirs (arch-neutral headers) — the actual x86_64 libraries come from
# CMAKE_PREFIX_PATH below. Adding $X86_PREFIX/lib/pkgconfig would work too, but
# it is not what built the shipped 0.2.12 Intel slice.
export PKG_CONFIG_PATH="$GSTPC:/opt/homebrew/lib/pkgconfig:/opt/homebrew/opt/openssl@3/lib/pkgconfig"
echo "PKG_CONFIG_PATH=$PKG_CONFIG_PATH"

# -U LIBCRYPTO/LIBPLIST: both are find_library CACHE entries (lib/CMakeLists.txt),
# and find_library never re-searches once its cache entry is set — neither a new
# CMAKE_PREFIX_PATH nor CMAKE_IGNORE_PATH invalidates it. Without this, one
# configure that ran before the guards existed (or before X86_PREFIX moved) pins
# Homebrew's arm64 archives into this build tree for good. Re-finding costs nothing.
cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -U LIBCRYPTO -U LIBPLIST \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_CORE_DLL=ON \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -DCMAKE_OSX_ARCHITECTURES="$ARCH" \
  -DCMAKE_PREFIX_PATH="$X86_PREFIX" \
  -DCMAKE_IGNORE_PATH=/opt/homebrew/lib \
  -DGST_MACOS=OFF \
  -DNO_MARCH_NATIVE=ON \
  -DPKG_CONFIG_EXECUTABLE=/opt/homebrew/bin/pkg-config

ninja -C "$BUILD" uxplay-core
echo "=== built ==="
ls -la "$BUILD"/uxplay-core.dylib
# Must be x86_64 alone. Anything else means an arm64 lib leaked past
# CMAKE_IGNORE_PATH and make-app.sh's lipo would produce a broken fat file — so
# fail here rather than print it and hope the operator reads the line.
ARCHS="$(lipo -archs "$BUILD"/uxplay-core.dylib)"
[ "$ARCHS" = "$ARCH" ] || { echo "FATAL: uxplay-core.dylib is '$ARCHS', not $ARCH"; exit 1; }
echo "archs: $ARCHS"
