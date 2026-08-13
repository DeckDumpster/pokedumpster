#!/usr/bin/env bash
# Container-tier gate (pd-4g7c): what the SHIPPED image answers to a tenant
# header, over real HTTP, in each of the four cases that matter.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/tenants/handles.sh          # ~1min after the image is warm
#   KEEP=1 bash tests/tenants/handles.sh   # leave WORK + the container up
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
# The resolver used to accept the tenant header unvalidated, on the argument
# that a value the registry never issued simply misses the lookup. That is true
# and it is not the whole story: `pkdump tenant create` holds a handle to a
# rule and the registry stores it under a CHECK of that same rule, so a header
# outside the rule names something NO ROW COULD EVER HAVE HELD. Answering it
# "404 no such tenant" states that a well-formed name is unused. That is false.
# The request is malformed, 400 says so, and OWASP's multi-tenant guidance
# names the unvalidated header as the anti-pattern in as many words.
#
# The distinction is a STATUS CODE, which is to say it is an artefact of the
# wire and not of `Tenants::resolve`. A 400 the middleware flattened into a 404
# on the way out would pass the unit tests in crates/pkdump-server and fail
# every caller. So it is asserted here, through the shipped ENTRYPOINT, against
# a volume `pkdump tenant create` provisioned, with curl reading the status line.
#
# ── WHAT IT ASSERTS ─────────────────────────────────────────────────────────
#   §3 Multi-tenant resolution on a NON-LOOPBACK bind, with no second opt-in,
#      refuses to start at all. That refusal is the reason §4 onwards must ask
#      for the escape hatch, so it is pinned here rather than assumed.
#   §4 Registered handle -> 200. The rule refuses nothing it should admit.
#   §5 Malformed handle -> 400, naming the rule, NOT echoing what was sent.
#   §6 Well-formed but not an active user -> 404: never registered, detached,
#      and a real unregistered database sitting in tenants/ under that name.
#   §7 No header at all -> 400. There is no ambient tenant.
#   §8 Nothing above created a database, and nothing outside tenants/ was
#      touched. A refusal must not provision.
#   §9 SINGLE-TENANT MODE IS UNAFFECTED — the header is not read at all, so a
#      malformed one is served exactly like any other request. This is the
#      only mode production runs, and this gate must not be what changes it.
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

# The shipped image, built here or — when deploy/ci.sh already built it once for
# every gate in the run — tagged from that one. See deploy/image-lib.sh.
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"

# PER-CHECKOUT, for the reason deploy/ci.sh derives its instance the same way:
# several polecats run this concurrently from their own worktrees, and a fixed
# container name means run B's opening `podman rm -f` kills run A mid-suite.
SUFFIX="${PDH_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
IMAGE="localhost/pkdump:handles-${SUFFIX}"
APP_CTR="pkdump-handles-${SUFFIX}"
# The host port is PODMAN'S to pick (`-p 127.0.0.1::8080`), read back off the
# container by `start_app` — the way deploy/setup.sh does with PORT=0. Deriving
# it from the checkout hash gave 100 distinct values shared by 19 concurrent
# polecat worktrees, deterministic per checkout and with no retry if the bind
# failed: the same collision class the per-checkout instance name already fixed
# one layer up. PDH_PORT still pins it for a human who wants a known port.
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
		echo "KEEP=1 — leaving $APP_CTR and WORK=$WORK in place."
		return
	fi
	podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true
	rm -rf "$WORK"
}
trap cleanup EXIT

# The shipped image with the shipped ENTRYPOINT and PKDUMP_HOME; the only thing
# added is the opt-in that switches resolution on. Starting the binary some
# other way would not be the thing that ships.
start_app() { # start_app [-e VAR=VAL ...]
	podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true
	# An EMPTY host port in the mapping is how Podman is asked to choose one —
	# the same `:8080` deploy/setup.sh writes into the Quadlet unit for PORT=0.
	podman run -d --name "$APP_CTR" -p "127.0.0.1:${PDH_PORT:-}:8080" \
		-v "${DATA}:/data:Z" "$@" "$IMAGE" >/dev/null
	# Read back off the container rather than from `podman port`: the mapping is
	# fixed at create time and inspect still reports it after the process has
	# exited, which is exactly the case §3 asserts on.
	PORT="$(podman inspect -f '{{ (index .NetworkSettings.Ports "8080/tcp" 0).HostPort }}' "$APP_CTR" 2>/dev/null || true)"
	if [[ -z "$PORT" ]]; then
		echo "  ABORT: podman published no host port for ${APP_CTR}."
		podman logs "$APP_CTR" 2>&1 | sed 's/^/  /'
		exit 1
	fi
}
stop_app() { podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true; }

# The condition: answering, or provably never going to. APP_ANSWERING is how the
# caller tells those two apart — `wait_until` only reports that the wait ended.
APP_ANSWERING=0
app_settled() {
	if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/health"; then
		APP_ANSWERING=1
		return 0
	fi
	# A container that has already exited will never answer; stop waiting.
	[[ "$(podman inspect -f '{{.State.Status}}' "$APP_CTR" 2>/dev/null)" == "exited" ]]
}
wait_up() {
	APP_ANSWERING=0
	# Four times a second, against the SAME 45-second bound (pd-86er).
	wait_until 45 0.25 app_settled || true
	[[ $APP_ANSWERING == 1 ]] && echo up || echo down
}

# The state of the container, as one field, for both the assertion in §3 and the
# abort below.
app_state() { podman inspect -f '{{.State.Status}}/{{.State.ExitCode}}' "$APP_CTR" 2>/dev/null || echo missing; }

# A server that never came up is an ABORT, not one more FAIL line.
#
# Every assertion after a start_app reads an HTTP status code, so a container
# that exited before it listened turns ONE root cause into a run of `got 000`
# and names none of them — which is how this gate reached CI as "10 passed, 16
# failed" with the actual refusal (a one-line message on the container's stdout,
# discarded by start_app) never reaching the log at all. Stop at the first one
# and print what the container said.
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

# A one-off `pkdump` against the same data directory — how deploy/TENANTS.md
# tells an operator to run these.
pkdump() { podman run --rm -v "${DATA}:/data:Z" --entrypoint pkdump "$IMAGE" "$@"; }

# The status line for a request naming `$1` as its tenant. The header is passed
# with `-H 'name: value'` so the value reaches the server verbatim, spaces and
# all; `-H 'name;'` is curl's way of sending it empty.
api_status() { # api_status <handle>
	local hdr="x-pkdump-tenant: $1"
	[[ -z "$1" ]] && hdr="x-pkdump-tenant;"
	curl -s -o /dev/null -w '%{http_code}' -H "$hdr" \
		"http://127.0.0.1:${PORT}/api/collection"
}
api_body() { # api_body <handle>
	# `|| true`: a refused connection is exit 7, and under `set -e` a failed
	# command substitution in an assignment takes the whole script with it —
	# which turns "the server did not come up" into a silent early exit instead
	# of the run of FAIL lines that says so.
	curl -s -H "x-pkdump-tenant: $1" "http://127.0.0.1:${PORT}/api/collection" || true
}
# No tenant header at all — not an empty one.
api_status_anonymous() {
	curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/api/collection"
}

# The tenant DATABASES, by filename. Only `*.sqlite`: serving a tenant leaves a
# `-wal` and a `-shm` beside theirs, which is SQLite doing its job and not a
# database appearing.
tenant_files() { ls "${DATA}/tenants" 2>/dev/null | grep '\.sqlite$' | sort; }

log "1. the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"

log "2. a data directory two users were provisioned into"
mkdir -p "$DATA"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
pkdump tenant create alice >/dev/null
pkdump tenant create bob >/dev/null
# Detached: the handle is released, the database is kept. A name that WAS held
# is the sharpest 404 there is — it is well-formed by construction.
pkdump tenant detach bob --yes >/dev/null
# And a real, openable collection in tenants/ that no registry row names, under
# a perfectly ordinary handle. This is what makes §5 more than a spelling test.
cp "${FIXTURES}/collection.sqlite" "${DATA}/tenants/ghost.sqlite"
check "one registered user, and it is not named by its handle" "1" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT count(*) FROM user WHERE state = 'active' AND handle = 'alice';")"
check "and bob's row survives, detached" "detached" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT state FROM user WHERE handle = 'bob';")"
BEFORE_FILES="$(tenant_files)"

log "3. an EXPOSED multi-tenant bind refuses to start — no second opt-in, no server"
# This container publishes 8080, so the shipped ENTRYPOINT necessarily binds
# 0.0.0.0, which is the one combination `check_bind` refuses: multi-tenant
# resolution takes the tenant from a header nothing authenticates, so off-box
# reachability hands every collection to whoever can reach the port.
#
# §4 onwards opens the escape hatch, and an escape hatch nobody ever tests
# closed is indistinguishable from a guard that has been deleted. So the refusal
# is asserted first, here, and this section is what makes it impossible to
# "fix" §4 by weakening the product.
start_app -e PKDUMP_MULTITENANT=1 -e PKDUMP_USER=alice
check "it never listens" "down" "$(wait_up)"
check "the process exited non-zero" "exited/1" "$(app_state)"
check "and said which opt-in would allow it" "1" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c 'PKDUMP_MULTITENANT_INSECURE_BIND' || true)"
stop_app

log "4. multi-tenant: a registered handle is served"
# PKDUMP_MULTITENANT_INSECURE_BIND is the second opt-in §3 just proved is
# required. This gate is entitled to it and production is not: this is a
# throwaway container, published on 127.0.0.1 only, holding two fixture
# collections that exist for the length of this script. Production's exposure is
# real and its data is the user's, which is why deploy/ sets neither variable —
# do NOT copy this line into a deploy script or a unit file.
#
# PKDUMP_USER is set even with resolution ON: `pkdump serve` resolves the
# process's own collection up front either way, so that a data directory it
# cannot make sense of is a startup failure and never a server that comes up
# empty. Leave it at the default `collection` on a volume where nobody is
# registered under that handle and the container exits before it listens.
start_app -e PKDUMP_MULTITENANT=1 -e PKDUMP_MULTITENANT_INSECURE_BIND=1 -e PKDUMP_USER=alice
require_up "the server came up with resolution on"
check "alice is served" "200" "$(api_status alice)"

log "5. a MALFORMED handle is a 400 — the request, not the tenant, is wrong"
# Every one of these is a string the registry's CHECK would refuse, so no row
# could ever have held it. Under the un-validated resolver they were all 404.
for bad in "Alice" "-flag" "a/b" "alice.sqlite" "has space" "../shared" \
	"../../etc/passwd" "01JQ8Z2C4E6G8K0M2P4R6T8V0X" ""; do
	check "${bad:-<empty>} -> 400" "400" "$(api_status "$bad")"
done
BODY="$(api_body "A/../lice")"
check "the 400 says what a handle may be" "1" \
	"$(printf '%s' "$BODY" | grep -c 'starting with a letter or digit' || true)"
check "and does not echo the header back" "0" \
	"$(printf '%s' "$BODY" | grep -c 'A/\.\./lice' || true)"

log "6. a WELL-FORMED handle that is not an active user is a 404"
check "never registered -> 404" "404" "$(api_status mallory)"
check "detached -> 404" "404" "$(api_status bob)"
# The pd-rqgv canary, restated as the other half of this distinction: `ghost`
# passes the boundary check — it is a fine handle — and still resolves to
# nothing, because the header is a lookup key and not a filename.
check "an unregistered database in tenants/ -> 404" "404" "$(api_status ghost)"

log "7. no tenant named at all is a 400"
check "anonymous -> 400" "400" "$(api_status_anonymous)"

log "8. nothing above provisioned anything"
check "the tenant databases are exactly the ones we made" "$BEFORE_FILES" \
	"$(tenant_files)"
check "and nothing was created beside the catalog" "absent" \
	"$([ -e "${DATA}/collection.sqlite" ] && echo present || echo absent)"

log "9. SINGLE-TENANT MODE IS UNAFFECTED — the header is not read at all"
# Production's mode. The check added at the boundary lives inside the
# multi-tenant branch of the resolver, and if it did not, this is where that
# would show: a malformed header would start refusing requests on an instance
# that never opted in.
stop_app
start_app -e PKDUMP_USER=alice
require_up "the server came up with resolution off"
check "a malformed header is ignored, not refused" "200" "$(api_status Alice)"
check "so is a traversing one" "200" "$(api_status ../shared)"
check "so is an unknown one" "200" "$(api_status mallory)"
check "and so is no header at all" "200" "$(api_status_anonymous)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || {
	echo "  --- last 30 lines from ${APP_CTR} ---"
	podman logs "$APP_CTR" 2>&1 | tail -30 | sed 's/^/  /'
	exit 1
}
echo "  PASS — malformed is 400, unknown is 404, and single-tenant mode does"
echo "         not read the header at all."
