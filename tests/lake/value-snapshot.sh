#!/usr/bin/env bash
# Container-tier gate (pd-ruwh): the transform tier publishes to EVERY tenant.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/value-snapshot.sh          # ~6min, full run + teardown
#   KEEP=1 bash tests/lake/value-snapshot.sh   # leave MinIO + Nessie + WORK up
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# `pd-s5yn`, verified: `pkdump data refresh` step 7 snapshots ONE tenant —
# whichever `$PKDUMP_USER` names inside the container — because there is no
# loop. Every other tenant gets no value history, ever, with no error. It is
# latent only because prod has one tenant, which is exactly the kind of defect
# that surfaces on the day a second one exists.
#
# So §4 registers TWO tenants and requires both to be snapshotted, with
# DIFFERENT numbers: a loop that wrote one tenant's totals into everybody's
# database would pass "both have rows" and fail here.
#
# The other half is trust. A rewrite of a number the user looks at has to be
# observably a no-op BEFORE it is trusted with anything new, so §3 runs the old
# path (`pkdump data snapshot-value`, which is step 7's own two lines) and the
# new one over the same fixture and requires the rows to be byte-identical.
# Only then do the new behaviours get asserted:
#
#   * §5 a tenant whose database is gone is logged, SKIPPED, and named — the
#     run still finishes everyone else and says so in its exit status. Silent
#     partial success is the failure mode of the loop this design replaces, so
#     "it kept going" is not enough on its own; the status has to carry it.
#   * §6 re-running produces identical rows (the publish contract's idempotence).
#   * §7 pointing the job at an OLDER date reconstructs that date's values from
#     that date's prices — backfill, which the design says falls out for free
#     and which therefore has to be shown rather than asserted in prose.
#   * §8 no tenant data is in the lake, and no lake table carries a
#     tenant-identifying column. A rule nobody can grep for is a rule that
#     erodes, so it is checked mechanically.
#
# The network is `--internal`, as in tests/lake/prices.sh: this tier never
# calls an upstream either.
#
# Prod-safe: its own podman network, MinIO, Nessie, temp dir, image tag and
# $PKDUMP_HOME. Touches no pkdump-* unit, no pkdump-*-data volume, no real S3
# bucket, and no real tenant database.
set -euo pipefail

NESSIE_IMAGE=${NESSIE_IMAGE:-ghcr.io/projectnessie/nessie:0.104.3}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Failure diagnostics (pd-8gjs): name the failing file, line, command and status
# before the EXIT trap's teardown chatter.
# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

die() {
	diag "!! $*"
	exit 1
}

# Non-prod container storage belongs on the data disk, not the disk prod runs
# from — the same resolution deploy/ci.sh uses.
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout: deploy/ci.sh is parallel-safe and several polecats run it
# at once from their own worktrees.
SUFFIX="${PDVS_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdvs-test-net-${SUFFIX}
MINIO_CTR=pdvs-test-minio-${SUFFIX}
NESSIE_CTR=pdvs-test-nessie-${SUFFIX}
JOB_IMAGE=pkdump-lake:vs-${SUFFIX}
BUCKET=pdvs-test-${SUFFIX}
AKID=pdvstestroot
SECRET=pdvstestsecret123
WAREHOUSE="s3://${BUCKET}/lake"

# Nothing here publishes a host port: every client runs as a container ON the
# network, which is the only way to reach an --internal one anyway — and it
# means this gate cannot pick a host port (pd-r0ri).

# The fixture's single price date, and an earlier one this gate invents at
# different prices so backfill has something to be right about.
DATE_NOW=2024-01-15
DATE_PAST=2024-01-10
# What the earlier day's prices are scaled by. Not 1.0, obviously, and not 0.5
# either — a factor that halves would make a "took the wrong day" bug look like
# a plausible rounding story. 0.25 cannot be mistaken for anything.
PAST_FACTOR=0.25

WORK=${WORK:-$(mktemp -d /tmp/pdvs-test.XXXXXX)}
DATA="$WORK/pkdump"
mkdir -p "$WORK/nessie" "$WORK/minio" "$DATA"
chmod 777 "$WORK/nessie" "$WORK/minio"

cleanup() {
	if [ -n "${KEEP:-}" ]; then
		echo ""
		echo "KEEP=1 — leaving everything up:"
		echo "  network ${NET} (internal: reach it with podman run --network ${NET})"
		echo "  data    ${DATA}"
		echo "  work    ${WORK}"
		echo "  tear down: podman rm -f ${MINIO_CTR} ${NESSIE_CTR}; podman network rm ${NET}"
		return
	fi
	podman rm -f "$MINIO_CTR" "$NESSIE_CTR" >/dev/null 2>&1 || true
	podman network rm "$NET" >/dev/null 2>&1 || true
	podman rmi -f "$JOB_IMAGE" >/dev/null 2>&1 || true
	# Nessie writes its version store as a SUBUID the invoking user cannot
	# unlink; a plain rm -rf leaves the whole work dir behind, every run.
	podman unshare rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

mc() { podman run --rm --network "$NET" -e MC_HOST_m="http://${AKID}:${SECRET}@${MINIO_CTR}:9000" "$MC_IMAGE" "$@"; }

# A lake job. $PKDUMP_HOME is mounted read-WRITE because publishing INTO a
# tenant database is the whole point of this tier — it is a throwaway fixture
# data dir, not anything real.
run_job() {
	podman run --rm --network "$NET" \
		-v "$DATA:/pkdump:Z" \
		-e PKDUMP_HOME=/pkdump \
		-e PKDUMP_LAKE_NESSIE_URI="http://${NESSIE_CTR}:19120/iceberg/" \
		-e PKDUMP_LAKE_S3_BUCKET="$BUCKET" \
		-e PKDUMP_LAKE_S3_REGION=us-west-2 \
		-e PKDUMP_LAKE_S3_ENDPOINT="http://${MINIO_CTR}:9000" \
		-e PKDUMP_LAKE_S3_ACCESS_KEY_ID="$AKID" \
		-e PKDUMP_LAKE_S3_SECRET_ACCESS_KEY="$SECRET" \
		-e PKDUMP_LAKE_S3_PATH_STYLE=1 \
		"$JOB_IMAGE" "$@"
}

pkdump() { PKDUMP_HOME="$DATA" "$REPO_DIR/target/debug/pkdump" "$@"; }

# One tenant's snapshot rows for a date, as a stable string.
snapshot_rows() {
	sqlite3 "$DATA/tenants/$1.sqlite" \
		"SELECT date, dimension, IFNULL(bucket,'-'), market_value, cost_basis, card_count
		   FROM collection_value_snapshot WHERE date = '$2'
		  ORDER BY dimension, bucket;"
}

database_id() {
	pkdump tenant list 2>/dev/null | awk -v h="$1" '$1 == h { print $2 }'
}

echo "==> §0  A data dir with two tenants, from the committed fixture"
[ -f "$REPO_DIR/tests/ui/fixtures/collection.sqlite" ] ||
	die "no UI fixture — run: cargo run --bin pkdump -- seed-fixture"
[ -x "$REPO_DIR/target/debug/pkdump" ] ||
	die "no pkdump binary — run: cargo build --bin pkdump"

cp "$REPO_DIR/tests/ui/fixtures/shared.sqlite" "$DATA/shared.sqlite"
# Through the real commands, so the registry and the opaque-id layout are the
# ones prod runs rather than a shape invented here.
pkdump tenant create alice >/dev/null
pkdump tenant create bob >/dev/null
ALICE=$(database_id alice)
BOB=$(database_id bob)
[ -n "$ALICE" ] && [ -n "$BOB" ] || die "tenant create did not register both handles"

# Alice holds the fixture collection. Bob holds a strict SUBSET of it, so the
# two tenants' totals cannot coincide by accident — §4 rests on that.
cp "$REPO_DIR/tests/ui/fixtures/collection.sqlite" "$DATA/tenants/${ALICE}.sqlite"
cp "$REPO_DIR/tests/ui/fixtures/collection.sqlite" "$DATA/tenants/${BOB}.sqlite"
sqlite3 "$DATA/tenants/${BOB}.sqlite" \
	"DELETE FROM collection_value_snapshot;
	 UPDATE collection SET status = 'sold' WHERE id > 5;"
sqlite3 "$DATA/tenants/${ALICE}.sqlite" "DELETE FROM collection_value_snapshot;"
echo "    alice ${ALICE}  bob ${BOB}"
echo "    alice owns $(sqlite3 "$DATA/tenants/${ALICE}.sqlite" "SELECT count(*) FROM collection WHERE status='owned';"), bob owns $(sqlite3 "$DATA/tenants/${BOB}.sqlite" "SELECT count(*) FROM collection WHERE status='owned';")"

echo "==> §1  Build the lake job image"
podman build -t "$JOB_IMAGE" -f "${REPO_DIR}/lake/Containerfile" "${REPO_DIR}/lake" >/dev/null
echo "    ${JOB_IMAGE}"

echo "==> §2  An INTERNAL network: this tier calls no upstream either"
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
	die "the job container can reach the internet (${EGRESS})"
echo "    ok   egress from the job image is blocked"

echo "==> §3  The lake (MinIO) and the catalog (Nessie)"
podman run -d --name "$MINIO_CTR" --network "$NET" \
	-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET" \
	-v "$WORK/minio:/data:Z" \
	"$MINIO_IMAGE" server /data >/dev/null
for _ in $(seq 1 30); do
	mc ls m >/dev/null 2>&1 && break
	sleep 1
done
mc ls m >/dev/null 2>&1 || die "MinIO never became reachable on ${NET}"
mc mb "m/${BUCKET}" >/dev/null

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
echo "    Nessie up, warehouse ${WAREHOUSE}"

echo "==> §3b Load catalog.prices for ${DATE_PAST} and ${DATE_NOW}"
# The table is created through pkdump_lake.prices.load_or_create with that
# module's own SCHEMA and PARTITION_SPEC, so this gate cannot be passing
# against a table shaped differently from the one pd-1ojt's build job writes.
# Where the ROWS come from is the only difference: the build job parses raw/,
# and this reads the same prices out of the fixture catalog, because the point
# here is the transform, not the parse.
run_job python -c "
import datetime as dt, sqlite3
import pyarrow as pa
from pkdump_lake.catalog import catalog
from pkdump_lake.prices import DEFAULT_TABLE, load_or_create

conn = sqlite3.connect('/pkdump/shared.sqlite')
conn.execute('PRAGMA query_only = ON')
base = [
    {'tcgplayer_product_id': int(p), 'sub_type_name': s, 'price_type': t, 'price': float(v)}
    for p, s, t, v in conn.execute(
        'SELECT tcgplayer_product_id, sub_type_name, price_type, price FROM prices')
]
conn.close()

table = load_or_create(catalog(), DEFAULT_TABLE)
arrow = table.schema().as_arrow()
for day, factor in (('${DATE_PAST}', ${PAST_FACTOR}), ('${DATE_NOW}', 1.0)):
    observed = dt.date.fromisoformat(day)
    rows = [dict(r, price=round(r['price'] * factor, 4), observed_date=observed) for r in base]
    table.append(pa.Table.from_pylist(rows, schema=arrow))
    print(f'    {len(rows)} rows at observed_date={day} (x{factor})')
" || die "could not load catalog.prices"

echo "==> §4  Byte-identical to the old path (pkdump data snapshot-value)"
# The old path first, into alice's database — the same two lines refresh runs
# as its step 7, which is still in place (deleting it is pd-hkbc, deliberately
# a separate bead so this one can be checked against the behaviour it replaces).
PKDUMP_USER=alice pkdump data snapshot-value --date "$DATE_NOW" >/dev/null ||
	die "the old path failed"
OLD_ROWS=$(snapshot_rows "$ALICE" "$DATE_NOW")
[ -n "$OLD_ROWS" ] || die "the old path wrote no rows for ${DATE_NOW}"
echo "    old path: $(echo "$OLD_ROWS" | wc -l) row(s)"

run_job pkdump-lake-value-snapshot --date "$DATE_NOW" || die "the transform failed"
NEW_ROWS=$(snapshot_rows "$ALICE" "$DATE_NOW")
[ "$OLD_ROWS" = "$NEW_ROWS" ] || {
	diag "the transform's rows differ from snapshot-value's:"
	diff <(echo "$OLD_ROWS") <(echo "$NEW_ROWS") >&2 || true
	die "not byte-identical — the rewrite is not a no-op"
}
echo "    ok   byte-identical to snapshot-value for ${DATE_NOW}"

echo "==> §5  BOTH tenants get a snapshot, and they are their own (pd-s5yn)"
BOB_ROWS=$(snapshot_rows "$BOB" "$DATE_NOW")
[ -n "$BOB_ROWS" ] ||
	die "bob got NO snapshot — this is pd-s5yn itself: the run reached one tenant"
echo "    bob:   $(echo "$BOB_ROWS" | wc -l) row(s)"
# Both were visited. Now prove each got its OWN numbers: writing alice's totals
# into every tenant would satisfy "bob has rows" and be just as wrong.
ALICE_ALL=$(snapshot_rows "$ALICE" "$DATE_NOW" | grep '|all|')
BOB_ALL=$(snapshot_rows "$BOB" "$DATE_NOW" | grep '|all|')
[ "$ALICE_ALL" != "$BOB_ALL" ] ||
	die "alice and bob have IDENTICAL totals (${ALICE_ALL}) — one tenant's numbers were written to both"
echo "    ok   alice ${ALICE_ALL}"
echo "    ok   bob   ${BOB_ALL}"

echo "==> §6  Re-running produces identical rows"
run_job pkdump-lake-value-snapshot --date "$DATE_NOW" >/dev/null || die "the second run failed"
[ "$(snapshot_rows "$ALICE" "$DATE_NOW")" = "$NEW_ROWS" ] ||
	die "alice's rows changed on a re-run — not idempotent"
[ "$(snapshot_rows "$BOB" "$DATE_NOW")" = "$BOB_ROWS" ] ||
	die "bob's rows changed on a re-run — not idempotent"
echo "    ok   both tenants unchanged"

echo "==> §7  Backfill: an older date gets THAT date's prices"
run_job pkdump-lake-value-snapshot --date "$DATE_PAST" || die "the backfill run failed"
PAST_ALL=$(snapshot_rows "$ALICE" "$DATE_PAST" | grep '|all|')
[ -n "$PAST_ALL" ] || die "no rows for ${DATE_PAST} — backfill produced nothing"
# The earlier day was loaded at PAST_FACTOR of the later day's prices, so the
# market value must be that much of it. Compared as a ratio rather than a
# literal, because the fixture's prices are the fixture's business.
NOW_VALUE=$(echo "$ALICE_ALL" | cut -d'|' -f4)
PAST_VALUE=$(echo "$PAST_ALL" | cut -d'|' -f4)
RATIO_OK=$(python3 -c "print(abs($PAST_VALUE - $NOW_VALUE * $PAST_FACTOR) < 0.01)")
[ "$RATIO_OK" = "True" ] ||
	die "${DATE_PAST} valued at ${PAST_VALUE}, expected ${PAST_FACTOR} of ${NOW_VALUE} — it used the wrong day's prices"
# And the later date is untouched: reconstructing the past must not rewrite it.
[ "$(snapshot_rows "$ALICE" "$DATE_NOW")" = "$NEW_ROWS" ] ||
	die "backfilling ${DATE_PAST} changed ${DATE_NOW}'s rows"
echo "    ok   ${DATE_PAST} valued ${PAST_VALUE} = ${PAST_FACTOR} x ${NOW_VALUE}, and ${DATE_NOW} untouched"

echo "==> §8  A failing tenant is skipped, named, and in the exit status"
# A third tenant whose database is gone. Registered, so the run must try her;
# missing, so she must fail — and alice and bob must still be published.
pkdump tenant create carol >/dev/null
CAROL=$(database_id carol)
rm -f "$DATA/tenants/${CAROL}.sqlite"
sqlite3 "$DATA/tenants/${ALICE}.sqlite" "DELETE FROM collection_value_snapshot WHERE date='$DATE_NOW';"
sqlite3 "$DATA/tenants/${BOB}.sqlite" "DELETE FROM collection_value_snapshot WHERE date='$DATE_NOW';"

# `|| rc=$?` rather than `set +e`: the ERR trap diagnostics.sh installs fires
# regardless of set -e, so a bare `set +e` would print a "!! FAILED" block for
# the failure this section is asserting. A failure a `||` handles is not one
# bash reports.
PARTIAL_RC=0
PARTIAL_OUT=$(run_job pkdump-lake-value-snapshot --date "$DATE_NOW" 2>&1) || PARTIAL_RC=$?
echo "$PARTIAL_OUT" | sed 's/^/      /'
[ "$PARTIAL_RC" -ne 0 ] ||
	die "a tenant failed and the run still exited 0 — silent partial success is the defect being designed out"
case "$PARTIAL_OUT" in
*"$CAROL"*) : ;;
*) die "the summary does not name the tenant that failed (${CAROL})" ;;
esac
[ -n "$(snapshot_rows "$ALICE" "$DATE_NOW")" ] ||
	die "carol's failure stranded alice — the run did not continue"
[ -n "$(snapshot_rows "$BOB" "$DATE_NOW")" ] ||
	die "carol's failure stranded bob — the run did not continue"
[ "$(snapshot_rows "$ALICE" "$DATE_NOW")" = "$NEW_ROWS" ] ||
	die "alice's rows changed when carol failed"
echo "    ok   exit ${PARTIAL_RC}, carol named, alice and bob published anyway"

echo "==> §8b A LOCKED tenant is skipped too, and the run still finishes"
# Missing is the easy half. The one that actually happens is a database
# somebody else is writing — so hold a real exclusive lock on bob for longer
# than the job is willing to wait, and require the same treatment: bob skipped
# and named, alice published, non-zero status.
sqlite3 "$DATA/tenants/${ALICE}.sqlite" "DELETE FROM collection_value_snapshot WHERE date='$DATE_NOW';"
sqlite3 "$DATA/tenants/${BOB}.sqlite" "DELETE FROM collection_value_snapshot WHERE date='$DATE_NOW';"
python3 - "$DATA/tenants/${BOB}.sqlite" <<'PY' &
import sqlite3, sys, time
conn = sqlite3.connect(sys.argv[1], isolation_level=None)
conn.execute("BEGIN EXCLUSIVE")
time.sleep(60)
PY
LOCKER=$!
# Wait for the lock to actually be held, rather than racing the interpreter.
for _ in $(seq 1 30); do
	sqlite3 "$DATA/tenants/${BOB}.sqlite" "BEGIN EXCLUSIVE; COMMIT;" 2>/dev/null || break
	sleep 1
done

LOCKED_RC=0
LOCKED_OUT=$(run_job pkdump-lake-value-snapshot --date "$DATE_NOW" 2>&1) || LOCKED_RC=$?
kill "$LOCKER" 2>/dev/null || true
wait "$LOCKER" 2>/dev/null || true
echo "$LOCKED_OUT" | sed 's/^/      /'
[ "$LOCKED_RC" -ne 0 ] || die "a locked tenant did not reach the exit status"
case "$LOCKED_OUT" in
*"$BOB"*"locked"* | *"locked"*"$BOB"*) : ;;
*) die "the summary does not name bob as locked: ${LOCKED_OUT}" ;;
esac
[ -n "$(snapshot_rows "$ALICE" "$DATE_NOW")" ] ||
	die "bob's lock stranded alice — the run did not continue"
echo "    ok   exit ${LOCKED_RC}, bob skipped as locked, alice published anyway"

echo "==> §9  The lake holds no tenant data (the standing decision, checked)"
# "Tenant data never enters the lake" is the decision the whole design rests
# on, and prose cannot enforce it. Two mechanical checks: what tables exist,
# and what columns they carry.
TABLES=$(run_job python -c "
from pkdump_lake.catalog import catalog
cat = catalog()
print(' '.join(sorted(
    f'{ns[0]}.{name}' for ns in cat.list_namespaces() for _, name in cat.list_tables(ns))))
" | tail -1)
echo "    tables: ${TABLES}"
case "$TABLES" in
*"catalog.prices"*) : ;;
*) die "catalog.prices is not in the catalog: ${TABLES}" ;;
esac
FIELDS=$(run_job python -c "
from pkdump_lake.catalog import catalog
print(' '.join(f.name for f in catalog().load_table('catalog.prices').schema().fields))
" | tail -1)
echo "    catalog.prices columns: ${FIELDS}"
for forbidden in tenant tenant_id database_id handle collection user; do
	case " $FIELDS " in
	*" $forbidden "*) die "catalog.prices carries a tenant-identifying column: ${forbidden}" ;;
	esac
done
# And nothing tenant-shaped reached the bucket: every object is warehouse
# metadata or data under lake/.
STRAY=$(mc ls --recursive "m/${BUCKET}" | grep -cv ' lake/' || true)
[ "$STRAY" -eq 0 ] || die "${STRAY} object(s) landed outside the lake/ prefix"
echo "    ok   no tenant column, no object outside lake/"

echo "==> §10 Provenance: each artefact records the Nessie commit it came from"
PUB=$(sqlite3 "$DATA/tenants/${ALICE}.sqlite" \
	"SELECT artefact || ' ' || date || ' ' || lake_ref FROM lake_publication ORDER BY date;")
[ -n "$PUB" ] || die "nothing recorded in lake_publication"
echo "$PUB" | sed 's/^/      /'
echo "$PUB" | grep -q 'main@' ||
	die "lake_ref is not a Nessie ref pinned to a commit: ${PUB}"
echo "    ok   both dates carry a pinned main@<hash>"

echo ""
echo "==> tests/lake/value-snapshot.sh PASSED"
