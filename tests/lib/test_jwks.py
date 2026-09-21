#!/usr/bin/env python3
"""JWKS server + RS256 JWT issuer for container-tier isolation tests.

Generates a fresh RSA-2048 key pair per invocation, serves the JWKS at
  http://0.0.0.0:PORT/cdn-cgi/access/certs
and issues RS256-signed tokens on demand.

Containers reach this server via host.containers.internal (set by Podman in
/etc/hosts); pass that hostname as PKDUMP_ACCESS_JWKS_URL.

Sub-commands:
  serve <dir>          Write key + metadata to <dir>/jwks.json, serve on a
                       random port (or PDH_JWKS_PORT if set). Blocks until
                       killed.
  issue <dir> <email>  Issue an RS256 JWT for <email> using the key in <dir>.
                       Prints the token to stdout.
"""

import json
import os
import sys
import time
import base64
from http.server import BaseHTTPRequestHandler, HTTPServer


def _b64url(n: int) -> str:
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def cmd_serve(dirpath: str) -> None:
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.hazmat.primitives import serialization

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    pub = key.public_key()
    pub_numbers = pub.public_numbers()

    pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )
    os.makedirs(dirpath, exist_ok=True)
    with open(os.path.join(dirpath, "key.pem"), "wb") as f:
        f.write(pem)

    kid = "test-kid-1"
    jwks = {
        "keys": [
            {
                "kty": "RSA",
                "kid": kid,
                "n": _b64url(pub_numbers.n),
                "e": _b64url(pub_numbers.e),
            }
        ]
    }
    jwks_body = json.dumps(jwks).encode()

    class JWKSHandler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/cdn-cgi/access/certs":
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(jwks_body)
            else:
                self.send_response(404)
                self.end_headers()

        def log_message(self, *args):
            pass

    port = int(os.environ.get("PDH_JWKS_PORT", "0"))
    server = HTTPServer(("0.0.0.0", port), JWKSHandler)
    port = server.server_address[1]

    aud = "test-aud-pkdump"
    issuer = "https://test.cloudflareaccess.com"
    metadata = {"port": port, "aud": aud, "issuer": issuer, "kid": kid}
    with open(os.path.join(dirpath, "jwks.json"), "w") as f:
        json.dump(metadata, f)

    sys.stdout.write(json.dumps(metadata) + "\n")
    sys.stdout.flush()

    server.serve_forever()


def cmd_issue(dirpath: str, email: str) -> None:
    import jwt as pyjwt

    with open(os.path.join(dirpath, "jwks.json")) as f:
        meta = json.load(f)
    with open(os.path.join(dirpath, "key.pem")) as f:
        private_key = f.read()

    now = int(time.time())
    payload = {
        "sub": f"test-sub-{email}",
        "email": email,
        "iss": meta["issuer"],
        "aud": [meta["aud"]],
        "iat": now,
        "nbf": now,
        "exp": now + 3600,
    }
    token = pyjwt.encode(
        payload,
        private_key,
        algorithm="RS256",
        headers={"kid": meta["kid"]},
    )
    print(token)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        sys.exit(1)
    cmd = sys.argv[1]
    if cmd == "serve":
        cmd_serve(sys.argv[2])
    elif cmd == "issue":
        if len(sys.argv) < 4:
            print("usage: test_jwks.py issue <dir> <email>", file=sys.stderr)
            sys.exit(1)
        cmd_issue(sys.argv[2], sys.argv[3])
    else:
        print(f"unknown command: {cmd}", file=sys.stderr)
        sys.exit(1)
