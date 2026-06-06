#!/usr/bin/env python3
"""Mint Ed25519 keys + EdDSA JWTs for the sqld auth spike (pokedumpster-13w fam).

sqld accepts the public key as URL-safe base64 of the raw 32-byte Ed25519 key
(or PKCS#8 PEM). Tokens are EdDSA-signed with claim {"a":"rw"|"ro"}.

  genkey --priv privA.pem --pub pubA.b64     # writes private PEM + public b64, prints b64
  sign   --priv privA.pem --access rw         # prints a signed JWT
"""
import argparse
import base64
import sys
import time

import jwt
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def genkey(a):
    k = Ed25519PrivateKey.generate()
    priv_pem = k.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )
    pub_raw = k.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    pub_b64 = base64.urlsafe_b64encode(pub_raw).decode().rstrip("=")
    with open(a.priv, "wb") as f:
        f.write(priv_pem)
    with open(a.pub, "w") as f:
        f.write(pub_b64)
    print(pub_b64)


def sign(a):
    with open(a.priv, "rb") as f:
        priv = f.read()
    now = int(time.time())
    claims = {"a": a.access, "iat": now, "exp": now + 3600}
    print(jwt.encode(claims, priv, algorithm="EdDSA"))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("genkey")
    g.add_argument("--priv", required=True)
    g.add_argument("--pub", required=True)
    g.set_defaults(fn=genkey)
    s = sub.add_parser("sign")
    s.add_argument("--priv", required=True)
    s.add_argument("--access", default="rw", choices=["rw", "ro"])
    s.set_defaults(fn=sign)
    a = ap.parse_args()
    a.fn(a)
    return 0


if __name__ == "__main__":
    sys.exit(main())
