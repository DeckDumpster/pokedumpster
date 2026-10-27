#!/usr/bin/env bash
# tests/deploy/health_probe_test.sh — the host-local /health probe carries a machine token (sp-396d3g).
#
# With PKDUMP_HEALTH_REQUIRE_AUTH=1 the app answers /health 401 to anything without a session or
# an allow-listed bearer, and it cannot tell a host probe from the tunnel by address (behind the
# rootless podman publish nothing arrives from loopback). So a no-token probe is a 401 and the
# shared helper, which fetches a client_credentials token first, is a 200. The stub below is the
# app's rule and Pocket ID's token endpoint, on a loopback port; the real middleware is held by
# `cargo test -p pkdump-server machine`.
#
#   §1  no credentials          -> 401   (the red arm: what a bare curl gets)
#   §2  the helper with creds   -> 200
#   §3  a wrong secret, or a partial set of settings -> 000, never a token-less 401 read as an outage
#   §4  heartbeat.sh uses the helper and keeps no second token fetch
# Hermetic: python3 and curl, no container.
set -uo pipefail
REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
FAILED=0
check() { # name expected got
	if [ "$2" = "$3" ]; then echo "  ok   $1"; else echo "  FAIL $1 (expected '$2', got '$3')"; FAILED=1; fi
}

TMP="$(mktemp -d)"
trap 'kill "$SRV" 2>/dev/null; rm -rf "$TMP"' EXIT
cat >"$TMP/stub.py" <<'PY'
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0))).decode()
        ok = "client_secret=good" in body and "grant_type=client_credentials" in body
        self.send_response(200 if ok else 401); self.end_headers()
        self.wfile.write(b'{"access_token":"tok123","token_type":"Bearer"}' if ok else b'{}')
    def do_GET(self):
        ok = self.headers.get("Authorization") == "Bearer tok123"
        self.send_response(200 if ok else 401); self.end_headers()
s = http.server.HTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
PY
python3 -I "$TMP/stub.py" >"$TMP/port" &
SRV=$!
for _ in $(seq 1 50); do [ -s "$TMP/port" ] && break; sleep 0.1; done
PORT="$(cat "$TMP/port")"
[ -n "$PORT" ] || { echo "stub did not start"; exit 1; }

. "$REPO_DIR/deploy/health-probe-lib.sh"
URL="http://127.0.0.1:${PORT}/health"
clean() { unset PKDUMP_PROBE_CLIENT_ID PKDUMP_PROBE_CLIENT_SECRET PKDUMP_PROBE_TOKEN_URL PKDUMP_PROBE_AUDIENCE \
	PKDUMP_HEARTBEAT_CLIENT_ID PKDUMP_HEARTBEAT_CLIENT_SECRET PKDUMP_HEARTBEAT_TOKEN_URL PKDUMP_HEARTBEAT_AUDIENCE; }

echo "§1 a probe with no token"
clean
check "a bare curl of a local listener is 401" 401 "$(curl -s -o /dev/null -w '%{http_code}' "$URL")"
check "the helper with nothing configured sends nothing (401)" 401 "$(pkdump_health_probe "$URL" 5)"

echo "§2 the helper with the machine client"
clean
export PKDUMP_PROBE_CLIENT_ID=probe PKDUMP_PROBE_CLIENT_SECRET=good PKDUMP_PROBE_TOKEN_URL="http://127.0.0.1:${PORT}/token"
check "the helper gets 200" 200 "$(pkdump_health_probe "$URL" 5)"

echo "§3 a failed token is a failed probe"
export PKDUMP_PROBE_CLIENT_SECRET=bad
check "a wrong secret is 000" 000 "$(pkdump_health_probe "$URL" 5 2>/dev/null)"
clean
export PKDUMP_PROBE_CLIENT_ID=probe
check "a partial set is 000" 000 "$(pkdump_health_probe "$URL" 5 2>/dev/null)"

echo "§4 heartbeat.sh goes through the helper"
clean
check "heartbeat sources the helper" 1 "$(grep -c '^\. .*health-probe-lib.sh' "$REPO_DIR/deploy/heartbeat.sh")"
check "heartbeat fetches no token itself" 0 "$(grep -c 'grant_type' "$REPO_DIR/deploy/heartbeat.sh")"

exit $FAILED
