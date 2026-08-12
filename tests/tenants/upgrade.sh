#!/usr/bin/env bash
# Container-tier gate (pd-hqee): the UPGRADE PATH — an OLD-LAYOUT data volume
# meets the current binary, gets migrated onto opaque database ids, and serves
# the same collection at every step.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/tenants/upgrade.sh          # ~1min after the image is warm
#   KEEP=1 bash tests/tenants/upgrade.sh   # leave WORK + the container up
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
# The previous epic (pd-gckl) shipped a required migration, and production went
# down on the first automated deploy of it. Two alignment beads had both
# verified single-tenant startup, one of them by building a real rootless-Podman
# container from the tree and watching it serve — and both were testing a FRESH
# instance, because `deploy/setup.sh <inst> --test` creates its volume already
# in the new layout. NOBODY EVER STARTED THE NEW BINARY AGAINST A VOLUME THE OLD
# ONE MADE (pd-uoph).
#
# So this gate is not "does it work". It is "does a volume that already has data
# in the OLD shape survive". The old shape is built here deliberately:
#
#   /data/shared.sqlite                       the catalog
#   /data/tenants/collection.sqlite           named by HANDLE — prod's state
#   /data/.../.collection.sqlite-litestream   the sidecar's replication state
#   (no registry.sqlite at all)
#
# ── WHAT IT ASSERTS, AND WHY EACH ONE ───────────────────────────────────────
#   §3 The binary SERVES that volume un-migrated. The migration is deliberately
#      not a startup gate: a required migration is exactly what took prod down,
#      and prod is single-tenant. It must keep running and migrate on its
#      operator's schedule.
#   §4 Migration registers the handle, renames the file to its ULID, and CLEARS
#      Litestream's state directory rather than carrying it — carrying it across
#      a prefix change is what left prod's replica at txid.replica=0 while the
#      unit reported healthy (pd-1717).
#   §5 It serves the SAME COLLECTION afterwards. The comparison is a hash of the
#      shipped CSV export taken before the migration, so a collection that came
#      back empty, short, or reordered fails — which is the failure mode this
#      project can least afford and the one a "did it start?" check cannot see.
#   §6 Running the migration twice changes nothing (the bead asks for it by
#      name).
#   §7 A registry row naming a database that is not there fails CLEANLY, with
#      the id in the message — it does not crash-loop and does not come up on a
#      fresh empty collection.
#   §8 The ROLLBACK, executed on that same migrated volume, and then serving the
#      same collection again.
#
# Prod-safe: its own image tag, its own container name, its own temp directory,
# its own port. It touches no pkdump-* unit, no pkdump-*-data volume, no bucket.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES="${REPO_DIR}/tests/ui/fixtures"

# The shipped image, built here or — when deploy/ci.sh already built it once for
# every gate in the run — tagged from that one. See deploy/image-lib.sh.
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"

# PER-CHECKOUT, for the reason deploy/ci.sh derives its instance the same way:
# several polecats run this concurrently from their own worktrees, and a fixed
# container name means run B's opening `podman rm -f` kills run A mid-suite.
SUFFIX="${PDUP_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
IMAGE="localhost/pkdump:upgrade-${SUFFIX}"
APP_CTR="pkdump-upgrade-${SUFFIX}"
# The host port is PODMAN'S to pick (`-p 127.0.0.1::8080`), read back off the
# container by `start_app` — the way deploy/setup.sh does with PORT=0. Deriving
# it from the checkout hash gave 100 distinct values shared by 19 concurrent
# polecat worktrees, deterministic per checkout and with no retry if the bind
# failed. PDUP_PORT still pins it for a human who wants a known port.
PORT=""

WORK=${WORK:-$(mktemp -d /tmp/pd-upgrade.XXXXXX)}
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

# The shipped image, with the shipped ENTRYPOINT (`pkdump serve --host 0.0.0.0
# --port 8080`) and the shipped PKDUMP_HOME. Starting the binary some other way
# would not be the thing that failed.
start_app() {
	podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true
	# An EMPTY host port in the mapping is how Podman is asked to choose one —
	# the same `:8080` deploy/setup.sh writes into the Quadlet unit for PORT=0.
	podman run -d --name "$APP_CTR" -p "127.0.0.1:${PDUP_PORT:-}:8080" \
		-v "${DATA}:/data:Z" "$IMAGE" >/dev/null
	# Read back off the container rather than from `podman port`: the mapping is
	# fixed at create time and inspect still reports it after the process has
	# exited, which is exactly the case §7 asserts on.
	PORT="$(podman inspect -f '{{ (index .NetworkSettings.Ports "8080/tcp" 0).HostPort }}' "$APP_CTR" 2>/dev/null || true)"
	if [[ -z "$PORT" ]]; then
		echo "  ABORT: podman published no host port for ${APP_CTR}."
		podman logs "$APP_CTR" 2>&1 | sed 's/^/  /'
		exit 1
	fi
}
stop_app() { podman rm -f --ignore "$APP_CTR" >/dev/null 2>&1 || true; }

# Wait for /health, or give up. Echoes up/down so a caller can assert on it —
# §7 is a section that WANTS `down`, so this stays a soft check. Every section
# that needs a server uses `require_up` instead.
wait_up() {
	for _ in $(seq 45); do
		if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/health"; then
			echo up
			return
		fi
		# A container that has already exited will never answer; stop waiting.
		[[ "$(podman inspect -f '{{.State.Status}}' "$APP_CTR" 2>/dev/null)" == "exited" ]] && break
		sleep 1
	done
	echo down
}

# A server that was supposed to come up and did not is an ABORT, not one more
# FAIL line: every assertion after it reads the collection over HTTP, so one
# root cause would print as a run of empty hashes and name none of them — and
# start_app sends the container's stdout to /dev/null, so the reason would never
# reach the log either (pd-z1xb, where this cost a manual `podman logs`).
require_up() { # require_up <label>
	local up
	up="$(wait_up)"
	check "$1" up "$up"
	if [[ "$up" == up ]]; then
		return
	fi
	echo
	echo "  ABORT: the server never answered on 127.0.0.1:${PORT} — everything"
	echo "         after this reads the collection over HTTP and would report"
	echo "         an empty one, not a cause."
	echo "  container: ${APP_CTR}  state: $(podman inspect -f '{{.State.Status}}/{{.State.ExitCode}}' "$APP_CTR" 2>/dev/null || echo missing)"
	echo "  --- podman logs ${APP_CTR} ---"
	podman logs "$APP_CTR" 2>&1 | sed 's/^/  /'
	echo "  --- end of log ---"
	echo "  ${pass} passed, ${fail} failed before the abort"
	exit 1
}

# A one-off `pkdump` against the same data directory — how deploy/TENANTS.md
# tells an operator to run these (`podman run --rm -v <volume>:/data ...`).
pkdump() { podman run --rm -v "${DATA}:/data:Z" --entrypoint pkdump "$IMAGE" "$@"; }

# The collection as the app serves it, hashed. Comparing this across the
# migration is the assertion: an empty, short or reordered collection all fail,
# where "the server answered 200" would not.
collection_hash() { curl -sf "http://127.0.0.1:${PORT}/api/export/csv" | sha256sum | cut -d' ' -f1; }
collection_lines() { curl -sf "http://127.0.0.1:${PORT}/api/export/csv" | grep -c . || true; }

# The tenant databases on the volume, by filename stem.
tenant_stems() { ls "${DATA}/tenants" 2>/dev/null | sed -n 's/\.sqlite$//p' | sort; }

log "1. the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"

log "2. an OLD-LAYOUT data directory — handle-named, no registry"
mkdir -p "${DATA}/tenants"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
cp "${FIXTURES}/collection.sqlite" "${DATA}/tenants/collection.sqlite"
# Litestream's per-database state, as prod has beside its collection. It must
# NOT survive a rename onto a new filename (pd-1717) — §4 is where that lands.
LS_OLD="${DATA}/tenants/.collection.sqlite-litestream"
mkdir -p "${LS_OLD}/ltx/0"
printf 'ltx' >"${LS_OLD}/ltx/0/0001-0001.ltx"
printf '00000000000004e6' >"${LS_OLD}/txid.db"

SEED_ROWS=$(sqlite3 "file:${DATA}/tenants/collection.sqlite?mode=ro" 'SELECT count(*) FROM collection;')
# A fixture with no rows would let every "the collection survived" assertion
# below pass on an empty database. Refuse to run rather than report a green.
if [ "${SEED_ROWS:-0}" -lt 2 ]; then
	echo "  ABORT: the fixture collection has ${SEED_ROWS:-0} rows — this gate proves nothing without data."
	exit 1
fi
check "the volume starts with a handle-named database" "collection" "$(tenant_stems)"
check "and no registry" "absent" \
	"$([ -e "${DATA}/registry.sqlite" ] && echo present || echo absent)"
echo "  ${SEED_ROWS} collection rows on the volume"

log "3. the current binary SERVES that volume, un-migrated (prod's upgrade path)"
start_app
require_up "the server came up on an un-migrated volume"
BEFORE_HASH=$(collection_hash)
BEFORE_LINES=$(collection_lines)
# Belt to that braces: a hash comparison alone would be satisfied by two equally
# empty exports.
check "and serves a non-empty collection" "yes" \
	"$([ "${BEFORE_LINES:-0}" -gt "$SEED_ROWS" ] && echo yes || echo no)"
# It says which database it opened, and that it is not on the model yet. That
# line is the only signal an operator gets that the migration is outstanding.
check "it reports the collection as not yet migrated" "1" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c 'NOT YET MIGRATED' || true)"
check "and names the handle-named file it opened" "yes" \
	"$([ "$(podman logs "$APP_CTR" 2>&1 | grep -c '/data/tenants/collection.sqlite' || true)" -gt 0 ] && echo yes || echo no)"
# `tenant list` sees a database no registered user claims — the drift this
# migration exists to remove.
check "tenant list reports it as unclaimed" "1" \
	"$(pkdump tenant list 2>&1 | grep -c 'collection.sqlite' || true)"

log "4. migrate onto opaque ids"
# Stop the app first: the migration checkpoints each database with
# wal_checkpoint(TRUNCATE) and refuses to move one that is busy.
stop_app
pkdump tenant migrate --dry-run >"$WORK/dry-run.log" 2>&1
check "--dry-run changes nothing" "collection" "$(tenant_stems)"
check "and says what it would do" "1" \
	"$(grep -c 'would be registered and renamed' "$WORK/dry-run.log" || true)"

pkdump tenant migrate >"$WORK/migrate.log" 2>&1
DB_ID=$(tenant_stems)
check "the database is now named by an opaque id, and only that" "1" \
	"$(printf '%s\n' "$DB_ID" | grep -c '^[0-9A-HJKMNP-TV-Z]\{26\}$' || true)"
check "the handle does not appear in the filename" "0" \
	"$(printf '%s' "$DB_ID" | grep -ci collection || true)"
check "the registry now exists" "present" \
	"$([ -e "${DATA}/registry.sqlite" ] && echo present || echo absent)"
check "and joins the handle to that id" "collection|${DB_ID}|active" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" \
		"SELECT handle || '|' || database_id || '|' || state FROM user;")"

# THE pd-1717 ASSERTION. The replica prefix is derived from the filename, so a
# rename starts a new, EMPTY prefix. Litestream's local LTX history describes
# the old one; carried across, it comes up mid-stream against an empty prefix,
# cannot catch it up, and replicates NOTHING while logging "snapshot complete".
check "Litestream's state for the old name is gone" "gone" \
	"$([ -e "$LS_OLD" ] && echo present || echo gone)"
check "and was NOT carried onto the new name" "gone" \
	"$([ -e "${DATA}/tenants/.${DB_ID}.sqlite-litestream" ] && echo present || echo gone)"
check "the migration says the prefix changed and to check replication" "1" \
	"$(grep -c 'txid.replica' "$WORK/migrate.log" || true)"
# The catalog is not a tenant and does not move.
check "the catalog stayed where it was" "present" \
	"$([ -e "${DATA}/shared.sqlite" ] && echo present || echo absent)"

log "5. it serves the SAME collection from its new name"
start_app
require_up "the server came up on the migrated volume"
check "it serves exactly the collection it served before the migration" "$BEFORE_HASH" \
	"$(collection_hash)"
check "it reports the collection as registered" "1" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c "registered as \"collection\" -> database ${DB_ID}" || true)"
check "and nothing warns that it is un-migrated any more" "0" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c 'NOT YET MIGRATED' || true)"

log "6. migrating twice does nothing the second time"
stop_app
pkdump tenant migrate >"$WORK/migrate-2.log" 2>&1
check "the second run reports nothing to do" "1" \
	"$(grep -c 'Nothing to migrate' "$WORK/migrate-2.log" || true)"
check "the database still has exactly one id" "$DB_ID" "$(tenant_stems)"
check "and the registry still has exactly one row" "1" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" 'SELECT count(*) FROM user;')"

log "7. a registry row whose database is missing fails CLEANLY"
# Not a crash loop, and above all not a fresh empty collection served in its
# place. The message has to carry the id, because that is what a restore is
# addressed by (deploy/RESTORE.md).
mv "${DATA}/tenants/${DB_ID}.sqlite" "$WORK/hidden.sqlite"
start_app
check "the server refuses to come up" "down" "$(wait_up)"
check "it exited rather than looping half-configured" "exited" \
	"$(podman inspect -f '{{.State.Status}}' "$APP_CTR" 2>/dev/null || echo missing)"
check "the error names the missing database id" "1" \
	"$(podman logs "$APP_CTR" 2>&1 | grep -c "$DB_ID" || true)"
check "and it did not invent an empty collection in its place" "absent" \
	"$([ -e "${DATA}/tenants/${DB_ID}.sqlite" ] && echo present || echo absent)"
stop_app
mv "$WORK/hidden.sqlite" "${DATA}/tenants/${DB_ID}.sqlite"

log "8. ROLLBACK, on this same migrated volume"
# Exercised, not written down: the bead's second acceptance criterion. A build
# predating this epic looks for tenants/<handle>.sqlite and does not read the
# registry at all, so both halves have to come back.
pkdump tenant unmigrate >"$WORK/unmigrate.log" 2>&1
check "the database is named by its handle again" "collection" "$(tenant_stems)"
check "and the registry has nothing left to say" "0" \
	"$(sqlite3 "file:${DATA}/registry.sqlite?mode=ro" 'SELECT count(*) FROM user;')"
check "the rollback also cleared the Litestream state" "gone" \
	"$([ -e "${DATA}/tenants/.collection.sqlite-litestream" ] && echo present || echo gone)"

start_app
require_up "the server came up on the rolled-back volume"
check "with the same collection it started with" "$BEFORE_HASH" "$(collection_hash)"

# And the volume is back in the state §4 started from, so the migration can be
# run again — which is what makes this a rollback and not a one-way door.
stop_app
pkdump tenant migrate >"$WORK/migrate-3.log" 2>&1
check "it can be migrated again afterwards" "1" \
	"$(tenant_stems | grep -c '^[0-9A-HJKMNP-TV-Z]\{26\}$' || true)"
start_app
require_up "the server came up on the re-migrated volume"
check "still the same collection" "$BEFORE_HASH" "$(collection_hash)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || {
	echo "  --- last 30 lines from ${APP_CTR} ---"
	podman logs "$APP_CTR" 2>&1 | tail -30 | sed 's/^/  /'
	exit 1
}
echo "  PASS — an old-layout volume survives the migration, the rollback, and"
echo "         the migration again, serving the same collection throughout."
