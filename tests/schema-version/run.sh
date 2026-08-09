#!/usr/bin/env bash
# Container-tier gate (pd-ja38): the schema-version gate, exercised against a
# PROD-SHAPED instance — the shipped image's own entrypoint
# (`pkdump serve --host 0.0.0.0 --port 8080`), the shipped Quadlet unit, a real
# data volume.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/schema-version/run.sh          # ~1 image build + 2 starts
#   KEEP=1 bash tests/schema-version/run.sh   # leave the instance up to poke at
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# On 2026-08-08 prod went down because a required migration shipped with no
# upgrade-path test. Every verification anyone had was FRESH-INSTALL shaped:
# built on volumes already in the new layout, with nobody ever starting the new
# binary against a volume the OLD one made.
#
# This bead adds a gate to the code path that opens prod's databases, and every
# database in existence — prod's included — is `user_version` 0. So the two
# things asserted here are the two a unit test on a tempdir cannot settle:
#
#   §1 ADOPTION. An UNVERSIONED volume (both databases forced to 0, which is
#      exactly what the pre-gate binary leaves behind) boots, SERVES, and comes
#      out stamped. This is the release blocker: if it is wrong, prod does not
#      start.
#   §2 REFUSAL. A collection database written by a NEWER build stops the server
#      dead, with both version numbers in the log. Rollback (`pkdump tenant
#      revert`) is only safe because of this — an older binary must refuse
#      rather than quietly operate on a schema it does not understand.
#   §3 REPORTING (pd-enje). `pkdump tenant list` names each tenant's own schema
#      version — including the one §2 just made unopenable. An operator whose
#      server will not start needs the report to say WHICH database is from the
#      future; a report that failed the same way the server did would name
#      nothing. Read-only in the same breath: asking must not stamp or migrate.
#
# Prod-safe: its own per-checkout instance name, its own volume, its own port.
# Touches no pkdump-*@prod unit, no pkdump-prod-data volume, no real bucket.
set -euo pipefail

# systemctl --user / podman need XDG_RUNTIME_DIR; CI runners and
# non-interactive shells often lack it.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Per-checkout instance name, for the reason spelled out in deploy/ci.sh: the
# swarm runs several polecats per rig, each from its own worktree, and a shared
# name means one run's teardown destroys another's container mid-suite.
INSTANCE="${PKDUMP_SV_INSTANCE:-sv-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
SERVICE_NAME="pkdump-${INSTANCE}"
CONTAINER="systemd-${SERVICE_NAME}"
VOLUME="pkdump-${INSTANCE}-data"
IMAGE="localhost/pkdump:${INSTANCE}"

# Paths inside the data volume — the layout deploy/TENANTS.md describes: the
# catalog at the root, one collection per tenant under tenants/.
CATALOG_DB=/data/shared.sqlite
COLLECTION_DB=/data/tenants/collection.sqlite

command -v sqlite3 >/dev/null 2>&1 || {
	echo "ERROR: sqlite3 not found on the host (needed to inspect the volume's databases)."
	exit 1
}

# Every SQLite call here goes through `sq`, for the same reason
# tests/litestream/run.sh does it: a bare `sqlite3` has no busy timeout, and
# under `set -e` a single SQLITE_BUSY takes the whole CI run down.
sq() { sqlite3 -cmd '.timeout 5000' "$@"; }

WORK=$(mktemp -d /tmp/pkdump-sv.XXXXXX)

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		echo "        expected: $2"
		echo "        actual:   $3"
		fail=$((fail + 1))
	fi
}

contains() { # contains <label> <needle> <haystack>
	if [[ "$3" == *"$2"* ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		echo "        missing: $2"
		echo "        in: ---"
		echo "$3"
		echo "        ---"
		fail=$((fail + 1))
	fi
}

cleanup() {
	rm -rf "$WORK"
	if [[ -n "${KEEP:-}" ]]; then
		echo "KEEP set — instance '${INSTANCE}' left in place."
		return
	fi
	bash "$REPO_DIR/deploy/teardown.sh" "$INSTANCE" --purge >/dev/null 2>&1 || true
}
trap cleanup EXIT

# ── Reaching into the data volume ───────────────────────────────────────────
# Rootless podman maps a volume's files into a user namespace, so the host
# cannot read them directly. Both directions go through a throwaway container
# with the volume mounted — the same one-off-container trick deploy/setup.sh
# uses to seed the fixture.

volume_container() { # -> prints the name of a fresh temp container
	local ctr="pkdump-sv-$$-${RANDOM}"
	podman run -d --name "$ctr" -v "${VOLUME}:/data:Z" \
		--entrypoint sleep "$IMAGE" infinity >/dev/null
	printf '%s' "$ctr"
}

vol_pull() { # vol_pull <db-in-volume> -> prints the host path it landed at
	local base ctr dest
	base=$(basename "$1")
	dest=$(mktemp -d "$WORK/pull.XXXXXX")
	ctr=$(volume_container)
	podman cp "${ctr}:$1" "$dest/$base"
	# The sidecars come too when they exist: a version that has been written
	# but not yet checkpointed lives in the WAL, and a main file read on its
	# own would report the value from before the server started.
	podman cp "${ctr}:$1-wal" "$dest/$base-wal" 2>/dev/null || true
	podman cp "${ctr}:$1-shm" "$dest/$base-shm" 2>/dev/null || true
	podman rm -f "$ctr" >/dev/null 2>&1 || true
	printf '%s' "$dest/$base"
}

vol_push() { # vol_push <host-db> <db-in-volume>
	local ctr
	ctr=$(volume_container)
	podman cp "$1" "${ctr}:$2"
	# The sidecars described the file we just replaced. Left behind, the next
	# open would replay their frames on top of the bytes we put there —
	# the WAL-correctness trap pokedumpster-lxm was filed for.
	podman exec "$ctr" rm -f "$2-wal" "$2-shm"
	podman rm -f "$ctr" >/dev/null 2>&1 || true
}

read_version() { # read_version <db-in-volume>
	sq "$(vol_pull "$1")" 'PRAGMA user_version;'
}

write_version() { # write_version <db-in-volume> <value>
	local host
	host=$(vol_pull "$1")
	sq "$host" "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA user_version = $2;" >/dev/null
	vol_push "$host" "$1"
}

# ── Server lifecycle ────────────────────────────────────────────────────────

# Read the published port from podman rather than assuming one: the instance
# takes an auto-assigned host port.
published_port() {
	podman port "$CONTAINER" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true
}

wait_for_server() { # wait_for_server <seconds> -> prints the port, or fails
	local deadline=$((SECONDS + $1)) port
	while ((SECONDS < deadline)); do
		port=$(published_port)
		if [[ -n "$port" ]] && curl -sf -o /dev/null "http://localhost:${port}/"; then
			printf '%s' "$port"
			return 0
		fi
		sleep 2
	done
	return 1
}

echo "==> Schema-version container gate (instance '${INSTANCE}')"

# Anything an interrupted previous run of THIS checkout left behind.
bash "$REPO_DIR/deploy/teardown.sh" "$INSTANCE" --purge >/dev/null 2>&1 || true

# ── §1 Adoption ─────────────────────────────────────────────────────────────
# `--test` seeds the volume from tests/ui/fixtures, which are pre-gate files.
# They are forced to 0 anyway rather than trusted to still be 0: the fixtures
# get regenerated (`pkdump seed-fixture`) whenever the shared schema changes,
# and a regenerated fixture would come back already stamped — silently turning
# this section into a second fresh-install test, which is the exact shape of
# verification that took prod down.

echo ""
echo "==> Building the image and seeding an UNVERSIONED data volume..."
bash "$REPO_DIR/deploy/setup.sh" "$INSTANCE" --test >/dev/null

write_version "$CATALOG_DB" 0
write_version "$COLLECTION_DB" 0

echo ""
echo "--- §1 Adoption: an unversioned volume boots and serves ---"
check "catalog starts unversioned" "0" "$(read_version "$CATALOG_DB")"
check "collection starts unversioned" "0" "$(read_version "$COLLECTION_DB")"

systemctl --user start "$SERVICE_NAME"
PORT=$(wait_for_server 60 || true)
if [[ -z "$PORT" ]]; then
	echo "  FAIL  server did not come up against an unversioned volume"
	echo "        This is the release blocker — prod's databases are all 0."
	journalctl --user -u "$SERVICE_NAME" --no-pager -n 40 2>/dev/null || true
	exit 1
fi
echo "  PASS  server came up on port ${PORT}"
pass=$((pass + 1))

# Serving, not merely listening: a real API read, answered out of the adopted
# catalog.
check "GET /api/sets answers 200" "200" \
	"$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:${PORT}/api/sets")"

systemctl --user stop "$SERVICE_NAME"

CATALOG_V=$(read_version "$CATALOG_DB")
COLLECTION_V=$(read_version "$COLLECTION_DB")
check "catalog was stamped" "stamped" \
	"$([[ "$CATALOG_V" -gt 0 ]] && echo stamped || echo "still ${CATALOG_V}")"
check "collection was stamped" "stamped" \
	"$([[ "$COLLECTION_V" -gt 0 ]] && echo stamped || echo "still ${COLLECTION_V}")"
echo "        (catalog v${CATALOG_V}, collection v${COLLECTION_V})"

# ── §2 Refusal ──────────────────────────────────────────────────────────────
# The version this build understands is the one it just wrote, so "newer than
# the binary" is that + 1. No number is hardcoded, so this section cannot drift
# out of step with a future bump.

AHEAD=$((COLLECTION_V + 1))

echo ""
echo "--- §2 Refusal: a collection from the future stops the server ---"
write_version "$COLLECTION_DB" "$AHEAD"
check "collection now claims a future version" "$AHEAD" "$(read_version "$COLLECTION_DB")"

# The unit is Restart=on-failure, so the container keeps retrying behind this.
# `|| true` because `systemctl start` on a unit whose start job fails is itself
# an error — which is the expected outcome here.
systemctl --user start "$SERVICE_NAME" 2>/dev/null || true
if PORT=$(wait_for_server 20); then
	echo "  FAIL  server came up on port ${PORT} against a database from the future"
	echo "        A rollback onto this volume would silently corrupt it."
	fail=$((fail + 1))
else
	echo "  PASS  server refused to serve"
	pass=$((pass + 1))
fi

LOG=$(journalctl --user -u "$SERVICE_NAME" --no-pager -n 200 2>/dev/null || true)
contains "the refusal is logged" "refusing to open it" "$LOG"
contains "it names the file's version" "version ${AHEAD}" "$LOG"
contains "it names the binary's version" "version ${COLLECTION_V}" "$LOG"
contains "it names the database" "collection.sqlite" "$LOG"

systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true

# A gate that "fixed" the file it refused would defeat its own purpose: the
# operator's next move is to run the newer build, and it has to find its
# database as it left it.
check "the refused database is untouched" "$AHEAD" "$(read_version "$COLLECTION_DB")"

# ── §3 Reporting ────────────────────────────────────────────────────────────
# Deliberately run against the volume §2 left behind: `collection` is from the
# future and the server will not start on it. That is the state an operator is
# in when they reach for this command, so it is the state it is proved in.
#
# A second tenant is provisioned first so the listing has something to compare
# against — drift is only visible next to a database that is current, and one
# row cannot show a spread. Provisioning it also asserts something in passing:
# per-file versions mean a tenant this build refuses is not a box-wide outage.

echo ""
echo "--- §3 Reporting: tenant list names each tenant's own version ---"

# Both run through the shipped image against the real volume, the same one-off
# container `deploy/seed.sh` uses — no host binary, no host data dir.
pkdump_in_volume() {
	podman run --rm -v "${VOLUME}:/data:Z" -e PKDUMP_HOME=/data \
		--entrypoint pkdump "$IMAGE" "$@" 2>&1
}

CREATE_OUT=$(pkdump_in_volume tenant create drift-probe || true)
contains "a tenant is provisioned beside the refused one" "drift-probe" "$CREATE_OUT"
PROBE_V=$(read_version /data/tenants/drift-probe.sqlite)
check "the new tenant carries this build's version" "$COLLECTION_V" "$PROBE_V"

LIST_STATUS=0
LIST=$(pkdump_in_volume tenant list) || LIST_STATUS=$?
echo "$LIST" | sed 's/^/        /'

check "the report succeeds despite an unopenable tenant" "0" "$LIST_STATUS"

# Each row is `<name> <version> <status> …`, so the columns are read rather
# than the whole blob searched: a bare grep for the version number would pass
# on the OTHER tenant's row, which is the one mix-up this section exists to
# rule out.
row() { printf '%s\n' "$LIST" | awk -v n="$1" '$1 == n'; }

check "the refused tenant is listed with ITS version" "$AHEAD" \
	"$(row collection | awk '{print $2}')"
check "...and reported as ahead of this build" "ahead" \
	"$(row collection | awk '{print $3}')"
check "the current tenant is listed with ITS version" "$PROBE_V" \
	"$(row drift-probe | awk '{print $2}')"
check "...and reported as current" "current" \
	"$(row drift-probe | awk '{print $3}')"

# Asking what version a database is must not be a way of changing it. If the
# report opened tenants the way the app does, it would stamp and re-apply the
# schema to every tenant on the box as a side effect of being asked.
check "reporting left the refused tenant alone" "$AHEAD" "$(read_version "$COLLECTION_DB")"
check "reporting left the current tenant alone" "$PROBE_V" \
	"$(read_version /data/tenants/drift-probe.sqlite)"

# ── Result ──────────────────────────────────────────────────────────────────

echo ""
if ((fail > 0)); then
	echo "==> FAILED: ${fail} failed, ${pass} passed."
	exit 1
fi
echo "==> PASSED: ${pass} checks."
