#!/usr/bin/env bash
# Build a signed flat APT repo from .deb(s). Stateless: regenerates from whatever
# .deb files are in DEBS_DIR (a single-app repo always carries just the latest).
# Needs: dpkg-dev (dpkg-scanpackages), apt-utils (apt-ftparchive), gnupg.
#
#   DEBS_DIR=dir-with-debs  GPGKEY=repo-priv.asc  OUT=repo/apt  bash build-apt-repo.sh
set -euo pipefail
DEBS_DIR="${DEBS_DIR:?set DEBS_DIR}"
GPGKEY="${GPGKEY:?set GPGKEY (armored private key)}"
OUT="${OUT:?set OUT (apt repo root)}"
SUITE="${SUITE:-stable}"

export GNUPGHOME="$(mktemp -d)"; chmod 700 "$GNUPGHOME"
trap 'rm -rf "$GNUPGHOME"' EXIT
gpg --batch --quiet --import "$GPGKEY"
KEYID="$(gpg --list-secret-keys --with-colons | awk -F: '/^sec/{print $5; exit}')"

rm -rf "$OUT"
mkdir -p "$OUT/pool" "$OUT/dists/$SUITE/main/binary-amd64"
cp "$DEBS_DIR"/*.deb "$OUT/pool/"
( cd "$OUT" && dpkg-scanpackages --multiversion pool > "dists/$SUITE/main/binary-amd64/Packages" )
gzip -9kf "$OUT/dists/$SUITE/main/binary-amd64/Packages"

cat > "$GNUPGHOME/release.conf" <<EOF
APT::FTPArchive::Release::Origin "Popyachsa AirPlay";
APT::FTPArchive::Release::Label "Popyachsa AirPlay";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "amd64";
APT::FTPArchive::Release::Components "main";
EOF
( cd "$OUT/dists/$SUITE" && apt-ftparchive -c "$GNUPGHOME/release.conf" release . > Release )
# InRelease (inline-signed) + Release.gpg (detached) — apt accepts either.
( cd "$OUT/dists/$SUITE" && gpg --batch --yes --default-key "$KEYID" --clearsign -o InRelease Release )
( cd "$OUT/dists/$SUITE" && gpg --batch --yes --default-key "$KEYID" -abs -o Release.gpg Release )
echo "=== APT repo built (key $KEYID) ==="
find "$OUT" -type f | sort
