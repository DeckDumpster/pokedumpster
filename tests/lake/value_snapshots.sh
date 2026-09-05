#!/usr/bin/env bash
# Container-tier gate (pd-ruwh): the value-snapshot transform, for EVERY tenant.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/value_snapshots.sh          # ~3min, full run + teardown
#   KEEP=1 bash tests/lake/value_snapshots.sh   # leave MinIO + Nessie + WORK up
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# `pkdump data refresh` step 7 snapshots the ONE collection $PKDUMP_USER
# resolves to. There is no loop, nothing errors, and every other tenant has no
# value history at all (pd-s5yn). The transform that replaces it is only worth
# having if two things are true at once, and each one is easy to fake alone:
#
#   * it reproduces the old numbers EXACTLY — §5 diffs alice's rows against
#     the ones Rust `value_history::snapshot_today` computed over the same
#     fixture, byte for byte. A rewrite that is not first a no-op is a rewrite
#     nobody can trust with a chart they have been reading for months.
#   * it does it for EVERYONE — §5 also requires bob, who has never had a
#     snapshot row in his life, to come out with his own (different) numbers.
#
# And the failure mode the design warns about specifically — a run that
# half-completes and reports success — is checked in both directions: §5's
# missing tenant and §8's locked one are skipped, the run finishes the other
# tenants, and the process exits 2 rather than 0.
#
# ── AND THE SEALED HALF (pd-bbv7) ───────────────────────────────────────────
# The collection has two priced halves, and this gate now covers both. §5's
# byte-identity diff includes the `sealed` row, so the transform's sealed
# arithmetic is held to `value_history::snapshot_today`'s the same way the card
# arithmetic is. §5b is the hard gate of that bead stated as a real
# before-and-after: the same build, the same collection, with and without its
# sealed holdings, and `dimension='all'` required to be identical to the cent.
# §5c is the refusal a version rollback would need — sealed lots from an
# EARLIER read of the zone sitting beside this read's cards — and §5d is the
# inverse of §5b, since a transform that simply never wrote a sealed row would
# pass it. Bob, who owns no sealed product, is the tenant who must get no
# sealed row at all.
#
# ── WHY THIS GATE NOW STANDS UP A TENANT ZONE (pd-i08u) ─────────────────────
# The transform used to read each tenant's live `collection`. pd-i08u deleted
# that read: it values `zone_holdings`, and the only thing that writes
# `zone_holdings` is `pkdump-ship holdings` reading the tenant zone. So this
# gate cannot run the transform at all without keys, an outbox, a shipment and
# a read-back, and §4b is that chain.
#
# It is not scope creep and it is not a copy of tests/lake/phase3.sh, which
# asks a different question (is the valuation READING the zone — §6 there
# changes a collection without shipping it and requires the number not to
# move). What §5 here asks is the arithmetic question pd-ruwh has always
# asked: are the Python transform's numbers the Rust implementation's numbers?
# Losing that comparison because its input moved would be the quiet cost of
# pd-i08u, and it is the one thing in this file worth keeping whatever else
# changes. The round trip is now inside the claim rather than beside it —
# alice's holdings reach the SQL through Parquet, AES-GCM and
# `outbox::project`, and still have to produce byte-identical rows.
#
# The network is `--internal`, as in tests/lake/prices.sh: the transform tier
# is offline infrastructure, and nothing here may quietly depend on reaching
# an upstream.
#
# Prod-safe: its own podman network, MinIO, Nessie, temp dir and image tag.
# Touches no pkdump-* unit, no pkdump-*-data volume, no real S3 bucket and no
# real tenant database — the fixture's data directory is built from scratch in
# a temp dir on every run.
set -euo pipefail

NESSIE_IMAGE=${NESSIE_IMAGE:-ghcr.io/projectnessie/nessie:0.104.3}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

# Bounded condition polling, in one place for every harness (pd-86er).
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"
# shellcheck source=tests/lib/objects.sh
. "${REPO_DIR}/tests/lib/objects.sh"

die() {
	diag "!! $*"
	exit 1
}

# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout: several polecats run deploy/ci.sh at once from their own
# worktrees, and a fixed name means one run's opening teardown kills another's.
SUFFIX="${PDVALUE_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdvalue-net-${SUFFIX}
MINIO_CTR=pdvalue-minio-${SUFFIX}
NESSIE_CTR=pdvalue-nessie-${SUFFIX}
LOCK_CTR=pdvalue-lock-${SUFFIX}
JOB_IMAGE=pkdump-lake:value-${SUFFIX}
APP_IMAGE=localhost/pkdump:value-${SUFFIX}
BUCKET=pdvalue-${SUFFIX}
AKID=pdvaluetestroot
SECRET=pdvaluetestsecret123

WAREHOUSE="s3://${BUCKET}/lake"

# Nothing here publishes a host port (pd-r0ri): every check runs as a container
# ON the network, which is the only way to reach an --internal one anyway.
WORK=${WORK:-$(mktemp -d /tmp/pdvalue.XXXXXX)}
FIXTURE="$WORK/fixture"
KEYS="$WORK/keys"
mkdir -p "$WORK/nessie" "$WORK/minio" "$FIXTURE" "$KEYS"
chmod 777 "$WORK/nessie" "$WORK/minio"

# Kept in step with crates/pkdump-ingest/tests/value_snapshot_fixture.rs.
DATE_OLD=2026-08-09
DATE_NEW=2026-08-10

cleanup() {
	podman rm -f "$LOCK_CTR" >/dev/null 2>&1 || true
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
	podman rmi -f "$JOB_IMAGE" "$APP_IMAGE" >/dev/null 2>&1 || true
	# Nessie writes its version store as a SUBUID the invoking user cannot
	# unlink; a plain rm -rf leaves the whole work dir behind, every run.
	podman unshare rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

mc() { podman run --rm --network "$NET" -e MC_HOST_m="http://${AKID}:${SECRET}@${MINIO_CTR}:9000" -v "$FIXTURE:/fixture:ro,Z" "$MC_IMAGE" "$@"; }

# A lake job. The fixture's whole data directory is mounted read-WRITE: the
# transform's output IS a write into each tenant's database, which is the one
# thing this gate is here to observe. Not `:ro` for the catalog either — these
# are WAL databases and SQLite cannot open one through a read-only mount.
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

# The transform, over the fixture's data directory.
snapshot() {
	run_job pkdump-lake-value-snapshots --data-dir /fixture/home "$@"
}

# The shipper and the zone reader, out of the SHIPPED image — the master key
# and the tenant profile, which the lake image above holds neither of and must
# not. Same split as tests/lake/phase3.sh and deploy/ship.sh: the zone side is
# Rust because the envelope, the key derivation and `outbox::project` have one
# implementation each, and it is not this one.
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
		--entrypoint pkdump-ship "$APP_IMAGE" "$@" --data-dir /data
}

# `pkdump` itself, for key custody and the backfill. No bucket: this is the
# ONLINE binary, and it never reaches one.
pkdump_cli() {
	podman run --rm --pull=never \
		-v "${FIXTURE}/home:/data:Z" \
		-v "${KEYS}:/keys:rw,Z" \
		-e PKDUMP_HOME=/data \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		--entrypoint pkdump "$APP_IMAGE" "$@"
}

# Run a job and require one of the exit statuses given, printing its output on
# any other. `ghost` is registered with no database, so every job that walks
# the registry here legitimately exits 2 — and a helper that accepted any
# non-zero would hide exactly what several sections are asserting.
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

# One tenant's snapshot rows for one date, in the SAME text form
# `value_snapshot_fixture.rs::dump_snapshot` writes: same columns, same NULL
# placeholder, same ordering, same ten decimal places. Diffing formatted text
# rather than comparing floats in one language is the only way this comparison
# can be made across the two implementations at all.
dump() {
	local db="$1" date="$2"
	run_job python -c "
import sqlite3
conn = sqlite3.connect('/fixture/home/tenants/${db}.sqlite')
for dim, bucket, mv, cb, cc in conn.execute('''
    SELECT dimension, bucket, market_value, cost_basis, card_count
      FROM collection_value_snapshot WHERE date = ? ORDER BY dimension, bucket''', ('${date}',)):
    print(dim, bucket if bucket is not None else '-', f'{mv:.10f}', f'{cb:.10f}', cc, sep='\t')
"
}

# A scalar out of a tenant's database. The SQL travels as an ARGUMENT rather
# than interpolated into the Python source: every query below ends in a quoted
# date, and `'...'` pasted inside `'''...'''` closes the wrong quote.
tenant_query() {
	local db="$1" sql="$2"
	run_job python -c "
import sqlite3, sys
print(sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()[0])
" "/fixture/home/tenants/${db}.sqlite" "$sql" | tail -1
}

echo "==> §0  Build the data directory, and the snapshot Rust computes from it"
# A catalog imported from landed bytes, a registry provisioned by the REAL
# `pkdump tenant create`, two collections, and the expected rows.
PKDUMP_VALUE_FIXTURE_OUT="$FIXTURE" \
	cargo test --manifest-path "${REPO_DIR}/Cargo.toml" -p pkdump-ingest --test value_snapshot_fixture ||
	die "the fixture generator failed — see crates/pkdump-ingest/tests/value_snapshot_fixture.rs"
[ -f "$FIXTURE/home/shared.sqlite" ] || die "the fixture produced no shared.sqlite"
[ -f "$FIXTURE/home/registry.sqlite" ] || die "the fixture produced no registry"

ALICE=$(awk -F'\t' '$1 == "alice" {print $2}' "$FIXTURE/tenants.tsv")
BOB=$(awk -F'\t' '$1 == "bob" {print $2}' "$FIXTURE/tenants.tsv")
GHOST=$(awk -F'\t' '$1 == "ghost" {print $2}' "$FIXTURE/tenants.tsv")
[ -n "$ALICE" ] && [ -n "$BOB" ] && [ -n "$GHOST" ] || die "the fixture wrote no tenant map"
echo "    alice=${ALICE} bob=${BOB} ghost=${GHOST} (missing, on purpose)"

echo "==> §1  Build both images"
podman build -t "$JOB_IMAGE" -f "${REPO_DIR}/lake/Containerfile" "${REPO_DIR}/lake" >/dev/null
echo "    ${JOB_IMAGE}"
# The app image carries the shipper and the zone reader. Through
# pkdump_image_ensure so a CI run that already built it (pd-5l2e) re-tags
# rather than rebuilding.
pkdump_image_ensure "$APP_IMAGE" "$REPO_DIR" >"${WORK}/build.log" 2>&1 || {
	tail -40 "${WORK}/build.log"
	die "the app image build failed"
}
echo "    ${APP_IMAGE}"

echo "==> §2  An INTERNAL network: the transform tier is offline infrastructure"
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
	die "the job container can reach the internet (${EGRESS}) — an offline transform proves nothing"
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
mc mirror --quiet /fixture/raw-zone "m/${BUCKET}" >/dev/null

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

echo "==> §4  catalog.prices and catalog.sealed_prices, both days, from raw/"
run_job pkdump-lake-build-prices --ingest-date "$DATE_OLD" >/dev/null || die "the ${DATE_OLD} build failed"
run_job pkdump-lake-build-prices --ingest-date "$DATE_NEW" >/dev/null || die "the ${DATE_NEW} build failed"
echo "    ok   ${DATE_OLD} and ${DATE_NEW} in the lake"

echo "==> §4b The holdings the transform values: outbox -> zone -> zone_holdings"
# pd-i08u. The transform has no online read left, so this is the chain that
# gives it anything to value at all. It is the real one — the shipped binary,
# a real bucket, real encryption — because a fixture that hand-wrote
# `zone_holdings` would prove the SQL runs and nothing about whether a
# collection survives the journey.
pkdump_cli keys init >/dev/null || die "keys init failed"
pkdump_cli keys register "$ALICE" >/dev/null || die "keys register alice failed"
pkdump_cli keys register "$BOB" >/dev/null || die "keys register bob failed"

# These collections were built before anything shipped, which is the shape of
# every existing box (pd-whsw): the triggers only record what changes AFTER
# them, so without the backfill the zone would be empty and correct. 2, not 0:
# ghost is registered with no database, and every job here says so.
expect_rc 2 "${WORK}/emit.log" pkdump_cli outbox emit --all --all-tenants
expect_rc 2 "${WORK}/ship.log" ship_job run
expect_rc 2 "${WORK}/holdings.log" ship_job holdings

# The premise every section below rests on, checked rather than assumed: what
# came back out of the zone IS the collection. If this is short, §5's diff
# would fail for a reason that has nothing to do with the arithmetic it exists
# to check, and the message would send the reader to the wrong file.
for T in "$ALICE" "$BOB"; do
	LIVE=$(tenant_query "$T" "SELECT COUNT(*) FROM collection")
	STAGED=$(tenant_query "$T" "SELECT COUNT(*) FROM zone_holdings")
	[ "$LIVE" = "$STAGED" ] ||
		die "tenant ${T}: ${STAGED} row(s) came back from the zone against ${LIVE} in the collection — the round trip lost or duplicated holdings"
	# And the other half of the holdings (pd-bbv7). Alice has sealed lots and
	# bob has none, so this is checked over a tenant with some and a tenant
	# with none — the second is what stops "the sealed table exists" passing
	# for "the sealed rows arrived".
	LIVE_SEALED=$(tenant_query "$T" "SELECT COUNT(*) FROM sealed_collection")
	STAGED_SEALED=$(tenant_query "$T" "SELECT COUNT(*) FROM zone_sealed_holdings")
	[ "$LIVE_SEALED" = "$STAGED_SEALED" ] ||
		die "tenant ${T}: ${STAGED_SEALED} sealed lot(s) came back from the zone against ${LIVE_SEALED} held — sealed product ships and must come back"
	echo "    ok   ${T}: ${STAGED} holding(s) + ${STAGED_SEALED} sealed lot(s) round-tripped through the zone"
done

echo "==> §5  Every tenant, and the numbers are the old ones exactly"
RC=0
OUT=$(snapshot --date "$DATE_NEW" 2>&1) || RC=$?
echo "$OUT" | sed 's/^/    | /'
# ghost is registered and has no database: skipped, and the run says so in its
# exit status rather than in its output alone.
[ "$RC" -eq 2 ] ||
	die "a run that skipped a tenant exited ${RC}, not 2 — a half-completed run must not read as success"
case "$OUT" in
*"ghost"*"SKIPPED"*) ;;
*) die "the run did not name the tenant it skipped" ;;
esac

diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "the transform's rows differ from value_history::snapshot_today — the rewrite is not a no-op, or the zone round trip is not lossless"
echo "    ok   alice byte-identical to snapshot_today, valued through the zone"

# It really ran: the fixture leaves no provenance row, and every write here
# leaves one carrying the PINNED ref it read.
REF=$(tenant_query "$ALICE" "SELECT lake_ref FROM collection_value_snapshot_run WHERE date = '${DATE_NEW}'")
case "$REF" in
*@*) echo "    ok   derived from ${REF}" ;;
*) die "the run recorded lake_ref='${REF}' — a bare branch is not a version" ;;
esac

# The bead, inverted: bob has never had a snapshot row.
BOB_ROWS=$(tenant_query "$BOB" "SELECT COUNT(*) FROM collection_value_snapshot WHERE date = '${DATE_NEW}'")
[ "$BOB_ROWS" -gt 0 ] ||
	die "bob got no snapshot — this is pd-s5yn, the bug the whole bead exists to fix"
BOB_VALUE=$(tenant_query "$BOB" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'all' AND date = '${DATE_NEW}'")
ALICE_VALUE=$(tenant_query "$ALICE" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'all' AND date = '${DATE_NEW}'")
[ "$BOB_VALUE" != "$ALICE_VALUE" ] ||
	die "bob and alice have the same total (${BOB_VALUE}) — one collection was valued twice"
echo "    ok   bob ${BOB_ROWS} row(s), total ${BOB_VALUE} (alice ${ALICE_VALUE})"

echo "==> §5b THE HARD GATE: sealed does not move the cards"
# pd-bbv7. `dimension='all'` means the LOOSE CARDS and must keep meaning it to
# the cent — every row ever written under it was computed over cards alone, and
# a blend would restate months of chart with nothing saying so.
#
# Stated as a real before-and-after rather than as an assertion about a
# constant: alice's sealed holdings are emptied out of the staging table, the
# transform is re-run, and the cards rows are required to be byte-identical to
# the ones it just produced WITH them. The staging table is the transform's
# only source of sealed holdings, so emptying it is exactly "this build without
# the feature", computed by this build.
#
# `zone_sealed_holdings` is transport state and the next `pkdump-ship holdings`
# rewrites it wholesale, so emptying it here damages nothing — and §5d puts it
# back before anything else reads it.
CARDS_WITH_SEALED=$(dump "$ALICE" "$DATE_NEW" | grep -v '^sealed	')
SEALED_ROW=$(dump "$ALICE" "$DATE_NEW" | grep '^sealed	') ||
	die "alice has no sealed row — the fixture holds sealed lots, so the dimension is missing"
echo "    alice sealed row: ${SEALED_ROW}"

# The run row is rewritten with it, in one statement, because a read that
# materialised no sealed lots is what this is standing in for — and the
# transform refuses a staging table whose count disagrees with the read that
# filled it (that mismatch is a rollback's fingerprint, not a fixture's).
run_job python -c "
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1], isolation_level=None)
conn.execute('BEGIN IMMEDIATE')
conn.execute('DELETE FROM zone_sealed_holdings')
conn.execute('UPDATE zone_holdings_run SET sealed_rows = 0')
conn.execute('COMMIT')
" "/fixture/home/tenants/${ALICE}.sqlite" || die "could not empty the sealed staging table"

snapshot --date "$DATE_NEW" --tenant alice >/dev/null ||
	die "the cards-only run failed"
CARDS_ALONE=$(dump "$ALICE" "$DATE_NEW")
diff <(printf '%s\n' "$CARDS_WITH_SEALED") <(printf '%s\n' "$CARDS_ALONE") ||
	die "owning sealed product moved a cards row — dimension='all' has been blended, which is \
the one thing this change may not do"
case "$CARDS_ALONE" in
*sealed*) die "a collection with no sealed holdings got a sealed row" ;;
esac
echo "    ok   the cards dimensions are identical with and without sealed, to the cent"

echo "==> §5c A half-materialised zone is refused, and a real read restores it"
# The rollback shape: an older `pkdump-ship holdings` rebuilds zone_holdings and
# leaves zone_sealed_holdings alone, so the staging table holds an EARLIER
# read's sealed lots beside this read's cards. Nothing else can see that, and
# valued it would be a plausible wrong number.
run_job python -c "
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1], isolation_level=None)
conn.execute(\"INSERT INTO zone_sealed_holdings (id, product_id, quantity, status, added_at) \
               VALUES (99, 201, 1, 'owned', '2026-01-01T00:00:00Z')\")
" "/fixture/home/tenants/${ALICE}.sqlite" || die "could not stage a mismatched sealed lot"
HALF_RC=0
HALF_OUT=$(snapshot --date "$DATE_NEW" --tenant alice 2>&1) || HALF_RC=$?
[ "$HALF_RC" -eq 1 ] ||
	die "a half-materialised zone exited ${HALF_RC}: the only tenant asked for was skipped, so the run valued NOBODY and that is exit 1"
case "$HALF_OUT" in
*"not materialised together"*) ;;
*) die "the refusal did not say the two staging tables came from different reads: ${HALF_OUT}" ;;
esac
echo "    ok   sealed lots from an earlier read are refused, not valued"

echo "==> §5d And the sealed row comes back when the zone is read again"
# The inverse, and it is what stops §5b passing on a transform that simply
# never writes a sealed row. Re-read the zone — the real path, the shipped
# binary — and the same sealed row must return.
expect_rc 2 "${WORK}/holdings2.log" ship_job holdings
snapshot --date "$DATE_NEW" --tenant alice >/dev/null || die "the re-run failed"
diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "re-reading the zone did not restore alice's rows"
echo "    ok   ${SEALED_ROW} is back, from the zone"


# And bob, who owns no sealed product at all, has no sealed row — which is the
# rule "a bucket exists when something is in it", not a special case.
BOB_SEALED=$(tenant_query "$BOB" \
	"SELECT COUNT(*) FROM collection_value_snapshot WHERE dimension = 'sealed'")
[ "$BOB_SEALED" = "0" ] ||
	die "bob owns no sealed product and got ${BOB_SEALED} sealed row(s) — a flat zero line is \
not the same answer as 'none held'"
echo "    ok   bob owns no sealed product and has no sealed row"

echo "==> §6  Re-running produces the same rows"
snapshot --date "$DATE_NEW" --tenant alice --tenant bob >/dev/null ||
	die "a run over two present tenants must exit 0"
diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "re-running changed alice's rows — the publish is not idempotent"
RUNS=$(tenant_query "$ALICE" "SELECT COUNT(*) FROM collection_value_snapshot_run")
[ "$RUNS" = "1" ] || die "provenance rows accumulated (${RUNS}) instead of being replaced"
echo "    ok   unchanged, one provenance row"

echo "==> §7  Backfill: an older date reconstructs THAT day"
# The fixture removed these rows; nothing ever captured them. They exist only
# because the older partition is still in the lake.
snapshot --date "$DATE_OLD" --tenant alice >/dev/null || die "the backfill run failed"
diff <(dump "$ALICE" "$DATE_OLD") "$FIXTURE/expected-alice-${DATE_OLD}.tsv" ||
	die "the backfilled day does not match what Rust computes over that day's prices"
OLD_VALUE=$(tenant_query "$ALICE" "SELECT market_value FROM collection_value_snapshot WHERE dimension = 'all' AND date = '${DATE_OLD}'")
[ "$OLD_VALUE" != "$ALICE_VALUE" ] ||
	die "the older day came out at today's value (${OLD_VALUE}) — it read the wrong partition"
echo "    ok   ${DATE_OLD} total ${OLD_VALUE}, distinct from ${DATE_NEW}'s ${ALICE_VALUE}"
# And it did not disturb the day beside it.
diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "backfilling ${DATE_OLD} changed ${DATE_NEW} — the delete is not scoped to one date"

echo "==> §8  A tenant nobody can write to is skipped, and the run finishes"
# A real SQLite writer lock, held from another process, is the failure the
# nightly run will actually meet: a tenant mid-import, or a restore in flight.
podman run -d --name "$LOCK_CTR" --network "$NET" -v "$FIXTURE:/fixture:Z" "$JOB_IMAGE" python -c "
import sqlite3, time
conn = sqlite3.connect('/fixture/home/tenants/${BOB}.sqlite', isolation_level=None)
conn.execute('BEGIN EXCLUSIVE')
print('LOCK HELD', flush=True)
time.sleep(120)
" >/dev/null
# WAIT for the lock, rather than sleeping long enough that it has probably been
# taken (pd-86er). The old form polled until the container's logs were readable
# and then slept 3s on top — and the whole of §8 is vacuous if the lock is not
# actually held when the job asks for it, so this is a condition worth being
# certain of rather than one worth guessing at. The locker says when it has it.
locker_ready() { logs_match "$LOCK_CTR" 'LOCK HELD'; }
wait_until 60 0.25 locker_ready ||
	die "the locker never took an EXCLUSIVE lock on ${BOB} — §8 would prove nothing"

LOCK_RC=0
LOCK_OUT=$(snapshot --date "$DATE_NEW" --tenant alice --tenant bob 2>&1) || LOCK_RC=$?
podman rm -f "$LOCK_CTR" >/dev/null 2>&1 || true
echo "$LOCK_OUT" | sed 's/^/    | /'
[ "$LOCK_RC" -eq 2 ] ||
	die "a locked tenant produced exit ${LOCK_RC}, not 2"
case "$LOCK_OUT" in
*"bob"*"SKIPPED"*) ;;
*) die "the locked tenant was not reported as skipped" ;;
esac
# alice was snapshotted anyway: one tenant's lock is not the run's failure.
diff <(dump "$ALICE" "$DATE_NEW") "$FIXTURE/expected-alice-${DATE_NEW}.tsv" ||
	die "the run stopped at the locked tenant instead of continuing"
echo "    ok   bob skipped, alice snapshotted, exit 2"

echo "==> §9  The lake still holds no tenant data"
# The standing decision, checked mechanically rather than by review: Iceberg
# holds catalog data only. A transform that ever wrote a collection back into
# the lake would show up here as a new namespace or table.
#
# There ARE tenant-keyed objects in this bucket now (§4b shipped some), and
# that does not weaken the claim — it is the pd-7x83 axis: the CATALOG zone is
# cross-tenant and holds nothing keyed by tenant, the TENANT zone is
# tenant-keyed by construction and lives under `tenant/`. So the Iceberg check
# below is joined by the prefix check beside it, which is the half that would
# notice the two zones bleeding into each other.
TABLES=$(run_job python -c "
from pkdump_lake.catalog import catalog
cat = catalog()
print(' '.join(sorted(
    '.'.join(ns) + '.' + name
    for ns in cat.list_namespaces()
    for _, name in cat.list_tables(ns))))
" | tail -1)
[ "$TABLES" = "catalog.prices catalog.sealed_prices" ] ||
	die "the lake holds '${TABLES}' — tenant data NEVER enters the lake, and neither does anything else this bead adds"
echo "    ok   ${TABLES}"

# Through the verified listing (pd-cxq4). "No tenant-keyed object outside
# tenant/" is exactly what an `mc` run that died under load also reports, and
# the die below would never fire on it.
STRAY_LISTING="$(object_store_ls "" mc ls --recursive "m/${BUCKET}")" ||
	die "the bucket listing could not be trusted — see above"
STRAY=$(awk '{print $NF}' <<<"$STRAY_LISTING" |
	grep -v '^tenant/' | grep -c 'database_id=' || true)
[ "$STRAY" = "0" ] ||
	die "${STRAY} tenant-keyed object(s) outside tenant/ — the catalog zone must hold nothing keyed by tenant"
echo "    ok   every tenant-keyed object is under tenant/"

echo "==> §10 The SHIPPED wrapper the timer runs, over the same fixture"
# pd-8m5c. Everything above drives the job directly, which is exactly how the
# transform shipped: correct, and scheduled nowhere. deploy/value-snapshots.sh is
# what pkdump-value-snapshots@<instance>.service executes, so it gets driven here
# rather than described — it is the file that resolves an instance's network,
# data volume and credentials, and none of that is exercised by calling the job
# by hand.
#
# The unit-file half (ordering after the refresh, SuccessExitStatus=2, the
# calendar entry derived from the refresh's own bounds) is asserted hermetically
# in tests/deploy/run.sh §10; this is the half that needs a real lake.
VS_HOME="$WORK/vshome"
mkdir -p "$VS_HOME/.config/pkdump" "$VS_HOME/.config/containers/systemd"
INST="pdvalue-${SUFFIX}"

# The wrapper asks the instance's Quadlet unit which container store to use
# (pd-fite). Rendered and stamped through the real functions, so it names the
# store this gate is actually running in — a no-op when there is no alternate
# store, which is prod's shape.
sed -e "s|{{INSTANCE}}|${INST}|g" -e 's|{{PORT}}:8080|:8080|' \
	"${REPO_DIR}/deploy/pkdump.container" >"$VS_HOME/.config/containers/systemd/pkdump-${INST}.container"
pkdump_store_stamp_unit "$VS_HOME/.config/containers/systemd/pkdump-${INST}.container"

# The host config the wrapper refuses to run without, pointed at this gate's
# throwaway MinIO and Nessie instead of a bucket.
cat >"$VS_HOME/.config/pkdump/lake.env" <<EOF
PKDUMP_LAKE_NESSIE_URI=http://${NESSIE_CTR}:19120/iceberg/
PKDUMP_LAKE_S3_BUCKET=${BUCKET}
PKDUMP_LAKE_S3_REGION=us-west-2
PKDUMP_LAKE_S3_ENDPOINT=http://${MINIO_CTR}:9000
PKDUMP_LAKE_S3_ACCESS_KEY_ID=${AKID}
PKDUMP_LAKE_S3_SECRET_ACCESS_KEY=${SECRET}
PKDUMP_LAKE_S3_PATH_STYLE=1
EOF

wrapper() {
	HOME="$VS_HOME" \
		PKDUMP_ALERTS_ENV="$VS_HOME/.config/pkdump/no-alerts.env" \
		PKDUMP_LAKE_NETWORK="$NET" \
		PKDUMP_LAKE_JOB_IMAGE="$JOB_IMAGE" \
		PKDUMP_LAKE_DATA="$FIXTURE/home" \
		bash "${REPO_DIR}/deploy/value-snapshots.sh" "$INST" "$@"
}

# A day nothing has snapshotted yet, so the rows this section asserts on can only
# have come from the run below. AFTER both partitions, not before: prices are the
# newest quote at or before the date, so a date earlier than the whole lake has
# nothing to value a collection with and the run would fail rather than skip.
DATE_SCHED=2026-08-11
WRAP_RC=0
WRAP_OUT=$(wrapper --date "$DATE_SCHED" 2>&1) || WRAP_RC=$?
echo "$WRAP_OUT" | sed 's/^/    | /'

# ghost is registered with no database: skipped. The wrapper passes the job's
# status through rather than flattening it — the unit is where "2 is not a
# failure" is expressed, and a wrapper that returned 0 would leave the unit
# unable to tell a partial run from a complete one.
[ "$WRAP_RC" -eq 2 ] ||
	die "the wrapper exited ${WRAP_RC} on a run that skipped a tenant, not 2"
case "$WRAP_OUT" in
*"PARTIAL — tenants skipped: ghost"*) ;;
*) die "the wrapper did not name the tenant it skipped — a partial run must not be silent" ;;
esac
echo "    ok   exit 2 passed through, ghost named"

# And it really ran the transform: every tenant with a database has that day's
# rows, which nothing else in this file has written.
for T in "$ALICE" "$BOB"; do
	N=$(tenant_query "$T" "SELECT COUNT(*) FROM collection_value_snapshot WHERE date = '${DATE_SCHED}'")
	[ "$N" -gt 0 ] ||
		die "tenant ${T} got no rows for ${DATE_SCHED} — the scheduled path snapshots nobody"
	echo "    ok   ${T}: ${N} row(s) for ${DATE_SCHED}"
done

# A complete run is a plain success. Same wrapper, same path, the two tenants
# that actually exist.
wrapper --date "$DATE_SCHED" --tenant alice --tenant bob >/dev/null ||
	die "a wrapper run over two present tenants must exit 0"
echo "    ok   a complete run exits 0"

echo "==> §11 The readiness probe a netns repair waits on, against the real catalog"
# pd-p39v. When `pkdump_store_netns_ensure` repairs a wedged rootless netns it
# restarts what was on it — here, Nessie — and `systemctl restart` returns while
# the JVM is still booting. The repair therefore waits for the catalog to ANSWER,
# and deploy/lake-lib.sh is what "answer" means.
#
# That probe runs ONLY during a wedge, which is the least-exercised path in the
# whole deployment: a typo in it would surface at 07:00 on the one morning it was
# needed, as the same false page it exists to prevent. tests/deploy/run.sh §8c
# drives the waiting around it against a fake podman; this drives the probe
# itself against the real thing, on a real network, with the real job image.
# shellcheck source=deploy/lake-lib.sh
. "${REPO_DIR}/deploy/lake-lib.sh"

PROBE_HEALTH="$(pkdump_lake_health_url "http://${NESSIE_CTR}:19120/iceberg/")"
[ "$PROBE_HEALTH" = "http://${NESSIE_CTR}:19120/api/v2/config" ] ||
	die "the health URL derived from the catalog is '${PROBE_HEALTH}'"
pkdump_lake_catalog_answering "$NET" "$JOB_IMAGE" "$PROBE_HEALTH" ||
	die "the probe says a Nessie that has been serving this whole file is not answering"
echo "    ok   a live catalog answers, asked from a container on the network"

# And it is a real check, not a rubber stamp. A probe that cannot fail would make
# the wait return on its first ask, which is the bug with more code in it.
if pkdump_lake_catalog_answering "$NET" "$JOB_IMAGE" "http://${NESSIE_CTR}:19121/api/v2/config"; then
	die "the probe answered yes about a port nothing is listening on"
fi
echo "    ok   and says no when nothing is listening"

echo ""
echo "==> tests/lake/value_snapshots.sh PASSED"
