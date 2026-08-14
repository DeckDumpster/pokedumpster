#!/usr/bin/env bash
# Container-tier gate (pd-1ojt): catalog.prices is built from raw/ ALONE.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/prices.sh          # ~3min, full run + teardown
#   KEEP=1 bash tests/lake/prices.sh   # leave MinIO + Nessie + WORK up
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# The landing zone's whole claim is that a table can be rebuilt from the bytes
# we kept, without asking an upstream anything. A build job that quietly
# reaches out for one missing piece would pass every row-count check and leave
# `raw/` decorative — and we would not find out until the day an upstream is
# down, which is the day the lake was bought for.
#
# So the network is not a convention here, it is removed:
#
#   * the podman network is `--internal`. §2 proves it by trying to reach the
#     internet FROM THE JOB IMAGE and requiring the attempt to fail.
#   * the only things on that network are MinIO (the lake) and Nessie (the
#     catalog). No upstream stand-in is ever started — the raw bytes were
#     landed on the host, before any of this came up.
#
# And the numbers are checked against a second implementation rather than
# against themselves: `crates/pkdump-ingest/tests/prices_fixture.rs` lands the
# raw objects through the REAL client and landing zone, and imports the SAME
# responses into a shared.sqlite through the REAL `import_prices`. §4 compares
# them row for row. A disagreement is a finding about one parser or the other.
#
# Prod-safe: its own podman network, MinIO, Nessie, temp dir and image tag.
# Touches no pkdump-* unit, no pkdump-*-data volume, no real S3 bucket and no
# tenant database — the catalog zone never holds tenant data at all, which
# tests/lake/tenant_isolation_test.sh asserts on the tree in the lint tier
# (pd-cgi9): catalog.prices carries no tenant-identifying column, and the module
# that builds it opens no database.
set -euo pipefail

NESSIE_IMAGE=${NESSIE_IMAGE:-ghcr.io/projectnessie/nessie:0.104.3}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

die() {
	diag "!! $*"
	exit 1
}

# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout: several polecats run deploy/ci.sh at once from their own
# worktrees, and a fixed name means one run's opening teardown kills another's.
SUFFIX="${PDPRICES_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdprices-net-${SUFFIX}
MINIO_CTR=pdprices-minio-${SUFFIX}
NESSIE_CTR=pdprices-nessie-${SUFFIX}
JOB_IMAGE=pkdump-lake:prices-${SUFFIX}
BUCKET=pdprices-${SUFFIX}
AKID=pdpricestestroot
SECRET=pdpricestestsecret123

# One bucket, two prefixes that never overlap: `raw/` is the landing zone the
# job reads, `lake/` is the warehouse Iceberg writes.
WAREHOUSE="s3://${BUCKET}/lake"

# Nothing here publishes a host port. Health checks and jobs all run as
# containers ON the network, which is the only way to reach an --internal one
# anyway — and it means this gate cannot pick a port (pd-r0ri).
WORK=${WORK:-$(mktemp -d /tmp/pdprices.XXXXXX)}
FIXTURE="$WORK/fixture"
mkdir -p "$WORK/nessie" "$WORK/minio" "$FIXTURE"
chmod 777 "$WORK/nessie" "$WORK/minio"

# The three dates the fixture lands, and what each one is for. Kept in step
# with crates/pkdump-ingest/tests/prices_fixture.rs.
DATE_INCOMPLETE=2026-08-08 # only a run that died partway
DATE_OLD=2026-08-09        # one complete run
DATE_NEW=2026-08-10        # a complete run, then a failed retry
ROWS_PER_DAY=11

cleanup() {
	if [ -n "${KEEP:-}" ]; then
		echo ""
		echo "KEEP=1 — leaving everything up:"
		echo "  network ${NET} (internal: reach it with podman run --network ${NET})"
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

mc() { podman run --rm --network "$NET" -e MC_HOST_m="http://${AKID}:${SECRET}@${MINIO_CTR}:9000" -v "$FIXTURE:/fixture:ro,Z" "$MC_IMAGE" "$@"; }

# A lake job. The shared.sqlite the fixture built is mounted so §5 can compare
# the two without copying a catalog into the image. Not `:ro` — the catalog is
# a WAL database and SQLite cannot open one through a read-only mount at all;
# the verifier's connection is `query_only` instead. It is a throwaway fixture
# either way.
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

# A stable fingerprint of one day of the table: row count and a digest over
# the sorted rows. Two builds of the same day must produce the same string.
day_digest() {
	run_job python -c "
import hashlib, sys
from pyiceberg.expressions import EqualTo
from pkdump_lake.catalog import catalog
rows = catalog().load_table('catalog.prices').scan(
    row_filter=EqualTo('observed_date', '$1')).to_arrow().to_pylist()
keyed = sorted(
    (r['tcgplayer_product_id'], r['sub_type_name'], r['price_type'], r['price'])
    for r in rows)
print(len(keyed), hashlib.sha256(repr(keyed).encode()).hexdigest()[:16])
" | tail -1
}

echo "==> §0  Land raw and build shared.sqlite from the SAME upstream bytes"
# Produced by the real TcgcsvClient through the real RawLanding, on the host,
# before anything below exists. Nothing in the gate's network ever serves a
# TCGCSV response.
PKDUMP_PRICES_FIXTURE_OUT="$FIXTURE" \
	cargo test --manifest-path "${REPO_DIR}/Cargo.toml" -p pkdump-ingest --test prices_fixture ||
	die "the fixture generator failed — see crates/pkdump-ingest/tests/prices_fixture.rs"
[ -f "$FIXTURE/shared.sqlite" ] || die "the fixture produced no shared.sqlite"
echo "    $(find "$FIXTURE/raw-zone" -name 'part-*.zst' | wc -l) part(s), $(find "$FIXTURE/raw-zone" -name '_manifest.json' | wc -l) manifest(s)"

echo "==> §1  Build the lake job image"
podman build -t "$JOB_IMAGE" -f "${REPO_DIR}/lake/Containerfile" "${REPO_DIR}/lake" >/dev/null
echo "    ${JOB_IMAGE}"

echo "==> §2  An INTERNAL network: there is no upstream to call"
podman network create --internal "$NET" >/dev/null
# The assertion the whole bead turns on, made mechanically rather than by
# convention. If this ever starts succeeding, the network stopped being
# internal and every "built from raw alone" claim below is unproven.
EGRESS=$(podman run --rm --network "$NET" "$JOB_IMAGE" python -c "
import socket
try:
    socket.create_connection(('1.1.1.1', 443), timeout=5)
    print('REACHABLE')
except OSError:
    print('blocked')
")
[ "$EGRESS" = "blocked" ] ||
	die "the job container can reach the internet (${EGRESS}) — 'built from raw alone' would prove nothing"
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

# Upload the landing zone exactly as it was landed: same keys, same bytes.
mc mirror --quiet /fixture/raw-zone "m/${BUCKET}" >/dev/null
RAW_OBJECTS=$(mc ls --recursive "m/${BUCKET}" | grep -c ' raw/' || true)
[ "$RAW_OBJECTS" -gt 0 ] || die "nothing landed under raw/ in ${BUCKET}"
echo "    ${RAW_OBJECTS} raw object(s) in ${BUCKET}"

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

echo "==> §4  Build ${DATE_OLD} from raw/, with nowhere to fetch from"
run_job pkdump-lake-build-prices --ingest-date "$DATE_OLD" ||
	die "the build failed for ${DATE_OLD}"
OLD_STATE=$(day_digest "$DATE_OLD")
[ "${OLD_STATE%% *}" = "$ROWS_PER_DAY" ] ||
	die "${DATE_OLD}: expected ${ROWS_PER_DAY} rows, got ${OLD_STATE%% *}"
echo "    ok   ${OLD_STATE}"

echo "==> §5  It matches the shared.sqlite built from the same bytes"
run_job pkdump-lake-verify-prices --sqlite /fixture/shared.sqlite --observed-date "$DATE_OLD" ||
	die "catalog.prices and shared.sqlite disagree for ${DATE_OLD}"

echo "==> §6  Re-running the build is idempotent"
# Same input, same table STATE. The Nessie commit is new either way — a build
# that ran is a fact worth recording — but the rows may not double or drift.
run_job pkdump-lake-build-prices --ingest-date "$DATE_OLD" >/dev/null ||
	die "the second build failed"
AGAIN=$(day_digest "$DATE_OLD")
[ "$AGAIN" = "$OLD_STATE" ] ||
	die "rebuilding ${DATE_OLD} changed it: ${OLD_STATE} -> ${AGAIN}"
echo "    ok   ${AGAIN} unchanged"

echo "==> §7  A newer date lands beside it, and takes the COMPLETE run"
# ${DATE_NEW} holds two runs: one complete, and a later retry that died with
# different prices in it. Reading the newest run would be wrong; reading both
# would be wronger.
run_job pkdump-lake-build-prices --ingest-date "$DATE_NEW" ||
	die "the build failed for ${DATE_NEW}"
NEW_STATE=$(day_digest "$DATE_NEW")
[ "${NEW_STATE%% *}" = "$ROWS_PER_DAY" ] ||
	die "${DATE_NEW}: expected ${ROWS_PER_DAY} rows, got ${NEW_STATE%% *}"
MARKET=$(run_job python -c "
from pyiceberg.expressions import And, EqualTo
from pkdump_lake.catalog import catalog
rows = catalog().load_table('catalog.prices').scan(row_filter=And(
    EqualTo('observed_date', '$DATE_NEW'), EqualTo('tcgplayer_product_id', 101),
    EqualTo('price_type', 'market'), EqualTo('sub_type_name', 'Normal'),
)).to_arrow().to_pylist()
print(rows[0]['price'])
" | tail -1)
# 27.5 is the complete run's price; 34.5 is the failed retry's.
[ "$MARKET" = "27.5" ] ||
	die "${DATE_NEW} took the wrong run: product 101 market is ${MARKET}, expected 27.5"
echo "    ok   ${NEW_STATE}, from the complete run (101 market ${MARKET})"

echo "==> §8  Rebuilding the OLDER date reproduces that day, not today"
run_job pkdump-lake-build-prices --ingest-date "$DATE_OLD" >/dev/null ||
	die "the older-date rebuild failed"
[ "$(day_digest "$DATE_OLD")" = "$OLD_STATE" ] ||
	die "rebuilding ${DATE_OLD} after ${DATE_NEW} did not reproduce it"
[ "$(day_digest "$DATE_NEW")" = "$NEW_STATE" ] ||
	die "rebuilding ${DATE_OLD} disturbed ${DATE_NEW} — the partition filter is wrong"
echo "    ok   both days intact, each from its own ingest_date"

echo "==> §9  A date with no complete run refuses, and says why"
REFUSE_RC=0
REFUSE_OUT=$(run_job pkdump-lake-build-prices --ingest-date "$DATE_INCOMPLETE" 2>&1) || REFUSE_RC=$?
[ "$REFUSE_RC" -ne 0 ] ||
	die "a date whose only run died was built as if it were whole"
case "$REFUSE_OUT" in
*"no complete run"*) echo "    ok   refused, naming the runs it found" ;;
*) die "it refused for the wrong reason: ${REFUSE_OUT}" ;;
esac

# Opting in builds it and the table has to SAY it is not a whole day.
run_job pkdump-lake-build-prices --ingest-date "$DATE_INCOMPLETE" --allow-incomplete >/dev/null ||
	die "--allow-incomplete did not build"
COMPLETE_PROP=$(run_job python -c "
from pkdump_lake.catalog import catalog
t = catalog().load_table('catalog.prices')
print(t.current_snapshot().summary['pkdump.raw-complete'])
" | tail -1)
[ "$COMPLETE_PROP" = "false" ] ||
	die "the partial day's snapshot claims pkdump.raw-complete=${COMPLETE_PROP}"
echo "    ok   --allow-incomplete builds it and records pkdump.raw-complete=false"

echo "==> §10 The freshness check judges the newest partition, not completeness"
# pd-up36. A lake whose price build has quietly stopped keeps producing correct
# arithmetic over stale prices, advancing nightly, with nothing saying the
# numbers stopped moving. That is detected by the AGE of the newest partition
# and by nothing else — in particular NOT by whether a day was complete, which
# is conservative across datasets and would page on a normal flaky night
# (pd-me6h). §9 just built an INCOMPLETE day into this table; the verdict below
# has to be about ${DATE_NEW} regardless.
AGE_RC=0
AGE_OUT=$(run_job pkdump-lake-prices-age --as-of "$DATE_NEW" --max-age-days 2 2>&1) || AGE_RC=$?
[ "$AGE_RC" -eq 0 ] ||
	die "the day the table was just built for read as stale (rc ${AGE_RC}): ${AGE_OUT}"
case "$AGE_OUT" in
*"observed_date=${DATE_NEW}"*) ;;
*) die "the check did not judge the newest partition: ${AGE_OUT}" ;;
esac
case "$AGE_OUT" in
*"incomplete"* | *"complete"*) die "the age verdict mentions completeness: ${AGE_OUT}" ;;
esac
echo "    ok   fresh at ${DATE_NEW}, and the incomplete day in the table changed nothing"

# The boundary, from both sides. Derived from the fixture's own dates so this
# stays true as the calendar moves past them.
AT_LIMIT=$(date -u -d "${DATE_NEW} +2 days" +%F)
PAST_LIMIT=$(date -u -d "${DATE_NEW} +3 days" +%F)
run_job pkdump-lake-prices-age --as-of "$AT_LIMIT" --max-age-days 2 >/dev/null 2>&1 ||
	die "exactly 2 days behind read as stale at a 2-day limit"

STALE_RC=0
STALE_OUT=$(run_job pkdump-lake-prices-age --as-of "$PAST_LIMIT" --max-age-days 2 2>&1) || STALE_RC=$?
[ "$STALE_RC" -eq 3 ] ||
	die "3 days behind a 2-day limit exited ${STALE_RC}, expected 3 (stale)"
case "$STALE_OUT" in
*STALE*) echo "    ok   2 days behind is fresh, 3 days behind is STALE and says so" ;;
*) die "it went stale without saying so: ${STALE_OUT}" ;;
esac

echo "==> §11 A table nothing has ever built is stale, not an error"
# The most extreme form of "prices are not arriving". It must not read as a
# crash: a lakehouse armed and producing nothing is exactly what should page.
NOTABLE_RC=0
NOTABLE_OUT=$(run_job pkdump-lake-prices-age --table catalog.nosuchprices \
	--as-of "$DATE_NEW" 2>&1) || NOTABLE_RC=$?
[ "$NOTABLE_RC" -eq 3 ] ||
	die "a table that does not exist exited ${NOTABLE_RC}, expected 3 (stale)"
case "$NOTABLE_OUT" in
*"there is no catalog.nosuchprices"*) echo "    ok   refused as STALE, naming the table" ;;
*) die "it did not say which table was missing: ${NOTABLE_OUT}" ;;
esac

echo "==> §12 The SHIPPED wrapper, end to end, against this lake"
# deploy/prices.sh is what pkdump-prices@<instance>.service runs. Everything
# above tests the jobs; this tests the thing a timer actually invokes — build a
# day, then judge the table, then decide which of 0/2/1 the night was.
cat > "$WORK/lake.env" <<EOF
PKDUMP_LAKE_NESSIE_URI=http://${NESSIE_CTR}:19120/iceberg/
PKDUMP_LAKE_S3_BUCKET=${BUCKET}
PKDUMP_LAKE_S3_REGION=us-west-2
PKDUMP_LAKE_S3_ENDPOINT=http://${MINIO_CTR}:9000
PKDUMP_LAKE_S3_ACCESS_KEY_ID=${AKID}
PKDUMP_LAKE_S3_SECRET_ACCESS_KEY=${SECRET}
PKDUMP_LAKE_S3_PATH_STYLE=1
EOF

# A window wide enough that the fixture's fixed dates stay "recent" forever;
# §10 already pinned where the boundary is.
FOREVER=36500
wrapper() { # wrapper <max-age-days> [args...] — prints output, returns its status
	local max_age="$1"
	shift
	# The credentials are blanked, not merely left unconfigured. (c) below
	# provokes a real alarm on purpose, and a gate that pages an operator when
	# it works as designed is pd-n0lf: nine real pushes overnight from drills
	# tearing themselves down, after which nobody trusts the pager. alert.sh
	# reads an empty token as unconfigured and refuses, which is what the
	# wrapper's "reached nobody" path expects.
	PKDUMP_LAKE_ENV="$WORK/lake.env" \
		PKDUMP_ALERTS_ENV="$WORK/no-such-alerts.env" \
		PUSHOVER_TOKEN= PUSHOVER_USER= \
		PKDUMP_LAKE_NETWORK="$NET" \
		PKDUMP_LAKE_JOB_IMAGE="$JOB_IMAGE" \
		PKDUMP_LAKE_PRICES_MAX_AGE_DAYS="$max_age" \
		bash "${REPO_DIR}/deploy/prices.sh" "pdprices-${SUFFIX}" "$@" 2>&1
}

# (a) The decision this bead had to make, made against a real refusal: §9 proved
# the bare job REFUSES ${DATE_INCOMPLETE}. The nightly wrapper passes
# --allow-incomplete, so the same day builds — and an incomplete-but-recent day
# raises nothing at all.
W_RC=0
W_OUT=$(wrapper "$FOREVER" --ingest-date "$DATE_INCOMPLETE") || W_RC=$?
[ "$W_RC" -eq 0 ] ||
	die "the wrapper failed on the day the bare job refuses (rc ${W_RC}):
${W_OUT}"
case "$W_OUT" in
*"prices: OK"*) ;;
*) die "the wrapper did not report a clean night: ${W_OUT}" ;;
esac
case "$W_OUT" in
*STALE* | *MISSED*) die "an incomplete-but-recent day raised an alarm: ${W_OUT}" ;;
esac
echo "    ok   an incomplete day builds and does not alarm"

# (b) A night whose raw never landed. Today has no partition in this fixture, so
# the build genuinely fails — over a table that is still recent. Not an outage:
# tomorrow's build fills the day in, and the unit's SuccessExitStatus=2 keeps it
# out of `failed`.
W_RC=0
W_OUT=$(wrapper "$FOREVER") || W_RC=$?
[ "$W_RC" -eq 2 ] ||
	die "a missed day over a fresh table exited ${W_RC}, expected 2:
${W_OUT}"
case "$W_OUT" in
*"prices: MISSED"*) echo "    ok   a missed day over a fresh table is a warning, exit 2" ;;
*) die "it exited 2 without saying what was missed: ${W_OUT}" ;;
esac

# (c) The alarm the bead exists for, and it fires on a night the BUILD SUCCEEDED
# — which is the whole point of judging the table rather than the job's report
# of itself.
W_RC=0
W_OUT=$(wrapper 0 --ingest-date "$DATE_NEW") || W_RC=$?
[ "$W_RC" -eq 1 ] ||
	die "a stale table exited ${W_RC}, expected 1:
${W_OUT}"
case "$W_OUT" in
*"prices: built"*) ;;
*) die "the build did not run before the freshness check: ${W_OUT}" ;;
esac
case "$W_OUT" in
*"prices: STALE"*) echo "    ok   a stale table pages even after a clean build, exit 1" ;;
*) die "it failed for the wrong reason: ${W_OUT}" ;;
esac

echo ""
echo "==> tests/lake/prices.sh PASSED"
