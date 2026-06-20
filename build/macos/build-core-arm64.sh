#!/usr/bin/env bash
# M1 — build uxplay-core.dylib (arm64) on macOS.
# Source tree: UxPlay fc126fd (v1.73.6+1) + clean-full-vs-v1.73.6.diff
#              + lib/airplay_core.{h,cpp} + L1 gmainloop fix (applied in-tree).
#
# Notes:
#  * -DBUILD_CORE_DLL=ON                -> add_library(uxplay-core SHARED ...)
#  * -DCMAKE_POSITION_INDEPENDENT_CODE  -> PIC so the renderers/airplay/playfair
#                                          /llhttp static libs link into a .dylib
#                                          (same flag the Linux L0 build needed).
#  * -DGST_MACOS=OFF                    -> do NOT compile the gst_macos_main()
#                                          main() wrapper (M3: tao owns main loop;
#                                          the dylib's main() is dead code anyway).
#  * static openssl/libplist come from Homebrew (arm64-only) -> arm64 slice only.
set -euo pipefail

# UxPlay engine source = the third_party/uxplay submodule (see BUILD.md). Override $SRC if elsewhere.
SRC="${SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/third_party/uxplay}"
ARCH="${ARCH:-arm64}"
BUILD="$SRC/build-$ARCH"
FRAMEWORK=/Library/Frameworks/GStreamer.framework
GSTPC="$FRAMEWORK/Versions/1.0/lib/pkgconfig"

export PKG_CONFIG_PATH="$GSTPC:/opt/homebrew/lib/pkgconfig:/opt/homebrew/opt/openssl@3/lib/pkgconfig"
echo "PKG_CONFIG_PATH=$PKG_CONFIG_PATH"

cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_CORE_DLL=ON \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -DCMAKE_OSX_ARCHITECTURES="$ARCH" \
  -DGST_MACOS=OFF \
  -DNO_MARCH_NATIVE=ON \
  -DPKG_CONFIG_EXECUTABLE=/opt/homebrew/bin/pkg-config

ninja -C "$BUILD" uxplay-core
echo "=== built ==="
ls -la "$BUILD"/uxplay-core.dylib
lipo -archs "$BUILD"/uxplay-core.dylib
