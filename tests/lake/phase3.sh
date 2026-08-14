#!/usr/bin/env bash
# Container-tier gate (pd-szh2): PHASE 3 — a collection valued from the TENANT
# ZONE, proven row-for-row identical to the online computation it ships beside.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/phase3.sh          # ~5min, full run + teardown
#   KEEP=1 bash tests/lake/phase3.sh   # leave MinIO + Nessie + WORK up
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# The bead's acceptance bar is one sentence: "Phase 3's offline output must
# match the CURRENT online computation exactly, for every tenant compared."
# Three ways to satisfy that sentence and prove nothing:
#
#   * value both paths from `collection` and diff them. They agree because
#     they are the same computation. §5 forbids it: §6 changes a collection
#     WITHOUT shipping, and the two must then DISAGREE in a named way. A
#     valuation that still agrees is reading the live table.
#   * compare on a fixture with nothing awkward in it. The fixture here is
#     value_snapshot_fixture.rs — the one the transform's own gate uses —
#     which carries a sold copy, a NULL purchase price, a manual price, a
#     condition multiplier and two binders. Every arm of the price rule.
#   * compare one tenant. §5 compares every registered tenant, and the run
#     says how many, over how many rows.
#
# And the failure this whole item could quietly become — a Phase 3 that runs
# on a stale materialisation and reports beautiful numbers about last week's
# holdings — is §7: ship without re-reading, and the transform must REFUSE.
#
# The network is `--internal`, as in tests/lake/value_snapshots.sh: every job
# here is offline infrastructure and none of them may reach an upstream.
#
# ── NO REAL TENANT DATA ─────────────────────────────────────────────────────
# The fixture is built from scratch in a temp dir on every run and holds
# invented printings in invented sets. The tenant zone is the SUBJECT of this
# design, so its fixtures are treated as if they were real.
#
# Prod-safe: its own podman network, MinIO, Nessie, bucket, temp dir, image
# tags and master key. Touches no pkdump-* unit, no pkdump-*-data volume, no
# real bucket, no real tenant database and never the operator's key.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}
NESSIE_IMAGE=${NESSIE_IMAGE:-ghcr.io/projectnessie/nessie:0.104.3}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
pkdump_store_load_config
pkdump_store_activate

die() {
	diag "!! $*"
	exit 1
}
log() { printf '\n=== %s ===\n' "$*"; }

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

# Unique per checkout: deploy/ci.sh runs gates in parallel and several
# polecats run whole suites beside each other from their own worktrees.
SUFFIX="${PDPHASE3_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdphase3-net-${SUFFIX}
MINIO_CTR=pdphase3-minio-${SUFFIX}
NESSIE_CTR=pdphase3-nessie-${SUFFIX}
JOB_IMAGE=pkdump-lake:phase3-${SUFFIX}
APP_IMAGE=localhost/pkdump:phase3-${SUFFIX}
BUCKET=pdphase3-${SUFFIX}
AKID=pdphase3root
SECRET=pdphase3rootsecret123

WAREHOUSE="s3://${BUCKET}/lake"

WORK=${WORK:-$(mktemp -d /tmp/pdphase3.XXXXXX)}
FIXTURE="$WORK/fixture"
KEYS="$WORK/keys"
mkdir -p "$WORK/nessie" "$WORK/minio" "$FIXTURE" "$KEYS"
chmod 777 "$WORK/nessie" "$WORK/minio"

# Kept in step with crates/pkdump-ingest/tests/value_snapshot_fixture.rs.
DATE_NEW=2026-08-10

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	if [ -n "${KEEP:-}" ]; then
		echo ""
		echo "KEEP=1 — leaving everything up:"
		echo "  network ${NET} (internal: reach it with podman run --network ${NET})"
		echo "  work    ${WORK}   (fixture ${FIXTURE}, key ${KEYS}/tenant-master.key)"
		echo "  tear down: podman rm -f ${MINIO_CTR} ${NESSIE_CTR}; podman network rm ${NET}"
		return
	fi
	podman rm -f "$MINIO_CTR" "$NESSIE_CTR" >/dev/null 2>&1 || true
	podman network rm "$NET" >/dev/null 2>&1 || true
	podman rmi -f "$JOB_IMAGE" "$APP_IMAGE" >/dev/null 2>&1 || true
	# Nessie writes its version store as a SUBUID the invoking user cannot
	# unlink; a plain rm -rf leaves the whole work dir behind, every run.
	podman unshare rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

mc() {
	podman run --rm --network "$NET" \
		-e MC_HOST_m="http://${AKID}:${SECRET}@${MINIO_CTR}:9000" \
		-v "$FIXTURE:/fixture:ro,Z" "$MC_IMAGE" "$@"
}

# A lake (Python) job: prices out of Iceberg, the transform, the comparison.
# The data directory is read-WRITE — the transform's output is a write into
# each tenant's database, and these are WAL files besides.
run_job() {
	podman run --rm --network "$NET" \
		-v "$FIXTURE:/fixture:Z" \
		-e PKDUMP_LAKE_NESSIE_URI="http://${NESSIE_CTR}:19120/iceberg/" \
		-e PKDUMP_LAKE_S3_BUCKET="$BUCKET" \
		-e PKDUMP_LAKE_S3_REGION=us-west-2 \
		-e PKDUMP_LAKE_S3_ENDPOINT="http://${MINIO_CTR}:9000" \
		-e PKDUMP_LAKE_S3_ACCESS_KEY_ID="$AKID" \
		-e PKDUMP_LAKE_S3_SECRET_ACCESS_KEY="$SECRET" \
		-e PKDUMP_LAKE_S3_PATH_STYLE=1 \
		"$JOB_IMAGE" "$@"
}

snapshot() { run_job pkdump-lake-value-snapshots --data-dir /fixture/home "$@"; }

# The shipper and the zone reader, out of the SHIPPED image — the tenant
# profile and the master key, and nothing the app container ever holds.
ship_job() {
	podman run --rm --network "$NET" --pull=never \
		-v "${FIXTURE}/home:/data:Z" \
		-v "${KEYS}/tenant-master.key:/keys/tenant-master.key:ro,Z" \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		-e PKDUMP_LAKE_S3_BUCKET="$BUCKET" \
		-e PKDUMP_LAKE_S3_REGION=us-west-2 \
		-e PKDUMP_LAKE_S3_ENDPOINT="http://${MINIO_CTR}:9000" \
		-e PKDUMP_TENANT_AWS_PROFILE=pkdump-tenant-test \
		-e AWS_ACCESS_KEY_ID="$AKID" -e AWS_SECRET_ACCESS_KEY="$SECRET" \
		--entrypoint pkdump-ship "$APP_IMAGE" "$@"
}

# Run a job and require one of the exit statuses given, printing its output if
# it produced any other. `ghost` is registered with no database, so EVERY job
# here legitimately exits 2 — and a helper that accepted any non-zero would
# have hidden the one thing several sections below are asserting.
expect_rc() { # expect_rc <ok-codes,…> <logfile> <cmd…>
	local ok="$1" logfile="$2"
	shift 2
	local rc=0
	"$@" >"$logfile" 2>&1 || rc=$?
	case ",${ok}," in
	*",${rc},"*) return 0 ;;
	esac
	cat "$logfile"
	die "$1 exited ${rc}; expected one of ${ok}"
}

# `pkdump` itself, for provisioning. No bucket: this is the ONLINE binary.
pkdump_cli() {
	podman run --rm --pull=never \
		-v "${FIXTURE}/home:/data:Z" \
		-v "${KEYS}:/keys:rw,Z" \
		-e PKDUMP_HOME=/data \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		--entrypoint pkdump "$APP_IMAGE" "$@"
}

# Arbitrary SQL against one tenant's database, through the lake image (which
# has a python3 and is already on the network). The SQL travels as an
# ARGUMENT, never interpolated into the source.
tenant_exec() { # tenant_exec <database_id> <sql>
	run_job python -c "
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1], isolation_level=None)
cur = conn.execute(sys.argv[2])
row = cur.fetchone()
print(row[0] if row else '')
" "/fixture/home/tenants/${1}.sqlite" "$2" | tail -1
}

# ---------------------------------------------------------------------------
log "0. the fixture: a catalog, a registry, two collections, landed prices"
PKDUMP_VALUE_FIXTURE_OUT="$FIXTURE" \
	cargo test --manifest-path "${REPO_DIR}/Cargo.toml" -p pkdump-ingest --test value_snapshot_fixture ||
	die "the fixture generator failed — see crates/pkdump-ingest/tests/value_snapshot_fixture.rs"
[ -f "$FIXTURE/home/shared.sqlite" ] || die "the fixture produced no shared.sqlite"

ALICE=$(awk -F'\t' '$1 == "alice" {print $2}' "$FIXTURE/tenants.tsv")
BOB=$(awk -F'\t' '$1 == "bob" {print $2}' "$FIXTURE/tenants.tsv")
GHOST=$(awk -F'\t' '$1 == "ghost" {print $2}' "$FIXTURE/tenants.tsv")
[ -n "$ALICE" ] && [ -n "$BOB" ] && [ -n "$GHOST" ] || die "the fixture wrote no tenant map"
echo "  alice=${ALICE} bob=${BOB} ghost=${GHOST} (no database, on purpose)"

log "1. both images"
podman build -t "$JOB_IMAGE" -f "${REPO_DIR}/lake/Containerfile" "${REPO_DIR}/lake" >/dev/null ||
	die "the lake image build failed"
pkdump_image_ensure "$APP_IMAGE" "$REPO_DIR" >"${WORK}/build.log" 2>&1 || {
	tail -40 "${WORK}/build.log"
	die "the app image build failed"
}
# Not through ship_job: that mounts the master key, which does not exist yet,
# and a bind mount of a missing path creates a DIRECTORY where §4 needs a file.
check "the image carries the zone reader" "ok" \
	"$(podman run --rm --pull=never --entrypoint pkdump-ship "$APP_IMAGE" holdings --help \
		>/dev/null 2>&1 && echo ok || echo missing)"

log "2. an INTERNAL network, MinIO and Nessie"
podman network create --internal "$NET" >/dev/null
EGRESS=$(podman run --rm --network "$NET" "$JOB_IMAGE" python -c "
import socket
try:
    socket.create_connection(('1.1.1.1', 443), timeout=5)
    print('REACHABLE')
except OSError:
    print('blocked')
")
[ "$EGRESS" = "blocked" ] ||
	die "the job container can reach the internet (${EGRESS}) — an offline Phase 3 proves nothing"

podman run -d --name "$MINIO_CTR" --network "$NET" \
	-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET" \
	-v "$WORK/minio:/data:Z" \
	"$MINIO_IMAGE" server /data >/dev/null
minio_up() { mc ls m >/dev/null 2>&1; }
wait_until 30 1 minio_up || die "MinIO never became reachable on ${NET}"
mc mb "m/${BUCKET}" >/dev/null || die "could not create the bucket"
mc mirror --quiet /fixture/raw-zone "m/${BUCKET}" >/dev/null || die "could not mirror raw/"
echo "  MinIO up, bucket ${BUCKET} holds $(mc ls --recursive "m/${BUCKET}" | grep -c .) landed object(s)"

podman run -d --name "$NESSIE_CTR" --network "$NET" \
	--memory=1g --memory-swap=1g \
	-v "$WORK/nessie:/nessie:Z" \
	-e nessie.version.store.type=ROCKSDB \
	-e nessie.version.store.persist.rocks.database-path=/nessie/rocksdb \
	-e nessie.version.store.persist.cache-capacity-mb=64 \
	-e nessie.catalog.default-warehouse=lake \
	-e nessie.catalog.warehouses.lake.location="$WAREHOUSE" \
	-e nessie.catalog.service.s3.default-options.region=us-west-2 \
	-e nessie.catalog.service.s3.default-options.endpoint="http://${MINIO_CTR}:9000" \
	-e nessie.catalog.service.s3.default-options.path-style-access=true \
	-e nessie.catalog.service.s3.default-options.access-key=urn:nessie-secret:quarkus:nessie.catalog.secrets.access-key \
	-e nessie.catalog.secrets.access-key.name="$AKID" \
	-e nessie.catalog.secrets.access-key.secret="$SECRET" \
	"$NESSIE_IMAGE" >/dev/null
podman run --rm --network "$NET" "$JOB_IMAGE" python -c "
import sys, time, urllib.request
for _ in range(90):
    try:
        urllib.request.urlopen('http://${NESSIE_CTR}:19120/api/v2/config', timeout=3)
        sys.exit(0)
    except Exception:
        time.sleep(1)
sys.exit(1)
" || {
	podman logs "$NESSIE_CTR" 2>&1 | tail -40
	die "Nessie never answered on ${NET}"
}
echo "  Nessie up, warehouse ${WAREHOUSE}"
run_job pkdump-lake-build-prices --ingest-date "$DATE_NEW" >"${WORK}/prices.log" 2>&1 || {
	tail -30 "${WORK}/prices.log"
	podman logs "$NESSIE_CTR" 2>&1 | tail -30
	die "the ${DATE_NEW} price build failed"
}
echo "  catalog.prices for ${DATE_NEW} is in the lake"

# ---------------------------------------------------------------------------
log "3. Phase 3 refuses before the zone has been read"
# The failure this refusal replaces is the one nothing else would catch: a
# valuation computed from a table that is not there, or worse, from one left
# over from last week.
OUT=$(snapshot --date "$DATE_NEW" --holdings zone 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
# `--holdings zone` must be a source this job HAS. argparse rejects an unknown
# choice with exit 2, which is also EXIT_PARTIAL — so the exit code alone
# cannot tell a refusal from a typo, and this gate learned that the hard way.
check "the run got as far as opening the lake" "yes" \
	"$(grep -q 'active tenant(s)' <<<"$OUT" && echo yes || echo no)"
check "a zone run with no materialisation exits 1 (nobody valued)" "1" "$RC"
check "…and names the command that fixes it" "yes" \
	"$(grep -q 'pkdump-ship holdings' <<<"$OUT" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "4. keys, an outbox that describes the collections, and a shipped zone"
pkdump_cli keys init >/dev/null || die "keys init failed"
pkdump_cli keys register "$ALICE" >/dev/null || die "keys register alice failed"
pkdump_cli keys register "$BOB" >/dev/null || die "keys register bob failed"

# The backfill. These collections were built before anything shipped, which is
# the shape of every existing box (pd-whsw): the triggers only record what
# changes AFTER them, so without this the zone would be empty and correct.
# 2, not 0: ghost is registered and has no database, and every job here says so.
expect_rc 2 "${WORK}/emit.log" pkdump_cli outbox emit --all --all-tenants
expect_rc 2 "${WORK}/ship.log" ship_job run --data-dir /data
ZONE_OBJECTS=$(mc ls --recursive "m/${BUCKET}" 2>/dev/null | awk '{print $NF}' | grep -c '^tenant/' || true)
check "the zone holds objects for both tenants" "2" \
	"$(mc ls --recursive "m/${BUCKET}" 2>/dev/null | awk '{print $NF}' |
		grep '^tenant/' | sed 's#.*database_id=\([^/]*\)/.*#\1#' | sort -u | grep -c .)"
echo "  ${ZONE_OBJECTS} object(s) under tenant/"

expect_rc 2 "${WORK}/holdings.log" ship_job holdings --data-dir /data
sed 's/^/  | /' "${WORK}/holdings.log"
check "alice's staged holdings are her collection, row for row" \
	"$(tenant_exec "$ALICE" 'SELECT count(*) FROM collection')" \
	"$(tenant_exec "$ALICE" 'SELECT count(*) FROM zone_holdings')"

# ---------------------------------------------------------------------------
log "5. THE ACCEPTANCE BAR: every tenant, both ways, row for row"
OUT=$(snapshot --date "$DATE_NEW" --compare 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
# ghost is registered with no database: compared-nobody, so the run is partial
# (2) rather than clean. The claim being made is about the tenants that WERE
# compared, and the run must not let a skip read as agreement.
check "the comparison ran and found no difference" "no-diff" \
	"$(grep -q '0 tenant(s) DIFFERING' <<<"$OUT" && echo no-diff || echo differing)"
check "…for both tenants that have a database" "yes" \
	"$(grep -q '2 tenant(s) compared' <<<"$OUT" && echo yes || echo no)"
check "…and it exits 2 for the tenant it could not compare, not 0" "2" "$RC"
ROWS=$(sed -n 's/.*compared, \([0-9]*\) row(s).*/\1/p' <<<"$OUT" | tail -1)
[ "${ROWS:-0}" -gt 0 ] || die "the comparison compared zero rows and called it agreement"
echo "  ${ROWS} snapshot row(s) compared across 2 tenants, 0 differing"

# Measured on the PROVENANCE table, not on the snapshot rows: the fixture
# deliberately leaves alice's four DATE_NEW rows behind (they are what
# tests/lake/value_snapshots.sh diffs against), and leaves the provenance table
# empty. So a row here means a run committed, and there is nothing to subtract.
check "a comparison writes NOTHING" "0" \
	"$(tenant_exec "$ALICE" 'SELECT count(*) FROM collection_value_snapshot_run')"

# ---------------------------------------------------------------------------
log "6. SEEN RED: a collection change that never shipped must make them differ"
# The one thing that distinguishes "valued from the zone" from "valued from
# the collection while claiming otherwise". If this section passes green, the
# whole item is unfounded.
tenant_exec "$ALICE" "DELETE FROM collection WHERE status = 'owned' AND id = (
    SELECT min(id) FROM collection WHERE status = 'owned')" >/dev/null
OUT=$(snapshot --date "$DATE_NEW" --compare 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
check "the two valuations now DISAGREE" "4" "$RC"
check "…and the run names the tenant" "yes" \
	"$(grep -q "$ALICE" <<<"$OUT" && echo yes || echo no)"
check "…and the dimension that moved" "yes" \
	"$(grep -qE 'all: online .* != zone' <<<"$OUT" && echo yes || echo no)"

log "6b. …and shipping the change is what makes them agree again"
expect_rc 2 "${WORK}/ship2.log" ship_job run --data-dir /data
expect_rc 2 "${WORK}/holdings2.log" ship_job holdings --data-dir /data
OUT=$(snapshot --date "$DATE_NEW" --compare 2>&1) && RC=0 || RC=$?
check "identical again once the zone has been told" "no-diff" \
	"$(grep -q '0 tenant(s) DIFFERING' <<<"$OUT" && echo no-diff || echo differing)"
check "…still exiting 2 for ghost alone" "2" "$RC"

# ---------------------------------------------------------------------------
log "7. a STALE materialisation is refused, not valued"
# The quiet failure this item could become: the zone moves on, nobody re-reads
# it, and Phase 3 goes on producing beautiful numbers about older holdings.
tenant_exec "$BOB" "DELETE FROM collection WHERE status = 'owned' AND id = (
    SELECT min(id) FROM collection WHERE status = 'owned')" >/dev/null
expect_rc 2 "${WORK}/ship3.log" ship_job run --data-dir /data
# …and deliberately NO `holdings` run here.
OUT=$(snapshot --date "$DATE_NEW" --holdings zone 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
check "the run got as far as opening the lake" "yes" \
	"$(grep -q 'active tenant(s)' <<<"$OUT" && echo yes || echo no)"
check "a zone run over a stale staging table is partial, not clean" "2" "$RC"
check "…and says the materialisation predates the last ship" "yes" \
	"$(grep -q 'predates the last ship' <<<"$OUT" && echo yes || echo no)"
check "…naming the tenant it refused" "yes" \
	"$(grep -q "$BOB" <<<"$OUT" && echo yes || echo no)"

log "8. the zone path writes the same table the app reads"
expect_rc 2 "${WORK}/holdings4.log" ship_job holdings --data-dir /data
snapshot --date "$DATE_NEW" --holdings zone >"${WORK}/zone-run.log" 2>&1 && RC=0 || RC=$?
sed 's/^/  | /' "${WORK}/zone-run.log"
check "the run valued somebody out of zone_holdings" "yes" \
	"$(grep -q 'in zone_holdings' "${WORK}/zone-run.log" && echo yes || echo no)"
check "a real Phase 3 run exits 2 (ghost) having written the rest" "2" "$RC"
ZONE_ROWS=$(tenant_exec "$ALICE" "SELECT count(*) FROM collection_value_snapshot WHERE date = '${DATE_NEW}'")
[ "${ZONE_ROWS:-0}" -gt 0 ] || die "the zone path wrote no snapshot rows"
check "…and recorded which catalog it read" "pinned" \
	"$(tenant_exec "$ALICE" "SELECT lake_ref FROM collection_value_snapshot_run WHERE date = '${DATE_NEW}'" |
		grep -q '@' && echo pinned || echo unpinned)"

log "9. the catalog zone is untouched by any of it"
# Phase 3 reads the tenant zone and catalog.prices. Nothing it does may leave
# a tenant-shaped object outside tenant/.
check "no tenant-keyed object outside tenant/" "0" \
	"$(mc ls --recursive "m/${BUCKET}" 2>/dev/null | awk '{print $NF}' |
		grep -v '^tenant/' | grep -c 'database_id=' || true)"

# ---------------------------------------------------------------------------
echo ""
echo "==> ${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ] || exit 1
echo "==> Phase 3 values a collection from the tenant zone, and it is the same"
echo "    valuation the online path produces — proven for every tenant, and"
echo "    proven to STOP being the same when the zone stops being told."
