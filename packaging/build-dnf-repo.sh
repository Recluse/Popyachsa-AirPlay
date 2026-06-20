#!/usr/bin/env bash
# Build a signed DNF/YUM repo from .rpm(s). Needs createrepo_c + gnupg (run in a
# fedora container). Stateless: the repo carries whatever .rpm is in RPMS_DIR.
# We sign the repo METADATA (repomd.xml.asc -> repo_gpgcheck=1), mirroring how the
# APT repo signs its Release; the metadata's per-package checksums then anchor each
# .rpm's integrity (so the client .repo uses gpgcheck=0 repo_gpgcheck=1).
#
#   RPMS_DIR=dir-with-rpms  GPGKEY=repo-priv.asc  OUT=repo/rpm  bash build-dnf-repo.sh
set -euo pipefail
RPMS_DIR="${RPMS_DIR:?set RPMS_DIR}"
GPGKEY="${GPGKEY:?set GPGKEY}"
OUT="${OUT:?set OUT}"

export GNUPGHOME="$(mktemp -d)"; chmod 700 "$GNUPGHOME"
trap 'rm -rf "$GNUPGHOME"' EXIT
gpg --batch --quiet --import "$GPGKEY"
KEYID="$(gpg --list-secret-keys --with-colons | awk -F: '/^sec/{print $5; exit}')"

rm -rf "$OUT"; mkdir -p "$OUT"
cp "$RPMS_DIR"/*.rpm "$OUT/"
createrepo_c --quiet "$OUT"
gpg --batch --yes --default-key "$KEYID" --detach-sign --armor "$OUT/repodata/repomd.xml"
echo "=== DNF repo built + metadata signed (key $KEYID) ==="
find "$OUT" -type f | sort
