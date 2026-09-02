#!/usr/bin/env bash
# Container-tier gate (pd-dxn3): the SHIPPER, against a real object store,
# under the real tenant credential policy, killed for real.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/shipper.sh          # ~3min after the image is warm
#   KEEP=1 bash tests/lake/shipper.sh   # leave MinIO + WORK up for poking
#
# ── WHAT THIS EXISTS TO CATCH, THAT THE RUST TIER CANNOT ────────────────────
# crates/pkdump-ship/tests/shipping.rs already proves the four claims the bead
# asks for — gap detection, idempotence, resumability, encryption under the
# right key — fifteen tests over a `DirStore`. What it cannot prove is
# everything that is only true of the deployment:
#
#   §2  the SHIPPED IMAGE carries the shipper at all. A binary left out of the
#       Containerfile passes every cargo test there is.
#   §4  the shipper reaches S3 under the TENANT profile and writes objects a
#       real bucket accepts. `DirStore` cannot fail an IAM policy.
#   §5  what it wrote is unreadable to the CATALOG credentials — the boundary
#       item 2 built, now asserted against real shipped data rather than a
#       governance probe. And seen RED: the same assertion against a
#       whole-bucket grant must fail, or it is checking nothing.
#   §6  a real SIGKILL of a real process mid-run, resumed by a second real
#       process. The Rust tier panics inside a fixture store; this kills a PID.
#   §7  the wrapper `deploy/ship.sh` maps the job's four exit statuses to the
#       four things an operator needs to hear — including 3, the gap, which is
#       the one nothing else in the system would ever notice.
#   §8  pd-385w's headline proof, re-stated against THIS zone (pd-880q). That
#       one is a claim about `outbox::project`; this one puts Parquet, a
#       derived key, a real bucket and `pkdump-ship holdings` between the two
#       sides of it. Ship a collection's history one day at a time, delete the
#       tenant's partition outright, rebuild by `pkdump outbox emit --all`,
#       ship again — and the holdings the zone describes must be identical.
#       §8b is what makes the rest of it mean anything: over an emptied
#       partition the read-back must materialise NOTHING, because a reader
#       that had quietly fallen back to the live `collection` table would pass
#       every other check in this file.
#
# ── NO REAL TENANT DATA ─────────────────────────────────────────────────────
# Every holding here is an invented printing id in a throwaway collection, in a
# throwaway bucket, on a throwaway network. The tenant zone is the SUBJECT of
# this design, so its fixtures are treated as if they were real: nothing that
# has ever been anybody's is written, and `cleanup` tears the bucket down.
#
# Prod-safe: its own podman network, its own MinIO, its own bucket, its own
# image tag, its own data directory. It touches no pkdump-* unit, no
# pkdump-*-data volume, no real bucket, no real tenant database, and never the
# operator's master key — `PKDUMP_KEYS_CONF_DIR` points the wrapper at a temp
# directory holding a key minted for this run and destroyed with it.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout — deploy/ci.sh runs gates in parallel and several
# polecats run whole suites beside each other from their own worktrees.
SUFFIX="${PDSHIP_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
NET="pdship-net-${SUFFIX}"
MINIO_CTR="pdship-minio-${SUFFIX}"
BUCKET="pdship-${SUFFIX}"
IMAGE="localhost/pkdump:ship-${SUFFIX}"
MINIO_PORT=${MINIO_PORT:-$(free_port)}

ROOT_AK=pdshiproot
ROOT_SK=pdshiprootsecret123
CAT_AK=pdshipcatalog
CAT_SK=pdshipcatalogsecret123
TEN_AK=pdshiptenant
TEN_SK=pdshiptenantsecret123

WORK=${WORK:-$(mktemp -d /tmp/pdship.XXXXXX)}
DATA="$WORK/data"
KEYS="$WORK/keys"
mkdir -p "$WORK/minio" "$WORK/policies" "$DATA" "$KEYS"
chmod 777 "$WORK/minio" "$WORK/policies"

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
die() {
	diag "!! $*"
	exit 1
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving everything up:"
		echo "  MinIO   http://localhost:${MINIO_PORT}  (${ROOT_AK} / ${ROOT_SK})"
		echo "  bucket  ${BUCKET}"
		echo "  work    ${WORK}   (data ${DATA}, key ${KEYS}/tenant-master.key)"
		echo "  tear down: podman rm -f ${MINIO_CTR}; podman network rm ${NET}"
		return
	fi
	podman rm -f "$MINIO_CTR" >/dev/null 2>&1 || true
	podman network rm "$NET" >/dev/null 2>&1 || true
	podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
	podman unshare rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

MINIO_URL="http://${MINIO_CTR}:9000"

# mc as an arbitrary identity — the alias is always `x`, so §5's red run is
# the SAME command answering differently rather than a differently-spelled one.
mc_as() {
	local ak="$1" sk="$2"
	shift 2
	podman run --rm --network "$NET" \
		-v "$WORK/policies:/policies:ro,Z" \
		-e "MC_HOST_x=http://${ak}:${sk}@${MINIO_CTR}:9000" \
		"$MC_IMAGE" "$@"
}
mc_root() { mc_as "$ROOT_AK" "$ROOT_SK" "$@"; }

# The shipped image, as any offline job runs it: the data directory read-write,
# the master key read-only, the tenant profile and the MinIO endpoint from the
# environment. Nothing here is on the app container.
ship_job() { # ship_job [args...]
	podman run --rm --network "$NET" --pull=never \
		-v "${DATA}:/data:Z" \
		-v "${KEYS}/tenant-master.key:/keys/tenant-master.key:ro,Z" \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		-e PKDUMP_LAKE_S3_BUCKET="$BUCKET" \
		-e PKDUMP_LAKE_S3_REGION=us-west-2 \
		-e PKDUMP_LAKE_S3_ENDPOINT="$MINIO_URL" \
		-e PKDUMP_TENANT_AWS_PROFILE=pkdump-tenant-test \
		-e AWS_ACCESS_KEY_ID="$TEN_AK" -e AWS_SECRET_ACCESS_KEY="$TEN_SK" \
		--entrypoint pkdump-ship "$IMAGE" "$@"
}

# `pkdump` itself, for provisioning. No bucket, no key: this is the ONLINE
# binary and it has no business holding either.
pkdump_cli() { # pkdump_cli [args...]
	podman run --rm --pull=never \
		-v "${DATA}:/data:Z" \
		-v "${KEYS}:/keys:rw,Z" \
		-e PKDUMP_HOME=/data \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		--entrypoint pkdump "$IMAGE" "$@"
}

# Insert holdings straight into a collection with host python3. The triggers
# in the file are what write the outbox — which is the whole point of pd-5m54,
# and means this fixture needs no application code at all.
add_holdings() { # add_holdings <db> <n> <date>
	python3 - "$1" "$2" "$3" <<-'PY'
		import sqlite3, sys
		db, n, date = sys.argv[1], int(sys.argv[2]), sys.argv[3]
		conn = sqlite3.connect(db)
		before = conn.execute("SELECT coalesce(max(seq),0) FROM ownership_outbox").fetchone()[0]
		for i in range(n):
		    conn.execute(
		        "INSERT INTO collection (printing_id, acquired_at, source) VALUES (?, ?, 'gate')",
		        (f"fixture-{date}-{i}", f"{date}T00:00:00Z"),
		    )
		# The trigger stamps occurred_at from the clock; as_of is a partition
		# derived from it, so the fixture says which day this batch belongs to
		# exactly as a run on that day would have found it.
		conn.execute(
		    "UPDATE ownership_outbox SET occurred_at = ? || substr(occurred_at, 11) WHERE seq > ?",
		    (date, before),
		)
		conn.commit()
		conn.close()
	PY
}

# Listed from the BUCKET ROOT, not from `tenant/`: mc prints keys relative to
# the prefix it was given, and a key with its own prefix chopped off is not a
# key the shipper's decrypt path could ever be handed.
zone_ls() {
	mc_root ls --recursive "x/${BUCKET}" 2>/dev/null |
		awk '{print $NF}' | grep '^tenant/' | sort
}
zone_count() { zone_ls | grep -c . || true; }

# ---------------------------------------------------------------------------
log "0. MinIO, a bucket, and the two identities from the REAL policy documents"
podman network create "$NET" >/dev/null
podman run -d --name "$MINIO_CTR" --network "$NET" \
	-p "127.0.0.1:${MINIO_PORT}:9000" \
	-e MINIO_ROOT_USER="$ROOT_AK" -e MINIO_ROOT_PASSWORD="$ROOT_SK" \
	-v "$WORK/minio:/data:Z" \
	"$MINIO_IMAGE" server /data >/dev/null

minio_live() { curl -sf -o /dev/null "http://localhost:${MINIO_PORT}/minio/health/live"; }
wait_until 30 0.25 minio_live || true
minio_live || die "MinIO never became healthy on ${MINIO_PORT}"
mc_root mb "x/${BUCKET}" >/dev/null

# The catalog zone, so §5's denial is a denial and not an absence.
printf '%s' '{"catalog":"a landed upstream response"}' |
	mc_root pipe "x/${BUCKET}/raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-14/run=01GATE/part-0000.json" >/dev/null

# Rendered by the real deploy/setup-tenant-zone.sh — the same documents AWS
# would get, bucket substituted, so this gate cannot pass against a policy
# nobody deploys.
PKDUMP_LAKE_ENV="${WORK}/render.env" \
	bash "${REPO_DIR}/deploy/setup-tenant-zone.sh" --render --bucket "$BUCKET" \
	--out "${WORK}/policies" >/dev/null || die "--render failed"

policy_install() { # policy_install <name> <file>
	mc_root admin policy create x "$1" "/policies/$2" >/dev/null 2>&1 ||
		mc_root admin policy add x "$1" "/policies/$2" >/dev/null 2>&1 ||
		die "could not install the $1 policy"
}
mc_root admin user add x "$CAT_AK" "$CAT_SK" >/dev/null
mc_root admin user add x "$TEN_AK" "$TEN_SK" >/dev/null
policy_install pdship-catalog catalog-credentials.json
policy_install pdship-tenant tenant-credentials.json
mc_root admin policy attach x pdship-catalog --user "$CAT_AK" >/dev/null 2>&1 ||
	die "could not attach the catalog policy"
mc_root admin policy attach x pdship-tenant --user "$TEN_AK" >/dev/null 2>&1 ||
	die "could not attach the tenant policy"
echo "  bucket ${BUCKET}; ${CAT_AK} and ${TEN_AK} carry the two documents verbatim"

# ---------------------------------------------------------------------------
log "1. build the shipped image"
# Through the helper, never a bare `podman build`: deploy/ci.sh builds the
# image ONCE per run and exports PKDUMP_PREBUILT_IMAGE.
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >"${WORK}/build.log" 2>&1 ||
	{
		tail -40 "${WORK}/build.log"
		die "the image build failed"
	}

log "2. the image carries the shipper"
# A binary left out of the Containerfile passes every cargo test there is.
check "pkdump-ship is in the image" "ok" \
	"$(podman run --rm --pull=never --entrypoint pkdump-ship "$IMAGE" --help >/dev/null 2>&1 && echo ok || echo missing)"

# ---------------------------------------------------------------------------
log "3. two tenants, a master key, and holdings that never existed"
pkdump_cli tenant create alice >"${WORK}/alice.txt" || die "tenant create alice failed"
pkdump_cli tenant create bob >"${WORK}/bob.txt" || die "tenant create bob failed"
ALICE="$(sed -n 's/.*-> database \([A-Z0-9]*\) at.*/\1/p' "${WORK}/alice.txt")"
BOB="$(sed -n 's/.*-> database \([A-Z0-9]*\) at.*/\1/p' "${WORK}/bob.txt")"
[[ -n "$ALICE" && -n "$BOB" && "$ALICE" != "$BOB" ]] ||
	die "could not read the database ids back: alice=${ALICE} bob=${BOB}"

pkdump_cli keys init >/dev/null || die "keys init failed"
pkdump_cli keys register "$ALICE" >/dev/null || die "keys register alice failed"
pkdump_cli keys register "$BOB" >/dev/null || die "keys register bob failed"
[[ -f "${KEYS}/tenant-master.key" ]] || die "no master key on the host"

add_holdings "${DATA}/tenants/${ALICE}.sqlite" 5 2026-08-13
add_holdings "${DATA}/tenants/${ALICE}.sqlite" 4 2026-08-14
add_holdings "${DATA}/tenants/${BOB}.sqlite" 3 2026-08-14
echo "  alice ${ALICE}: 9 events over two days; bob ${BOB}: 3"

# ---------------------------------------------------------------------------
log "4. the shipper writes to a real bucket, under the tenant profile"
ship_job run --data-dir /data >"${WORK}/run1.log" 2>&1 ||
	{
		cat "${WORK}/run1.log"
		die "the first run failed"
	}
check "objects in the zone" "3" "$(zone_count)"
KEYS_WRITTEN="$(zone_ls)"
echo "$KEYS_WRITTEN" | sed 's/^/    /'
check "alice's first day is one part named for its range" "yes" \
	"$(grep -qF "tenant/database_id=${ALICE}/dataset=holdings/as_of=2026-08-13/part-seq-000000000001-000000000005.parquet.enc" <<<"$KEYS_WRITTEN" && echo yes || echo no)"
check "bob's data is under bob's prefix only" "1" \
	"$(grep -c "database_id=${BOB}/" <<<"$KEYS_WRITTEN")"

# What landed is ciphertext. Read the raw object as ROOT — the point is that
# the bytes themselves say nothing, whoever is allowed to fetch them.
FIRST_KEY="$(grep "database_id=${ALICE}/" <<<"$KEYS_WRITTEN" | head -1)"
mc_root cat "x/${BUCKET}/${FIRST_KEY}" >"${WORK}/object.bin" 2>/dev/null ||
	die "could not read ${FIRST_KEY} back"
check "the object is not a bare Parquet file" "sealed" \
	"$(head -c 4 "${WORK}/object.bin" | grep -q PAR1 && echo plaintext || echo sealed)"
check "no printing id is readable in it" "opaque" \
	"$(grep -qa 'fixture-2026-08-13' "${WORK}/object.bin" && echo readable || echo opaque)"

# …and it decrypts, through the shipped CLI, to the events that were shipped.
ship_job decrypt --data-dir /data --key "$FIRST_KEY" --json >"${WORK}/decrypted.jsonl" 2>&1 ||
	{
		cat "${WORK}/decrypted.jsonl"
		die "decrypt failed"
	}
check "it decrypts to the five events" "5" "$(grep -c . "${WORK}/decrypted.jsonl")"
check "…carrying the holdings that were added" "yes" \
	"$(grep -q 'fixture-2026-08-13-0' "${WORK}/decrypted.jsonl" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "5. what it wrote is unreachable to the CATALOG credentials"
# The boundary item 2 built, now asserted against real shipped data rather than
# a governance probe.
catalog_cannot_read_holdings() {
	local bad=0
	mc_as "$CAT_AK" "$CAT_SK" cat "x/${BUCKET}/${FIRST_KEY}" >/dev/null 2>&1 && {
		diag "   !! catalog credentials READ ${FIRST_KEY}"
		bad=1
	}
	mc_as "$CAT_AK" "$CAT_SK" ls "x/${BUCKET}/tenant/" >/dev/null 2>&1 && {
		diag "   !! catalog credentials LISTED tenant/"
		bad=1
	}
	return "$bad"
}
catalog_cannot_read_holdings || die "the catalog role reached a tenant's shipped holdings"
echo "  ok   the catalog role can neither read nor list what the shipper wrote"

# …and the tenant role, which just wrote all of that, still cannot reach raw/.
mc_as "$TEN_AK" "$TEN_SK" cat \
	"x/${BUCKET}/raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-14/run=01GATE/part-0000.json" \
	>/dev/null 2>&1 &&
	die "the tenant role read the catalog zone"
echo "  ok   the tenant role cannot read raw/, having just written tenant/"

log "5b. SEEN RED: the same check against a whole-bucket grant must fail"
# A boundary check that has only ever been seen passing is not known to check
# anything. Replace the catalog policy — not add beside it; an explicit Deny
# beats any Allow, which is tests/lake/tenant_zone.sh §6b's property.
WIDE='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":["arn:aws:s3:::'"${BUCKET}"'","arn:aws:s3:::'"${BUCKET}"'/*"]}]}'
printf '%s' "$WIDE" >"${WORK}/policies/wide.json"
policy_install pdship-wide wide.json
mc_root admin policy detach x pdship-catalog --user "$CAT_AK" >/dev/null 2>&1 ||
	die "could not detach the catalog policy"
mc_root admin policy attach x pdship-wide --user "$CAT_AK" >/dev/null 2>&1 ||
	die "could not attach the wide policy"
if catalog_cannot_read_holdings; then
	die "the check PASSED against a credential that can read the whole bucket — it is checking nothing"
fi
echo "  red  a whole-bucket grant reads the holdings, and the check fails as it must"
mc_root admin policy detach x pdship-wide --user "$CAT_AK" >/dev/null 2>&1 || true
mc_root admin policy attach x pdship-catalog --user "$CAT_AK" >/dev/null 2>&1 || true
catalog_cannot_read_holdings || die "restoring the correct policy did not restore the boundary"
echo "  ok   and green again once the correct policy is back"

# ---------------------------------------------------------------------------
log "6. KILLED mid-run, for real, and resumed"
# The Rust tier panics inside a fixture store, which lands on the same part
# every time. This kills a process: the part is in the bucket, the cursor in
# SQLite does not know it, and nothing coordinates the two.
add_holdings "${DATA}/tenants/${ALICE}.sqlite" 12 2026-08-15
BEFORE_CURSOR="$(python3 -c "
import sqlite3,sys
print(sqlite3.connect(sys.argv[1]).execute('SELECT shipped_thru FROM ownership_outbox_cursor').fetchone()[0])
" "${DATA}/tenants/${ALICE}.sqlite")"

# PKDUMP_SHIP_CRASH_AFTER_PARTS aborts the process the instant the second part
# has landed and before the cursor records it. SIGKILL would test the same
# thing on whichever iteration it happened to hit.
# `|| KILLED_RC=$?` rather than `set +e`, because a death here is the POINT.
# `set +e` stops errexit but not the ERR trap, so the bare form printed
# diagnostics.sh's `!! FAILED` banner in the middle of a passing gate — a
# false alarm exactly where a reader is most likely to believe it. A `||`
# list is one of the contexts bash does not run the ERR trap for.
KILLED_RC=0
podman run --rm --network "$NET" --pull=never \
	-v "${DATA}:/data:Z" \
	-v "${KEYS}/tenant-master.key:/keys/tenant-master.key:ro,Z" \
	-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
	-e PKDUMP_LAKE_S3_BUCKET="$BUCKET" -e PKDUMP_LAKE_S3_REGION=us-west-2 \
	-e PKDUMP_LAKE_S3_ENDPOINT="$MINIO_URL" \
	-e PKDUMP_TENANT_AWS_PROFILE=pkdump-tenant-test \
	-e AWS_ACCESS_KEY_ID="$TEN_AK" -e AWS_SECRET_ACCESS_KEY="$TEN_SK" \
	-e PKDUMP_SHIP_CRASH_AFTER_PARTS=2 \
	--entrypoint pkdump-ship "$IMAGE" run --data-dir /data --tenant alice --max-rows 4 \
	>"${WORK}/killed.log" 2>&1 || KILLED_RC=$?
[[ "$KILLED_RC" -ne 0 ]] || die "the crash seam did not fire — the run exited 0"
check "the process died rather than finishing" "yes" \
	"$(grep -q 'CRASH_AFTER_PARTS' "${WORK}/killed.log" && echo yes || echo no)"
AFTER_KILL="$(zone_count)"
MID_CURSOR="$(python3 -c "
import sqlite3,sys
print(sqlite3.connect(sys.argv[1]).execute('SELECT shipped_thru FROM ownership_outbox_cursor').fetchone()[0])
" "${DATA}/tenants/${ALICE}.sqlite")"
check "two more parts landed before the kill" "5" "$AFTER_KILL"
check "…and the cursor recorded only the first of them" "$((BEFORE_CURSOR + 4))" "$MID_CURSOR"

ship_job run --data-dir /data --max-rows 4 >"${WORK}/resume.log" 2>&1 ||
	{
		cat "${WORK}/resume.log"
		die "the resumed run failed"
	}
# 9 + 3 + 12 events; alice's last day is 12 events at 4 to a part.
check "the resumed run leaves the expected object count" "6" "$(zone_count)"
FINAL_CURSOR="$(python3 -c "
import sqlite3,sys
print(sqlite3.connect(sys.argv[1]).execute('SELECT shipped_thru FROM ownership_outbox_cursor').fetchone()[0])
" "${DATA}/tenants/${ALICE}.sqlite")"
check "…and everything alice has is shipped" "21" "$FINAL_CURSOR"

# Nothing lost and nothing duplicated: every sequence number 1..21 is in the
# zone exactly once, decrypted through the shipped CLI.
: >"${WORK}/all-seqs.txt"
while read -r key; do
	case "$key" in
	*"database_id=${ALICE}/"*) ;;
	*) continue ;;
	esac
	# </dev/null so podman cannot eat the loop's own input.
	ship_job decrypt --data-dir /data --key "$key" --json 2>/dev/null </dev/null |
		sed -n 's/.*"seq":\([0-9]*\).*/\1/p' >>"${WORK}/all-seqs.txt"
done < <(zone_ls)
check "every event reached the zone exactly once" "$(seq 1 21 | tr '\n' ' ')" \
	"$(sort -n "${WORK}/all-seqs.txt" | uniq | tr '\n' ' ')"
check "…with no duplicate rows among them" "21" "$(sort -n "${WORK}/all-seqs.txt" | wc -l)"

# ---------------------------------------------------------------------------
log "7. the wrapper's four exit statuses, through deploy/ship.sh"
# The real wrapper, pointed at this gate's image, data directory, key
# directory and lake.env. Production sets none of those overrides.
cat >"${WORK}/lake.env" <<EOF
PKDUMP_LAKE_S3_BUCKET=${BUCKET}
PKDUMP_LAKE_S3_REGION=us-west-2
PKDUMP_LAKE_S3_ENDPOINT=${MINIO_URL}
PKDUMP_TENANT_AWS_PROFILE=pkdump-tenant-test
EOF

wrapper() { # wrapper [args...] -> exit status, log in $WORK/wrapper.log
	set +e
	env PKDUMP_LAKE_ENV="${WORK}/lake.env" \
		PKDUMP_ALERTS_ENV=/nonexistent/alerts.env \
		PKDUMP_KEYS_CONF_DIR="$KEYS" \
		PKDUMP_SHIP_IMAGE="$IMAGE" \
		PKDUMP_SHIP_DATA="$DATA" \
		AWS_ACCESS_KEY_ID="$TEN_AK" AWS_SECRET_ACCESS_KEY="$TEN_SK" \
		PKDUMP_SHIP_NETWORK="$NET" \
		bash "${REPO_DIR}/deploy/ship.sh" gate "$@" >"${WORK}/wrapper.log" 2>&1
	local rc=$?
	set -e
	return "$rc"
}

RC=0
wrapper || RC=$?
check "nothing left to ship -> 0" "0" "$RC"
check "…and it says so" "yes" \
	"$(grep -q 'ship: OK' "${WORK}/wrapper.log" && echo yes || echo no)"

# A tenant whose key nobody registered: skipped, warned, exit 2.
python3 - "${DATA}/registry.sqlite" "$BOB" <<-'PY'
	import sqlite3, sys
	conn = sqlite3.connect(sys.argv[1])
	conn.execute("DELETE FROM tenant_key WHERE database_id = ?", (sys.argv[2],))
	conn.commit()
PY
add_holdings "${DATA}/tenants/${BOB}.sqlite" 2 2026-08-15
add_holdings "${DATA}/tenants/${ALICE}.sqlite" 2 2026-08-15
RC=0
wrapper || RC=$?
check "a tenant with no key state -> 2" "2" "$RC"
check "…and the warning names who" "yes" \
	"$(grep -q "ship: PARTIAL — tenants skipped: ${BOB}" "${WORK}/wrapper.log" && echo yes || echo no)"

# A lost outbox event: exit 3, its own message, and a durable record.
add_holdings "${DATA}/tenants/${ALICE}.sqlite" 4 2026-08-16
python3 - "${DATA}/tenants/${ALICE}.sqlite" <<-'PY'
	import sqlite3, sys
	conn = sqlite3.connect(sys.argv[1])
	cur = conn.execute("SELECT max(seq) FROM ownership_outbox").fetchone()[0]
	# Drop one row that has NOT been shipped: a lost event, which is what a
	# gap means. AUTOINCREMENT never reissues the number, so it is gone.
	conn.execute("DELETE FROM ownership_outbox WHERE seq = ?", (cur - 1,))
	conn.commit()
PY
RC=0
wrapper || RC=$?
check "a sequence gap -> 3" "3" "$RC"
check "…and it is alarmed as a gap, not as a partial" "yes" \
	"$(grep -q 'ship: SEQUENCE GAP' "${WORK}/wrapper.log" && echo yes || echo no)"
check "…recorded durably in the collection's ledger" "1" \
	"$(python3 -c "
import sqlite3,sys
print(sqlite3.connect(sys.argv[1]).execute('SELECT count(*) FROM ownership_outbox_gap').fetchone()[0])
" "${DATA}/tenants/${ALICE}.sqlite")"

# Nothing to ship to: the master key is gone. Every tenant skips, so the run
# shipped nobody — which is 1, not 2.
mv "${KEYS}/tenant-master.key" "${WORK}/tenant-master.key.away"
RC=0
wrapper || RC=$?
check "no master key -> 1 (the wrapper refuses before podman)" "1" "$RC"
check "…naming the file and the command" "yes" \
	"$(grep -q 'deploy/keys.sh gate init' "${WORK}/wrapper.log" && echo yes || echo no)"
mv "${WORK}/tenant-master.key.away" "${KEYS}/tenant-master.key"

# ---------------------------------------------------------------------------
log "8. THE HEADLINE PROOF, through the real zone (pd-880q)"
# pd-385w proved this claim in Rust over `outbox::project`, with the zone as an
# abstraction: throw every event away, rebuild by backfill, and the projection
# equals what the triggers produced one mutation at a time. `project` is the
# CONTRACT between the backfill and the shipper, so a proof stated over it is a
# proof about the reduction rather than about the zone.
#
# This is the same claim with the abstraction removed. Between the two sides
# now sit Parquet, AES-256-GCM under a derived key, a real bucket under the
# real tenant policy, and `pkdump-ship holdings` reading all of it back — four
# places a holding can be lost, duplicated or resolved to the wrong version,
# none of which the Rust tier can reach.
#
# carol is her own tenant rather than a fourth act on alice: §7 leaves alice
# carrying a deliberate sequence gap, and a gap is exactly the thing that would
# make a rebuilt zone legitimately differ.
#
# SEEN RED, the same way pd-385w's Rust proof was — a backfill that covered
# singles and silently missed sealed. Reproduce by adding one line after the
# emit in §8c and re-running:
#
#   carol_exec "DELETE FROM ownership_outbox \
#                WHERE source = 'backfill' AND source_table = 'sealed_collection'"
#
# §8d then fails naming the sealed row that did not come back, and §8c's counts
# fail beside it. What must NOT happen is what a singles-only fixture would
# give you: a green run.
pkdump_cli tenant create carol >"${WORK}/carol.txt" || die "tenant create carol failed"
CAROL="$(sed -n 's/.*-> database \([A-Z0-9]*\) at.*/\1/p' "${WORK}/carol.txt")"
[[ -n "$CAROL" ]] || die "could not read carol's database id back"
pkdump_cli keys register "$CAROL" >/dev/null || die "keys register carol failed"
CAROL_DB="${DATA}/tenants/${CAROL}.sqlite"

# One day's mutations at a time, with the outbox's own triggers doing every
# write — the fixture needs no application code, which is the whole point of
# pd-5m54. The shape is `a_collection_with_history` from the Rust proof: three
# singles of which one is edited, one sold and one deleted outright, and two
# sealed lots of which one is edited and one deleted.
#
# The surviving sealed lot deliberately shares row id 1 with a surviving
# single. `collection` and `sealed_collection` number their rows independently
# (pd-4gop), so a reduction keyed on `row_id` alone silently merges the two —
# and a fixture of singles alone would let a backfill that skipped sealed pass
# this whole section.
carol_stage() { # carol_stage <date> <acquire|edit|retire>
	python3 - "$CAROL_DB" "$1" "$2" <<-'PY'
		import sqlite3, sys
		db, date, stage = sys.argv[1], sys.argv[2], sys.argv[3]
		conn = sqlite3.connect(db)
		before = conn.execute("SELECT coalesce(max(seq),0) FROM ownership_outbox").fetchone()[0]
		if stage == "acquire":
		    for i in range(3):
		        conn.execute(
		            "INSERT INTO collection (printing_id, acquired_at, source)"
		            " VALUES (?, ?, 'gate')",
		            (f"carol-single-{i}", f"{date}T00:00:00Z"),
		        )
		    for i in range(2):
		        conn.execute(
		            "INSERT INTO sealed_collection (product_id, quantity, added_at, source)"
		            " VALUES (?, 1, ?, 'gate')",
		            (7000 + i, f"{date}T00:00:00Z"),
		        )
		elif stage == "edit":
		    conn.execute("UPDATE collection SET condition = 'Lightly Played' WHERE id = 1")
		    conn.execute("UPDATE sealed_collection SET quantity = 3 WHERE id = 1")
		elif stage == "retire":
		    conn.execute("UPDATE collection SET status = 'sold', sale_price = 12.5 WHERE id = 2")
		    conn.execute("DELETE FROM collection WHERE id = 3")
		    conn.execute("DELETE FROM sealed_collection WHERE id = 2")
		else:
		    raise SystemExit(f"unknown stage {stage}")
		# The trigger stamps occurred_at from the clock and `as_of=` is derived
		# from it, so dating each batch is what puts this history into more than
		# one partition — exactly as three days of real use would have.
		conn.execute(
		    "UPDATE ownership_outbox SET occurred_at = ? || substr(occurred_at, 11) WHERE seq > ?",
		    (date, before),
		)
		conn.commit()
		conn.close()
	PY
}

# The staged zone, both tables, in a form two runs can be compared as text.
# EVERY column, because the failure this is looking for is a row that came back
# at the wrong VERSION — a condition or a quantity one event stale is invisible
# to a count.
carol_holdings() {
	python3 - "$CAROL_DB" <<-'PY'
		import sqlite3, sys
		conn = sqlite3.connect(sys.argv[1])
		for table in ("zone_holdings", "zone_sealed_holdings"):
		    cols = [r[1] for r in conn.execute(f"PRAGMA table_info({table})")]
		    if not cols:
		        print(table, "ABSENT", sep="\t")
		        continue
		    for row in conn.execute(f"SELECT {', '.join(cols)} FROM {table} ORDER BY id"):
		        print(table, row[0], "|".join(repr(v) for v in row), sep="\t")
	PY
}

carol_sql() { # carol_sql <scalar query>
	python3 - "$CAROL_DB" "$1" <<-'PY'
		import sqlite3, sys
		print(sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()[0])
	PY
}

carol_exec() { # carol_exec <statement>
	python3 - "$CAROL_DB" "$1" <<-'PY'
		import sqlite3, sys
		conn = sqlite3.connect(sys.argv[1])
		conn.execute(sys.argv[2])
		conn.commit()
	PY
}

carol_zone_keys() { zone_ls | grep "database_id=${CAROL}/" || true; }
rows_in() { # rows_in <snapshot> <staging table> -> how many rows it holds
	awk -F'\t' -v t="$2" '$1 == t' <<<"$1" | wc -l
}

# One tenant, so §7's deliberate gap on alice cannot reach any of this.
carol_ship() { # carol_ship <log name>
	ship_job run --data-dir /data --tenant carol >"${WORK}/$1" 2>&1 ||
		{
			cat "${WORK}/$1"
			die "shipping carol failed ($1)"
		}
}

carol_stage 2026-08-20 acquire
carol_ship carol-ship-1.log
carol_stage 2026-08-21 edit
carol_ship carol-ship-2.log
carol_stage 2026-08-22 retire
carol_ship carol-ship-3.log

INCREMENTAL_KEYS="$(carol_zone_keys)"
echo "$INCREMENTAL_KEYS" | sed 's/^/    /'
check "three days of mutations are three parts in the zone" "3" \
	"$(grep -c . <<<"$INCREMENTAL_KEYS" || true)"
check "…carrying every event the triggers wrote" "10" \
	"$(carol_sql 'SELECT count(*) FROM ownership_outbox')"
check "…and all of it shipped" "10" \
	"$(carol_sql 'SELECT shipped_thru FROM ownership_outbox_cursor')"

ship_job holdings --data-dir /data --tenant carol >"${WORK}/carol-read-1.log" 2>&1 ||
	{
		cat "${WORK}/carol-read-1.log"
		die "the first read-back failed"
	}
sed 's/^/  | /' "${WORK}/carol-read-1.log"
INCREMENTAL="$(carol_holdings)"
echo "$INCREMENTAL" | sed 's/^/    /'
check "the zone's cards are the two singles that survived" "2" "$(rows_in "$INCREMENTAL" zone_holdings)"
check "…and its sealed lots the one that survived" "1" "$(rows_in "$INCREMENTAL" zone_sealed_holdings)"
check "…which is what carol's collection itself holds" "2 1" \
	"$(carol_sql 'SELECT count(*) FROM collection') $(carol_sql 'SELECT count(*) FROM sealed_collection')"
# The pair, in the real zone: two survivors that share a number and are not the
# same holding. A reduction keyed on `row_id` alone brings back one of them.
check "a surviving single and a surviving sealed lot share row id 1" "2" \
	"$(awk -F'\t' '$2 == "1"' <<<"$INCREMENTAL" | wc -l)"

# ---------------------------------------------------------------------------
log "8b. delete carol's partition, and prove the read-back reads the BUCKET"
# `zone_holdings` is written into carol's own database, beside `collection`.
# Emptied of objects, the zone must materialise NOTHING: a reader that had
# quietly fallen back to the live table looks identical to a correct one right
# up until here, and Phase 3 would go on reporting beautiful numbers.
mc_root rm --recursive --force "x/${BUCKET}/tenant/database_id=${CAROL}/" >/dev/null 2>&1 || true
check "carol's partition is gone from the zone" "0" "$(carol_zone_keys | grep -c . || true)"
check "…and nobody else's went with it" "yes" \
	"$([[ "$(zone_ls | grep -c "database_id=${ALICE}/" || true)" -gt 0 ]] && echo yes || echo no)"

ship_job holdings --data-dir /data --tenant carol >"${WORK}/carol-read-2.log" 2>&1 ||
	{
		cat "${WORK}/carol-read-2.log"
		die "the read-back over an emptied zone failed"
	}
check "an emptied zone materialises no holdings at all" "" "$(carol_holdings)"
check "…while the collection it is written beside is untouched" "2 1" \
	"$(carol_sql 'SELECT count(*) FROM collection') $(carol_sql 'SELECT count(*) FROM sealed_collection')"

# ---------------------------------------------------------------------------
log "8c. throw every outbox event away and rebuild by backfill"
# What "delete the zone" means on the inbound side: the objects went above, and
# the events they were made of go here. The cursor deliberately STAYS — a
# backfill is what an existing box runs against a live outbox, and its events
# are the next sequence numbers after everything already shipped, never a
# reset. Nothing here is a gap: every deleted row is at or below the cursor.
carol_exec 'DELETE FROM ownership_outbox'
check "the outbox is empty" "0" "$(carol_sql 'SELECT count(*) FROM ownership_outbox')"

pkdump_cli outbox emit --all --tenant carol >"${WORK}/carol-emit.log" 2>&1 ||
	{
		cat "${WORK}/carol-emit.log"
		die "the backfill failed"
	}
sed 's/^/  | /' "${WORK}/carol-emit.log"
# Per source, out of the run's own output: a backfill that covered singles and
# missed sealed is visible the night it runs rather than in a later
# reconciliation.
check "the backfill covers both sources" "yes" \
	"$(grep -q 'collection 2, sealed_collection 1' "${WORK}/carol-emit.log" && echo yes || echo no)"
check "…as three events where incremental shipping wrote ten" "3" \
	"$(carol_sql 'SELECT count(*) FROM ownership_outbox')"
check "…every one of them carrying its provenance" "3" \
	"$(carol_sql "SELECT count(*) FROM ownership_outbox WHERE source = 'backfill'")"

carol_ship carol-ship-4.log
REBUILT_KEYS="$(carol_zone_keys)"
echo "$REBUILT_KEYS" | sed 's/^/    /'
check "the rebuilt zone is objects, not an empty prefix" "yes" \
	"$([[ -n "$REBUILT_KEYS" ]] && echo yes || echo no)"
# A part is named for the sequence range it carries, and the backfill's range
# is its own — so this is a zone genuinely rebuilt rather than the old objects
# resurfacing from somewhere.
check "…and none of them is an object the incremental run wrote" "0" \
	"$(comm -12 <(sort <<<"$INCREMENTAL_KEYS") <(sort <<<"$REBUILT_KEYS") | grep -c . || true)"

# Rule 2 of the design, from the reader's side: the shipper CARRIES `source`
# and does not branch on it. Carrying is what makes "did last night's backfill
# actually reach the zone" answerable at all; branching is what would make the
# backfill a second code path, untested until the night it is needed.
ship_job decrypt --data-dir /data --key "$(head -1 <<<"$REBUILT_KEYS")" --json \
	>"${WORK}/carol-backfilled.jsonl" 2>&1 ||
	{
		cat "${WORK}/carol-backfilled.jsonl"
		die "decrypting the rebuilt part failed"
	}
check "the shipped part carries the backfill's provenance" "3" \
	"$(grep -c '"source":"backfill"' "${WORK}/carol-backfilled.jsonl" || true)"

# ---------------------------------------------------------------------------
log "8d. THE ACCEPTANCE BAR: the rebuilt zone equals the one shipping built"
ship_job holdings --data-dir /data --tenant carol >"${WORK}/carol-read-3.log" 2>&1 ||
	{
		cat "${WORK}/carol-read-3.log"
		die "the read-back of the rebuilt zone failed"
	}
sed 's/^/  | /' "${WORK}/carol-read-3.log"
REBUILT="$(carol_holdings)"
# A comparison against nothing agrees with everything. §8 has already counted
# these rows, and this says so once more where a reader of a green run can see
# it — §8b deliberately drives the same snapshot to empty, and an ordering
# mistake would leave that emptiness here.
[[ -n "$INCREMENTAL" ]] ||
	die "the incremental snapshot is empty — 8d would compare nothing and call it agreement"
diff <(printf '%s\n' "$INCREMENTAL") <(printf '%s\n' "$REBUILT") >"${WORK}/carol-diff.txt" 2>&1 || true
check "the rebuilt zone holds what incremental shipping produced, row for row" "" \
	"$(cat "${WORK}/carol-diff.txt")"

# ---------------------------------------------------------------------------
log "9. the catalog zone is untouched by all of it"
check "the seeded raw/ object is still the only thing under raw/" "1" \
	"$(mc_root ls --recursive "x/${BUCKET}/raw/" 2>/dev/null | grep -c . || true)"

echo
echo "==> tests/lake/shipper.sh: ${pass} passed, ${fail} failed"
[[ "$fail" -eq 0 ]] || exit 1
