#!/usr/bin/env bash
# Container-tier gate (pd-4g7c / db-ae5u): what the SHIPPED image does at
# startup and over real HTTP once Access auth is in effect.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/tenants/handles.sh          # ~1min after the image is warm
#   KEEP=1 bash tests/tenants/handles.sh   # leave WORK + the container up
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
# Tenant resolution is now driven by a verified Cloudflare Access identity —
# the `VerifiedIdentity` proof-carrying type whose constructor is private to
# the access module and can only be produced by verifying a real JWT. The
# compile-time barrier is stronger than the old header guard, but the two
# startup decisions that depend on it can only be tested at the process level:
#
#   1. Multi-tenant ON, Access NOT configured → the server refuses to start.
#      `check_multitenant_access` emits a message naming the three env vars.
#      This is asserted in §3, and it is what makes the rest of the gate safe
#      to run: a guard nobody ever tests closed is indistinguishable from a
#      deleted one.
#
#   2. Single-tenant mode (the production shape) is completely unaffected by
#      Access configuration — not just by policy, but by construction: the
#      access layer installs a placeholder identity and the resolver ignores
#      it. §4 asserts this over the shipped binary, over real HTTP, at the
#      bind address `pkdump serve` uses in production.
#
# JWT-based multi-tenant resolution (registered email → database) and the
# leftover-header non-escalation guarantee are asserted in the unit tests in
# crates/pkdump-server/src/lib.rs; they require a JWKS endpoint that the
# container tier has no fixture for.
#
# ── WHAT IT ASSERTS ─────────────────────────────────────────────────────────
#   §3 Multi-tenant ON, no Access env vars → refuses to start. The error
#      names `PKDUMP_ACCESS_TEAM_DOMAIN` so an operator knows what to set.
#   §4 SINGLE-TENANT MODE IS UNAFFECTED — the server comes up, and /api/
#      answers without any JWT. The header check that used to fire here is
#      gone; what matters is that the access layer does not block a request
#      that should sail through. This is the only mode production runs.
#
# Prod-safe: its own image tag, container name, temp directory and port. It
# touches no pkdump-* unit, no pkdump-*-data volume, no bucket.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES="${REPO_DIR}/tests/ui/fixtures"

# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"

# PER-CHECKOUT, for the reason deploy/ci.sh derives its instance the same way:
# several polecats run this concurrently from their own worktrees, and a fixed
# container name means run B's opening `podman rm -f` kills run A mid-suite.
SUFFIX="${PDH_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
IMAGE="localhost/pkdump:handles-${SUFFIX}"
APP_CTR="pkdump-handles-${SUFFIX}"
PORT=""

WORK=${WORK:-$(mktemp -d /tmp/pd-handles.XXXXXX)}
DATA="$WORK/data"

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
	# The image too (pd-5aba). The tag carries the checkout hash, so every
	# worktree that ever ran this gate left one behind and nothing collected
	# them. `rmi -f` on a name an image shares with others only UNTAGS it, so
	# under PKDUMP_PREBUILT_IMAGE the gates running beside this one keep theirs.
	podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
	rm -rf "$WORK"
}
trap cleanup EXIT

# The shipped image with the shipped ENTRYPOINT and PKDUMP_HOME.
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

log "1. the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"

log "2. a data directory with alice provisioned"
mkdir -p "$DATA"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
pkdump tenant create alice >/dev/null
check "alice is registered" "1" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT count(*) FROM user WHERE state = 'active' AND handle = 'alice';")"

log "3. multi-tenant ON, no Access env vars → refuses to start"
# PKDUMP_USER is still needed: `pkdump serve` resolves the process's own
# collection up front regardless of multi-tenant mode. Without it the
# container exits for a different reason before we can assert the Access guard.
start_app -e PKDUMP_MULTITENANT=1 -e PKDUMP_USER=alice
check "it never listens" "down" "$(wait_up)"
check "the process exited non-zero" "exited/1" "$(app_state)"
check "and named the first required env var" "1" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c 'PKDUMP_ACCESS_TEAM_DOMAIN' || true)"
stop_app

log "4. SINGLE-TENANT MODE IS UNAFFECTED"
# Production's mode — no Access env vars, one tenant, the header is not read.
# The access layer installs a synthetic placeholder identity (db-ae5u) so the
# tenant layer does not fail; Tenants::resolve ignores it in single mode.
start_app -e PKDUMP_USER=alice
require_up "the server came up with no Access configured"
check "api answers without a JWT" "200" \
	"$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/api/collection")"
# A leftover x-pkdump-tenant header is silently ignored — does not flip the
# status to 400 or 401.
check "a leftover tenant header is ignored" "200" \
	"$(curl -s -o /dev/null -w '%{http_code}' \
		-H 'x-pkdump-tenant: mallory' "http://127.0.0.1:${PORT}/api/collection")"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || {
	echo "  --- last 30 lines from ${APP_CTR} ---"
	podman logs "$APP_CTR" 2>&1 | tail -30 | sed 's/^/  /'
	exit 1
}
echo "  PASS — multi-tenant refuses without Access config, and single-tenant mode"
echo "         serves /api/ without a JWT."
