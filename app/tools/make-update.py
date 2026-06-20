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

Canonical signed message (must match src/update.rs::verify exactly):
    popyachsa-airplay\n<version>\n<sha256-hex>\n<url>

Usage
-----
  python make-update.py keygen                 # create key, print pubkey
  python make-update.py pubkey                  # print pubkey hex
  # Windows (zip):
  python make-update.py sign \
        --file dist/PopyachsaAirPlay-0.2.1.zip \
        --version 0.2.1 \
        --base-url https://airplay.popyachsa.com/download \
        --notes "Auto-updates, installer, 16 languages." \
        --out dist/updates.json

  # Linux (AppImage) — same signing key + message, separate feed file:
  python make-update.py sign \
        --file dist/Popyachsa_AirPlay-x86_64.AppImage \
        --version 0.2.1 \
        --base-url https://airplay.popyachsa.com/download \
        --notes "Linux release." \
        --out dist/updates-linux.json
"""
import argparse, hashlib, json, os, sys
from pathlib import Path

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import (
        Ed25519PrivateKey, Ed25519PublicKey)
    from cryptography.hazmat.primitives import serialization
except ImportError:
    sys.exit("need `cryptography`: pip install cryptography")

KEY_PATH = Path.home() / ".popyachsa-airplay" / "update-signing.key"
APP_TAG = "popyachsa-airplay"


def load_or_create_key(create=False):
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


def canonical_msg(version, sha256_hex, url):
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
    data = artifact_path.read_bytes()
    sha = hashlib.sha256(data).hexdigest()
    url = f"{args.base_url.rstrip('/')}/{artifact_path.name}"
    msg = canonical_msg(args.version, sha, url)
    sig = key.sign(msg)
    manifest = {
        "version": args.version,
        "url": url,
        "sha256": sha,
        "size": len(data),
        "notes": args.notes or "",
        "pub_date": args.date or "",
        "signature": sig.hex(),
    }
    # Optional mirror (not signed — the sha256 above still gates integrity).
    if args.mirror_url:
        manifest["mirror_url"] = f"{args.mirror_url.rstrip('/')}/{artifact_path.name}"
    out = Path(args.out)
    out.write_text(json.dumps(manifest, indent=2, ensure_ascii=False))
    print(f"wrote {out}  ({len(data)} bytes, sha256={sha[:16]}…)")
    print(f"url = {url}")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("keygen").set_defaults(func=cmd_keygen)
    sub.add_parser("pubkey").set_defaults(func=cmd_pubkey)
    s = sub.add_parser("sign")
    s.add_argument("--zip", "--file", dest="artifact", required=True,
                   help="release artifact to sign (Windows .zip or Linux .AppImage)")
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
