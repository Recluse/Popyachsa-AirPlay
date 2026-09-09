#!/usr/bin/env bash
# Build the x86_64 STATIC dependencies uxplay-core.dylib needs for the Intel half
# of the universal2 release: openssl (libcrypto/libssl) + libplist.
#
# Why from source: Homebrew on Apple Silicon ships arm64-only bottles, so there
# is no x86_64 openssl/libplist to link against. Everything else the dylib needs
# (GStreamer, the system frameworks) is already universal, so these two are the
# whole gap. They are built STATIC on purpose — a static slice cannot be missed
# at runtime on the user's Intel Mac, and nothing has to be bundled or rpath'd.
#
# Output prefix is what build-core-x86_64.sh passes as CMAKE_PREFIX_PATH /
# OPENSSL_ROOT_DIR. Run once; re-run only when a version below is bumped or
# Homebrew's openssl@3 moves (see the openssl-cache-drift note in BUILD-MACOS.md).
#
#   bash build/macos/build-x86-deps.sh
set -euo pipefail

DEPS="${DEPS:-$HOME/uxplay-mac-build/x86deps}"
PREFIX="${PREFIX:-$DEPS/prefix}"
SRC="${SRC:-$DEPS/src}"
JOBS="${JOBS:-$(sysctl -n hw.ncpu)}"

# Pinned + hash-checked: these tarballs are fetched over the network and end up
# inside a shipped binary, so a bad download must fail here, not in the field.
OPENSSL_VER="${OPENSSL_VER:-3.6.3}"
OPENSSL_SHA=243a86649cf6f23eeb6a2ff2456e09e5d77dd9018a54d3d96b0c6bdd6ba6c7f1
LIBPLIST_VER="${LIBPLIST_VER:-2.7.0}"
LIBPLIST_SHA=7ac42301e896b1ebe3c654634780c82baa7cb70df8554e683ff89f7c2643eb8b

mkdir -p "$SRC" "$PREFIX"

# fetch <url> <file> <sha256>
fetch() {
  if [ ! -f "$SRC/$2" ]; then
    echo "==> downloading $2"
    curl -fsSL "$1" -o "$SRC/$2.part"
    mv "$SRC/$2.part" "$SRC/$2"
  fi
  echo "$3  $SRC/$2" | shasum -a 256 -c - >/dev/null
}

# installed <pkgconfig file> <pinned version> — is THAT version already in the
# prefix? The old guard was "does libcrypto.a exist?", which cannot notice a
# version bump above: bump + re-run and the release keeps the previous library.
# The .pc is written by `make install`, so it names what is actually there.
installed() {
  [ -f "$PREFIX/lib/pkgconfig/$1" ] && grep -qx "Version: $2" "$PREFIX/lib/pkgconfig/$1"
}

# ---- openssl (static, x86_64) ------------------------------------------------
# darwin64-x86_64-cc is openssl's own cross target; no-apps/no-docs/no-tests just
# cut build time (we only ever link libcrypto/libssl).
fetch "https://github.com/openssl/openssl/releases/download/openssl-$OPENSSL_VER/openssl-$OPENSSL_VER.tar.gz" \
      "openssl-$OPENSSL_VER.tar.gz" "$OPENSSL_SHA"
if ! installed libcrypto.pc "$OPENSSL_VER"; then
  echo "==> building openssl $OPENSSL_VER (x86_64, static)"
  tar -xzf "$SRC/openssl-$OPENSSL_VER.tar.gz" -C "$SRC"
  ( cd "$SRC/openssl-$OPENSSL_VER"
    ./Configure darwin64-x86_64-cc no-shared no-tests no-docs no-apps --prefix="$PREFIX"
    make -j"$JOBS"
    make install_sw )
fi

# ---- libplist (static, x86_64) -----------------------------------------------
# autotools has no arch switch, so the -arch flag rides on CC/CXX. --without-cython
# keeps the Python bindings (and their host-arch interpreter) out of the picture.
fetch "https://github.com/libimobiledevice/libplist/releases/download/$LIBPLIST_VER/libplist-$LIBPLIST_VER.tar.bz2" \
      "libplist-$LIBPLIST_VER.tar.bz2" "$LIBPLIST_SHA"
if ! installed libplist-2.0.pc "$LIBPLIST_VER"; then
  echo "==> building libplist $LIBPLIST_VER (x86_64, static)"
  tar -xjf "$SRC/libplist-$LIBPLIST_VER.tar.bz2" -C "$SRC"
  ( cd "$SRC/libplist-$LIBPLIST_VER"
    ./configure --host=x86_64-apple-darwin \
      CC="clang -arch x86_64" CXX="clang++ -arch x86_64" \
      --disable-shared --enable-static --without-cython --prefix="$PREFIX"
    make -j"$JOBS"
    make install )
fi

echo "=== built ==="
ls -la "$PREFIX/lib/libcrypto.a" "$PREFIX/lib/libssl.a" "$PREFIX/lib/libplist-2.0.a"
# Prove the slice: a stray arm64 .a here links but produces an arm64-only dylib.
# CHECKED, not printed — a lipo failure inside a printf argument is invisible
# (the exit status is discarded), and three lines a human might read are not a gate.
for a in libcrypto.a libssl.a libplist-2.0.a; do
  archs="$(lipo -archs "$PREFIX/lib/$a")"
  [ "$archs" = "x86_64" ] || {
    echo "FATAL: $a is '$archs', not x86_64 — wipe $PREFIX and re-run"; exit 1; }
  printf '%-18s %s\n' "$a" "$archs"
done
