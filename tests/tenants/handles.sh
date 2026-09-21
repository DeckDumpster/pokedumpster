#!/usr/bin/env bash
# Container-tier gate: what the SHIPPED image answers to multi-tenant requests,
# using Cloudflare Access JWT authentication (db-ae5u).
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/tenants/handles.sh          # ~1min after the image is warm
#   KEEP=1 bash tests/tenants/handles.sh   # leave WORK + the container up
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
# The auth layer (db-ae5u) changed multi-tenant resolution from a plain
# x-pkdump-tenant header to Cloudflare Access JWT verification. This gate
# runs the shipped image against a TEST JWKS server (tests/lib/test_jwks.py)
# that issues real RS256 tokens, so it exercises the actual JWT verification
# path rather than a stub. The isolation claim rests on:
#
#   - the ONLY way to identify a tenant is the email from a verified JWT, and
#   - the ONLY way to produce a VerifiedIdentity (access::layer's output) is to
#     present a valid JWT to the access middleware — the constructor is private.
#
# This gate is the container-tier complement to the unit test
# `one_tenant_cannot_reach_another_tenants_collection` in
# crates/pkdump-server/src/lib.rs.
#
# ── WHAT IT ASSERTS ─────────────────────────────────────────────────────────
#   §3 A JWKS server is started; alice's email is bound in the data directory.
#   §4 Multi-tenant without Access vars refuses to start — Cloudflare Access
#      is required, not optional.
#   §5 A valid JWT for a bound email is served — the happy path.
#   §6 A valid JWT for an unbound email is a 403, naming the fix.
#   §7 An invalid JWT is a 401 — the server rejects it before resolving.
#   §8 No JWT at all is a 401 — there is no ambient identity.
#   §9 Nothing above provisioned anything new.
#  §10 SINGLE-TENANT MODE IS UNAFFECTED — no JWT is required, no email is
#      checked. This is the only mode production MUST run unless the full
#      Access setup is in place.
#
# Prod-safe: its own image tag, container name, temp directory and port. It
# touches no pkdump-* unit, no pkdump-*-data volume, no bucket.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES="${REPO_DIR}/tests/ui/fixtures"

# The shipped image, built here or — when deploy/ci.sh already built it once for
# every gate in the run — tagged from that one. See deploy/image-lib.sh.
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
# Bounded condition polling, in one place for every harness (pd-86er).
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"

# PER-CHECKOUT, for the reason deploy/ci.sh derives its instance the same way:
# several polecats run this concurrently from their own worktrees, and a fixed
# container name means run B's opening `podman rm -f` kills run A mid-suite.
SUFFIX="${PDH_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
IMAGE="localhost/pkdump:handles-${SUFFIX}"
APP_CTR="pkdump-handles-${SUFFIX}"
# The host port is PODMAN'S to pick (`-p 127.0.0.1::8080`), read back off the
# container by `start_app`.
PORT=""

WORK=${WORK:-$(mktemp -d /tmp/pd-handles.XXXXXX)}
DATA="$WORK/data"
JWKS_DIR="$WORK/jwks"
JWKS_PID=""

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving $APP_CTR, $IMAGE and WORK=$WORK in place."
		return
	fi
	podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true
	[[ -n "$JWKS_PID" ]] && kill "$JWKS_PID" 2>/dev/null || true
	# The image too (pd-5aba).
	podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
	rm -rf "$WORK"
}
trap cleanup EXIT

# The shipped image with the shipped ENTRYPOINT and PKDUMP_HOME; the only thing
# added is the env vars that switch resolution on.
start_app() { # start_app [-e VAR=VAL ...]
	podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true
	podman run -d --name "$APP_CTR" -p "127.0.0.1:${PDH_PORT:-}:8080" \
		-v "${DATA}:/data:Z" "$@" "$IMAGE" >/dev/null
	PORT="$(podman inspect -f '{{ (index .NetworkSettings.Ports "8080/tcp" 0).HostPort }}' "$APP_CTR" 2>/dev/null || true)"
	if [[ -z "$PORT" ]]; then
		echo "  ABORT: podman published no host port for ${APP_CTR}."
		podman logs "$APP_CTR" 2>&1 | sed 's/^/  /'
		exit 1
	fi
}
stop_app() { podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true; }

APP_ANSWERING=0
app_settled() {
	if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/health"; then
		APP_ANSWERING=1
		return 0
	fi
	[[ "$(podman inspect -f '{{.State.Status}}' "$APP_CTR" 2>/dev/null)" == "exited" ]]
}
wait_up() {
	APP_ANSWERING=0
	wait_until 45 0.25 app_settled || true
	[[ $APP_ANSWERING == 1 ]] && echo up || echo down
}

app_state() { podman inspect -f '{{.State.Status}}/{{.State.ExitCode}}' "$APP_CTR" 2>/dev/null || echo missing; }

require_up() { # require_up <label>
	local up
	up="$(wait_up)"
	check "$1" up "$up"
	if [[ "$up" == up ]]; then
		return
	fi
	echo
	echo "  ABORT: the server never answered on 127.0.0.1:${PORT} — everything"
	echo "         after this asserts over HTTP and would report 000, not a cause."
	echo "  container: ${APP_CTR}  state: $(app_state)"
	echo "  --- podman logs ${APP_CTR} ---"
	podman logs "$APP_CTR" 2>&1 | sed 's/^/  /'
	echo "  --- end of log ---"
	echo "  ${pass} passed, ${fail} failed before the abort"
	exit 1
}

pkdump() { podman run --rm -v "${DATA}:/data:Z" --entrypoint pkdump "$IMAGE" "$@"; }

# The Access JWT, passed in the header Cloudflare injects on proxied requests.
# In a real deployment the browser carries a CF_Authorization cookie; in this
# test we use the header form which the server accepts equally.
api_status() { # api_status <jwt>
	curl -s -o /dev/null -w '%{http_code}' \
		-H "Cf-Access-Jwt-Assertion: $1" \
		"http://127.0.0.1:${PORT}/api/collection"
}
api_status_anonymous() {
	curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/api/collection"
}

tenant_files() { ls "${DATA}/tenants" 2>/dev/null | grep '\.sqlite$' | sort; }

log "1. the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"

log "2. a data directory provisioned with two users"
mkdir -p "$DATA"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
pkdump tenant create alice >/dev/null
pkdump tenant create bob >/dev/null
pkdump tenant detach bob --yes >/dev/null
# An unregistered database in tenants/ — the registry is the only lookup path,
# so a file with no row cannot be reached regardless of its name.
cp "${FIXTURES}/collection.sqlite" "${DATA}/tenants/ghost.sqlite"
check "one registered active user" "1" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT count(*) FROM user WHERE state = 'active' AND handle = 'alice';")"
check "bob's row survives, detached" "detached" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT state FROM user WHERE handle = 'bob';")"
BEFORE_FILES="$(tenant_files)"

log "3. JWKS server started; alice's email bound in the registry"
# tests/lib/test_jwks.py generates a fresh RSA-2048 key pair, writes its JWKS
# to JWKS_DIR/jwks.json, and serves it at /cdn-cgi/access/certs on a random
# port. It binds to 0.0.0.0 so the app container can reach it via
# host.containers.internal (set by Podman in the container's /etc/hosts).
mkdir -p "$JWKS_DIR"
python3 "${REPO_DIR}/tests/lib/test_jwks.py" serve "$JWKS_DIR" &
JWKS_PID=$!
for i in $(seq 1 40); do
	[[ -f "$JWKS_DIR/jwks.json" ]] && break
	sleep 0.1
done
check "JWKS server is ready" "1" "$([[ -f "$JWKS_DIR/jwks.json" ]] && echo 1 || echo 0)"
JWKS_PORT=$(python3 -c "import json; print(json.load(open('$JWKS_DIR/jwks.json'))['port'])")
JWKS_AUD=$(python3 -c "import json; print(json.load(open('$JWKS_DIR/jwks.json'))['aud'])")
JWKS_ISSUER=$(python3 -c "import json; print(json.load(open('$JWKS_DIR/jwks.json'))['issuer'])")
# The URL the container uses: host.containers.internal is Podman's name for the
# host machine, present in every container's /etc/hosts.
JWKS_URL="http://host.containers.internal:${JWKS_PORT}/cdn-cgi/access/certs"

# Bind alice's email — this is the runtime step that connects an authenticated
# identity to a tenant. Without it, alice's JWT would get a 403.
pkdump tenant identity add alice --email alice@example.com >/dev/null
check "alice's email is bound" "1" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT count(*) FROM user_identity ui JOIN user u USING (database_id)
		 WHERE u.handle='alice' AND ui.email='alice@example.com';")"

# Issue test tokens. ALICE_TOKEN is signed by the test JWKS key and carries
# alice@example.com; UNBOUND_TOKEN carries an email nobody is registered under.
ALICE_TOKEN=$(python3 "${REPO_DIR}/tests/lib/test_jwks.py" issue "$JWKS_DIR" alice@example.com)
UNBOUND_TOKEN=$(python3 "${REPO_DIR}/tests/lib/test_jwks.py" issue "$JWKS_DIR" unbound@example.com)

log "4. multi-tenant WITHOUT Access vars refuses to start"
# The guard (check_multitenant_access) requires PKDUMP_ACCESS_TEAM_DOMAIN and
# PKDUMP_ACCESS_AUD at startup. Without them the server exits immediately —
# §5 onwards can only be reached by testing AGAINST this guard, not by removing
# it. An escape hatch that nobody ever tests closed is indistinguishable from a
# guard that has been deleted.
start_app -e PKDUMP_MULTITENANT=1 -e PKDUMP_USER=alice
check "it never listens" "down" "$(wait_up)"
check "the process exited non-zero" "exited/1" "$(app_state)"
# ANCHOR ON THE REFUSAL LINE, NOT THE PHRASE. check_multitenant_access bails with
# a multi-line message that says "Cloudflare Access" on two of its lines, so
# `grep -c 'Cloudflare Access'` scores 2 for a CORRECT refusal and this check
# failed on green code (PR #145). The sentence below appears exactly once and is
# the one the check is named after.
check "and said that Cloudflare Access was missing" "1" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c 'refusing to start: multi-tenant resolution is on' || true)"
stop_app

log "5. multi-tenant with Access: a bound email JWT is served"
# The three Access vars configure the JWT verifier. JWKS_URL points to our
# test JWKS server, which the container reaches via host.containers.internal.
# PKDUMP_USER is set even with resolution on: pkdump serve resolves the
# process's own collection up front either way.
start_app \
	-e PKDUMP_MULTITENANT=1 \
	-e PKDUMP_ACCESS_TEAM_DOMAIN="$JWKS_ISSUER" \
	-e PKDUMP_ACCESS_AUD="$JWKS_AUD" \
	-e PKDUMP_ACCESS_JWKS_URL="$JWKS_URL" \
	-e PKDUMP_USER=alice
require_up "the server came up with Access configured"
check "alice's JWT is served" "200" "$(api_status "$ALICE_TOKEN")"

log "6. a valid JWT for an UNBOUND email is a 403"
# The JWT is correctly signed and not expired; the verifier accepts it. But
# the email (unbound@example.com) has no binding in the registry, so the
# resolver answers 403 and names the command that fixes it.
check "unbound email -> 403" "403" "$(api_status "$UNBOUND_TOKEN")"
# Verify the 403 names the fix command (not just a generic error).
UNBOUND_BODY=$(curl -s \
	-H "Cf-Access-Jwt-Assertion: $UNBOUND_TOKEN" \
	"http://127.0.0.1:${PORT}/api/collection" || true)
check "the 403 names the fix" "1" \
	"$(printf '%s' "$UNBOUND_BODY" | grep -c 'pkdump tenant identity add' || true)"

log "7. an INVALID JWT is a 401"
# The server rejects the token before it ever reaches the tenant resolver.
check "invalid JWT -> 401" "401" "$(api_status not.a.valid.jwt)"

log "8. no JWT at all is a 401 — there is no ambient identity"
check "anonymous -> 401" "401" "$(api_status_anonymous)"

log "9. nothing above provisioned anything new"
check "the tenant databases are exactly the ones we made" "$BEFORE_FILES" \
	"$(tenant_files)"
check "and nothing was created beside the catalog" "absent" \
	"$([ -e "${DATA}/collection.sqlite" ] && echo present || echo absent)"

log "10. SINGLE-TENANT MODE IS UNAFFECTED — no JWT is required"
# Production's mode. In single-tenant mode the access layer is not wired at all:
# there is no PKDUMP_MULTITENANT=1 and no Access config. Requests without a JWT
# — or with an invalid one — are served normally. This is the gate that must
# not be changed by improvements to multi-tenant security.
stop_app
start_app -e PKDUMP_USER=alice
require_up "the server came up in single-tenant mode"
check "anonymous request is served" "200" "$(api_status_anonymous)"
check "invalid JWT header is ignored" "200" "$(api_status not.a.valid.jwt)"
check "alice's JWT header is also fine but unneeded" "200" "$(api_status "$ALICE_TOKEN")"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || {
	echo "  --- last 30 lines from ${APP_CTR} ---"
	podman logs "$APP_CTR" 2>&1 | tail -30 | sed 's/^/  /'
	exit 1
}
echo "  PASS — Access config required for multi-tenant, JWT verified against JWKS,"
echo "         bound email is served, unbound is 403, invalid is 401, and"
echo "         single-tenant mode requires no auth at all."
