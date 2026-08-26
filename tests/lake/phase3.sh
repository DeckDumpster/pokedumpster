#!/usr/bin/env bash
# Container-tier gate (pd-szh2, re-cut by pd-i08u): PHASE 3 — a collection
# valued from the TENANT ZONE, which since pd-i08u is the ONLY way it can be
# valued at all.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/phase3.sh          # ~5min, full run + teardown
#   KEEP=1 bash tests/lake/phase3.sh   # leave MinIO + Nessie + WORK up
#
# ── WHAT CHANGED, AND WHY THE CLAIM IS NOW A DIFFERENT SHAPE ────────────────
# pd-szh2 shipped the zone read ALONGSIDE the online one and proved them equal
# with `--compare` — 2 tenants, 7 rows, 0 differing. pd-i08u deleted the online
# read on the strength of that, so there is no second computation here to diff
# against and `--compare` does not exist. The claim this file makes instead is
# the one that survives having only one path:
#
#   §5  the zone valuation reproduces, byte for byte, the rows Rust
#       `value_history::snapshot_today` computed over the same collection —
#       the SAME expected TSV tests/lake/value_snapshots.sh diffs against. A
#       lossy round trip through Parquet, encryption and `outbox::project`
#       shows up here as a changed number.
#   §5b the online path is GONE rather than merely defaulted away: no
#       `--holdings`, no `--compare`. A flag left behind is a flag a runbook
#       or a timer can still reach for.
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# The failure this item could quietly become is a "Phase 3" that reads the live
# table and calls it the zone. Under `--compare` that showed up as two
# computations agreeing when they should not have; with one computation the
# equivalent is §6, and it is stronger for being self-contained:
#
#   change a collection WITHOUT shipping it, and the valuation must NOT MOVE.
#   Then ship and read back, and it MUST.
#
# A valuation that tracks the live table fails the first half; one that is
# frozen or cached fails the second. Only a real zone read passes both.
#
# The fixture is value_snapshot_fixture.rs — the one the transform's own gate
# uses — which carries a sold copy, a NULL purchase price, a manual price, a
# condition multiplier and two binders. Every arm of the price rule.
#
# And the other quiet failure — a Phase 3 that runs on a stale materialisation
# and reports beautiful numbers about last week's holdings — is §7: ship
# without re-reading, and the transform must REFUSE.
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

# The staging table the zone is read into. Named in Rust
# (`pkdump_db::outbox::ZONE_HOLDINGS_TABLE`, `pkdump_ship::zone`) and in Python
# (`pkdump_lake.value_snapshots.ZONE_HOLDINGS`); this gate is the fourth place
# and the one that holds them together.
ZONE_TABLE=zone_holdings
# The sealed half of the same read-back (pd-bbv7), named in the same three
# places plus this one.
ZONE_SEALED_TABLE=zone_sealed_holdings

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

# A lake (Python) job: prices out of Iceberg, and the transform itself.
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

# One tenant's snapshot rows for one date, in the SAME text form
# `value_snapshot_fixture.rs::dump_snapshot` writes — same columns, same NULL
# placeholder, same ordering, same ten decimal places. Identical to the helper
# in tests/lake/value_snapshots.sh, and deliberately so: §5 diffs against the
# very file that gate diffs against, so "the zone valuation" and "the online
# valuation Rust computed" are held to one expected output rather than two.
dump() { # dump <database_id> <date>
	run_job python -c "
import sqlite3
conn = sqlite3.connect('/fixture/home/tenants/${1}.sqlite')
for dim, bucket, mv, cb, cc in conn.execute('''
    SELECT dimension, bucket, market_value, cost_basis, card_count
      FROM collection_value_snapshot WHERE date = ? ORDER BY dimension, bucket''', ('${2}',)):
    print(dim, bucket if bucket is not None else '-', f'{mv:.10f}', f'{cb:.10f}', cc, sep='\t')
"
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
log "3. the valuation refuses before the zone has been read"
# The failure this refusal replaces is the one nothing else would catch: a
# valuation computed from a table that is not there, or worse, from one left
# over from last week. Since pd-i08u there is no other source to fall back to,
# which makes this refusal the whole of the job's behaviour on a box where the
# read-back has never run — so it has to name the command rather than merely
# fail.
OUT=$(snapshot --date "$DATE_NEW" 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
# The exit code alone cannot tell a refusal from an argparse error — argparse
# exits 2, which is also EXIT_PARTIAL — so the run is checked to have reached
# the lake first. This gate learned that the hard way.
check "the run got as far as opening the lake" "yes" \
	"$(grep -q 'active tenant(s)' <<<"$OUT" && echo yes || echo no)"
check "a run with no materialisation exits 1 (nobody valued)" "1" "$RC"
check "…and names the command that fixes it" "yes" \
	"$(grep -q 'pkdump-ship holdings' <<<"$OUT" && echo yes || echo no)"

# …and there is no way to ask for the old behaviour instead. A deleted path
# that leaves its flag behind is a path a runbook can still reach for, and the
# argument for deleting it was that one valuation should have one provenance.
#
# Checked on argparse's own "unrecognized arguments", not on the exit status:
# an unknown flag exits 2 and a legitimate run without a materialisation exits
# 1, so a status-only check here would read "the flag was rejected" off a run
# that failed for the ordinary reason.
#
# The output is captured before it is grepped rather than piped into grep:
# `set -o pipefail` is on, argparse exits 2, and a pipeline's status under
# pipefail is the job's rather than grep's — so the piped form reported
# "present" for a flag that had in fact been rejected.
rejects_flag() { # rejects_flag <flag…>
	local out
	out=$(snapshot --date "$DATE_NEW" "$@" 2>&1) || true
	case "$out" in
	*"unrecognized arguments"*) echo gone ;;
	*) echo present ;;
	esac
}
check "the online holdings read is gone, not defaulted away" "gone" \
	"$(rejects_flag --holdings collection)"
check "…nor reachable under its own name" "gone" "$(rejects_flag --holdings zone)"
check "…and so is the comparison that justified deleting it" "gone" \
	"$(rejects_flag --compare)"

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
	"$(tenant_exec "$ALICE" "SELECT count(*) FROM ${ZONE_TABLE}")"
# Both halves of the holdings make the round trip (pd-bbv7). A zero here would
# be indistinguishable from a tenant who owns no sealed product, which is why
# the fixture gives alice some.
check "…and her sealed lots too" \
	"$(tenant_exec "$ALICE" 'SELECT count(*) FROM sealed_collection')" \
	"$(tenant_exec "$ALICE" "SELECT count(*) FROM ${ZONE_SEALED_TABLE}")"
check "…and there really are some" "yes" \
	"$([ "$(tenant_exec "$ALICE" "SELECT count(*) FROM ${ZONE_SEALED_TABLE}")" -gt 0 ] &&
		echo yes || echo no)"

# ---------------------------------------------------------------------------
log "5. THE ACCEPTANCE BAR: the zone valuation IS the valuation, row for row"
# pd-szh2 proved this by running both paths and diffing them. With the online
# path deleted there is nothing to diff against at runtime, so the comparison
# moves to the artefact the fixture generator leaves behind: the rows Rust
# `value_history::snapshot_today` computed over alice's collection, in
# expected-alice-<date>.tsv — the same file tests/lake/value_snapshots.sh
# diffs against.
#
# That makes this a stronger claim than "two runs agree", not a weaker one:
# alice's holdings have been through `outbox emit`, an encrypted Parquet part
# in a real bucket, `pkdump-ship holdings` and `outbox::project` since Rust
# computed those numbers, and every one of those steps is somewhere a copy
# could be lost, duplicated or resolved to the wrong version.
OUT=$(snapshot --date "$DATE_NEW" 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
# ghost is registered with no database: skipped, so the run is partial (2)
# rather than clean, and a skip must never read as success.
check "a real run exits 2 for the tenant it could not value, not 0" "2" "$RC"
check "…and both tenants that have a database were valued" "yes" \
	"$(grep -q '2 tenant(s) snapshotted' <<<"$OUT" && echo yes || echo no)"
check "…out of ${ZONE_TABLE}, which is the only place it can read" "yes" \
	"$(grep -q "in ${ZONE_TABLE}" <<<"$OUT" && echo yes || echo no)"

diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "the zone valuation differs from value_history::snapshot_today over the same collection — the round trip through the zone is not lossless"
ROWS=$(wc -l <"$FIXTURE/expected-alice-${DATE_NEW}.tsv")
[ "${ROWS:-0}" -gt 0 ] || die "the expected fixture is empty — §5 would compare nothing and call it agreement"
echo "  alice: ${ROWS} snapshot row(s) byte-identical to snapshot_today, through the zone"

# The bead, inverted (pd-s5yn): bob is a second tenant with his own holdings,
# and his total must be his own rather than a second copy of alice's.
BOB_VALUE=$(tenant_exec "$BOB" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'all' AND date = '${DATE_NEW}'")
ALICE_VALUE=$(tenant_exec "$ALICE" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'all' AND date = '${DATE_NEW}'")
[ -n "$BOB_VALUE" ] && [ "$BOB_VALUE" != "$ALICE_VALUE" ] ||
	die "bob's total is '${BOB_VALUE}' against alice's '${ALICE_VALUE}' — one zone partition was valued twice"
echo "  bob ${BOB_VALUE}, alice ${ALICE_VALUE} — two partitions, two answers"

# ---------------------------------------------------------------------------
log "6. SEEN RED: a collection change that never shipped must NOT move the value"
# The one thing that distinguishes "valued from the zone" from "valued from the
# collection while claiming otherwise". Under pd-szh2 this was two computations
# being required to DISAGREE; with one computation it is the same claim made
# directly, and it cannot be satisfied by accident — a valuation that tracks
# the live table fails this half, and one that is merely frozen or cached fails
# §6b. If both halves pass green, the whole item is unfounded.
ALICE_SEALED=$(tenant_exec "$ALICE" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'sealed' AND date = '${DATE_NEW}'")
tenant_exec "$ALICE" "DELETE FROM collection WHERE status = 'owned' AND id = (
    SELECT min(id) FROM collection WHERE status = 'owned')" >/dev/null
# Both halves changed, so both halves have to fail to notice. A sealed
# dimension wired to `sealed_collection` instead of to the zone would pass
# every card assertion in this file and be exactly the bug §6 exists to catch.
tenant_exec "$ALICE" "DELETE FROM sealed_collection WHERE status = 'owned' AND id = (
    SELECT min(id) FROM sealed_collection WHERE status = 'owned')" >/dev/null
LIVE_COPIES=$(tenant_exec "$ALICE" "SELECT count(*) FROM collection WHERE status = 'owned'")
ZONE_COPIES=$(tenant_exec "$ALICE" "SELECT count(*) FROM ${ZONE_TABLE} WHERE status = 'owned'")
check "the live collection really did change" "yes" \
	"$([ "$LIVE_COPIES" -lt "$ZONE_COPIES" ] && echo yes || echo no)"
LIVE_LOTS=$(tenant_exec "$ALICE" "SELECT count(*) FROM sealed_collection WHERE status = 'owned'")
ZONE_LOTS=$(tenant_exec "$ALICE" "SELECT count(*) FROM ${ZONE_SEALED_TABLE} WHERE status = 'owned'")
check "…and so did the live sealed collection" "yes" \
	"$([ "$LIVE_LOTS" -lt "$ZONE_LOTS" ] && echo yes || echo no)"

expect_rc 2 "${WORK}/unshipped.log" snapshot --date "$DATE_NEW"
diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "a collection change that was never shipped moved the valuation — this is a Phase 3 reading the live table"
echo "  ok   the card left the collection and the valuation did not notice"

log "6b. …and shipping the change is what DOES move it"
expect_rc 2 "${WORK}/ship2.log" ship_job run --data-dir /data
expect_rc 2 "${WORK}/holdings2.log" ship_job holdings --data-dir /data
expect_rc 2 "${WORK}/shipped.log" snapshot --date "$DATE_NEW"
if diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" >/dev/null 2>&1; then
	die "the valuation is UNCHANGED after shipping the removal — it is frozen or cached, not read from the zone"
fi
AFTER=$(tenant_exec "$ALICE" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'all' AND date = '${DATE_NEW}'")
check "…and it moved DOWN, a card having been removed" "yes" \
	"$(awk -v a="$AFTER" -v b="$ALICE_VALUE" 'BEGIN { print (a < b) ? "yes" : "no" }')"
echo "  ${ALICE_VALUE} -> ${AFTER} once the zone was told"
AFTER_SEALED=$(tenant_exec "$ALICE" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'sealed' AND date = '${DATE_NEW}'")
check "…and so did the sealed half, a lot having been removed" "yes" \
	"$(awk -v a="$AFTER_SEALED" -v b="$ALICE_SEALED" 'BEGIN { print (a < b) ? "yes" : "no" }')"
echo "  sealed ${ALICE_SEALED} -> ${AFTER_SEALED}"

# ---------------------------------------------------------------------------
log "7. a STALE materialisation is refused, not valued"
# The quiet failure this item could become: the zone moves on, nobody re-reads
# it, and Phase 3 goes on producing beautiful numbers about older holdings.
tenant_exec "$BOB" "DELETE FROM collection WHERE status = 'owned' AND id = (
    SELECT min(id) FROM collection WHERE status = 'owned')" >/dev/null
expect_rc 2 "${WORK}/ship3.log" ship_job run --data-dir /data
# …and deliberately NO `holdings` run here.
OUT=$(snapshot --date "$DATE_NEW" 2>&1) && RC=0 || RC=$?
echo "$OUT" | sed 's/^/  | /'
check "the run got as far as opening the lake" "yes" \
	"$(grep -q 'active tenant(s)' <<<"$OUT" && echo yes || echo no)"
check "a run over a stale staging table is partial, not clean" "2" "$RC"
check "…and says the materialisation predates the last ship" "yes" \
	"$(grep -q 'predates the last ship' <<<"$OUT" && echo yes || echo no)"
check "…naming the tenant it refused" "yes" \
	"$(grep -q "$BOB" <<<"$OUT" && echo yes || echo no)"

log "8. the zone path writes the same table the app reads"
expect_rc 2 "${WORK}/holdings4.log" ship_job holdings --data-dir /data
snapshot --date "$DATE_NEW" >"${WORK}/zone-run.log" 2>&1 && RC=0 || RC=$?
sed 's/^/  | /' "${WORK}/zone-run.log"
check "the run valued somebody out of ${ZONE_TABLE}" "yes" \
	"$(grep -q "in ${ZONE_TABLE}" "${WORK}/zone-run.log" && echo yes || echo no)"
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
echo "==> Phase 3 values a collection from the tenant zone, and there is no"
echo "    longer any other way to value one. The numbers are the ones the"
echo "    online path produced, byte for byte — and they STOP moving the"
echo "    moment the zone stops being told."
