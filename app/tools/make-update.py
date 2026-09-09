#!/usr/bin/env python3
"""Build + sign the auto-update manifest (updates.json) for Popyachsa AirPlay.

The update channel is integrity-protected by an Ed25519 signature so a
compromised web host can't push a malicious build: the app embeds the public
key and refuses any manifest whose signature doesn't verify.

Signing key
-----------
A 32-byte Ed25519 seed lives OUTSIDE the repo at:
    ~/.popyachsa-airplay/update-signing.key   (hex, 0600)
Generated on first run. NEVER commit it. The matching public key (hex) is
printed by `keygen` / `pubkey` and must be pasted into src/update.rs
(EMBEDDED_PUBKEY_HEX).

Canonical signed messages (must match src/update.rs::verify_signature exactly):
    v2:     popyachsa-airplay\n<platform>\n<version>\n<sha256-hex>\n<url>\n<pub_date>
    legacy: popyachsa-airplay\n<version>\n<sha256-hex>\n<url>

Every manifest carries BOTH signatures — `signature_v2` and `signature`.

Why: one key signs all three feeds, and the legacy message has no platform in
it, so a manifest published for any platform verifies for every client. Copy
updates.json over updates-linux.json and every Linux client renames a 99 MB
Windows zip over its AppImage. v2 binds the manifest to one feed.

Rollout (do not skip a step — the feeds are read by clients you cannot upgrade):
  1. This script starts emitting both signatures NOW. Old clients keep reading
     `signature`; there is nothing for them to notice.
  2. Ship the client that prefers `signature_v2` (>= 0.2.13). Until it is out,
     v2 is inert.
  3. Only once the field has aged out of pre-0.2.13 do you drop the legacy
     signature here and the legacy arm in update.rs — in that order, feed last.
     Publishing a v2-only manifest earlier freezes every old client forever.

Usage
-----
  python make-update.py keygen                 # create key, print pubkey
  python make-update.py pubkey                  # print pubkey hex
  # Windows (zip):
  python make-update.py sign --platform windows \
        --file dist/PopyachsaAirPlay-0.2.1.zip \
        --version 0.2.1 \
        --base-url https://airplay.popyachsa.com/download \
        --notes "Auto-updates, installer, 16 languages." \
        --out dist/updates.json

  # Linux (AppImage) — same signing key, separate feed file:
  python make-update.py sign --platform linux \
        --file dist/Popyachsa_AirPlay-x86_64.AppImage \
        --version 0.2.1 \
        --base-url https://airplay.popyachsa.com/download \
        --notes "Linux release." \
        --out dist/updates-linux.json

  # macOS (zipped .app) — see MACOS-UPDATE.md:
  python make-update.py sign --platform macos \
        --file dist/PopyachsaAirPlay-macos-0.2.1.zip \
        --version 0.2.1 --out dist/updates-macos.json
"""
import argparse, hashlib, json, os, sys
from pathlib import Path

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import (
        Ed25519PrivateKey, Ed25519PublicKey)
    from cryptography.hazmat.primitives import serialization
except ImportError:
    # Reported when a key is actually needed, so `selftest` (which only checks the
    # message format) still runs anywhere.
    Ed25519PrivateKey = None

KEY_PATH = Path.home() / ".popyachsa-airplay" / "update-signing.key"
APP_TAG = "popyachsa-airplay"


def load_or_create_key(create=False):
    if Ed25519PrivateKey is None:
        sys.exit("need `cryptography`: pip install cryptography")
    if KEY_PATH.exists():
        seed = bytes.fromhex(KEY_PATH.read_text().strip())
        return Ed25519PrivateKey.from_private_bytes(seed)
    if not create:
        sys.exit(f"no signing key at {KEY_PATH} — run `keygen` first")
    KEY_PATH.parent.mkdir(parents=True, exist_ok=True)
    key = Ed25519PrivateKey.generate()
    seed = key.private_bytes(serialization.Encoding.Raw,
                             serialization.PrivateFormat.Raw,
                             serialization.NoEncryption())
    KEY_PATH.write_text(seed.hex())
    try:
        os.chmod(KEY_PATH, 0o600)
    except OSError:
        pass
    return key


def pubkey_hex(key):
    raw = key.public_key().public_bytes(serialization.Encoding.Raw,
                                         serialization.PublicFormat.Raw)
    return raw.hex()


# Platform ids — must match update.rs::PLATFORM for the matching cfg.
PLATFORMS = ("windows", "linux", "macos")

# Artifact extension each feed is allowed to point at. The client enforces the
# same rule (update.rs::ARTIFACT_EXT), so a mismatch here publishes a manifest
# that every client of that platform will refuse — catch it at signing time.
PLATFORM_EXT = {"windows": ".zip", "linux": ".appimage", "macos": ".zip"}


def canonical_msg_v2(platform, version, sha256_hex, url, pub_date):
    return f"{APP_TAG}\n{platform}\n{version}\n{sha256_hex}\n{url}\n{pub_date}".encode()


def canonical_msg(version, sha256_hex, url):
    """Legacy, platform-less message. Kept only for clients < 0.2.13 — see the
    rollout note at the top of this file before removing it."""
    return f"{APP_TAG}\n{version}\n{sha256_hex}\n{url}".encode()


def cmd_keygen(_):
    key = load_or_create_key(create=True)
    print("signing key:", KEY_PATH)
    print("EMBEDDED_PUBKEY_HEX =", pubkey_hex(key))


def cmd_pubkey(_):
    print(pubkey_hex(load_or_create_key()))


def cmd_sign(args):
    key = load_or_create_key()
    artifact_path = Path(args.artifact)
    ext = PLATFORM_EXT[args.platform]
    if not artifact_path.name.lower().endswith(ext):
        sys.exit(f"{artifact_path.name} is not a {ext} — wrong --platform {args.platform}?")
    data = artifact_path.read_bytes()
    sha = hashlib.sha256(data).hexdigest()
    url = f"{args.base_url.rstrip('/')}/{artifact_path.name}"
    pub_date = args.date or ""
    manifest = {
        "version": args.version,
        "url": url,
        "sha256": sha,
        "size": len(data),
        "notes": args.notes or "",
        "pub_date": pub_date,
        # Both forms, deliberately: v2 for >= 0.2.13, legacy for everyone already
        # installed. Read the rollout note before dropping either.
        "signature": key.sign(canonical_msg(args.version, sha, url)).hex(),
        "signature_v2": key.sign(
            canonical_msg_v2(args.platform, args.version, sha, url, pub_date)).hex(),
    }
    # Optional mirror (not signed — the sha256 above still gates integrity).
    if args.mirror_url:
        manifest["mirror_url"] = f"{args.mirror_url.rstrip('/')}/{artifact_path.name}"
    out = Path(args.out)
    out.write_text(json.dumps(manifest, indent=2, ensure_ascii=False))
    print(f"wrote {out}  ({len(data)} bytes, sha256={sha[:16]}…)")
    print(f"url = {url}")


def cmd_selftest(_):
    """Pin the signed bytes. The identical literal is asserted by
    update.rs::tests::v2_message_matches_make_update_py — if the two ever drift,
    every client silently stops updating, so both sides fail loudly instead."""
    assert canonical_msg_v2("linux", "1.2.3", "abc", "https://h/a.zip", "2026-01-02") == \
        b"popyachsa-airplay\nlinux\n1.2.3\nabc\nhttps://h/a.zip\n2026-01-02"
    assert canonical_msg("1.2.3", "abc", "https://h/a.zip") == \
        b"popyachsa-airplay\n1.2.3\nabc\nhttps://h/a.zip"
    print("ok")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("keygen").set_defaults(func=cmd_keygen)
    sub.add_parser("pubkey").set_defaults(func=cmd_pubkey)
    sub.add_parser("selftest").set_defaults(func=cmd_selftest)
    s = sub.add_parser("sign")
    s.add_argument("--zip", "--file", dest="artifact", required=True,
                   help="release artifact to sign (Windows .zip or Linux .AppImage)")
    s.add_argument("--platform", required=True, choices=PLATFORMS,
                   help="feed this manifest is for; goes INTO the v2 signature")
    s.add_argument("--version", required=True)
    s.add_argument("--base-url", default="https://airplay.popyachsa.com/download")
    s.add_argument("--mirror-url", default="",
                   help="optional fallback base URL (e.g. the Google mirror)")
    s.add_argument("--notes", default="")
    s.add_argument("--date", default="")
    s.add_argument("--out", default="dist/updates.json")
    s.set_defaults(func=cmd_sign)
    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
