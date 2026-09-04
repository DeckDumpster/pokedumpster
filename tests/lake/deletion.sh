#!/usr/bin/env bash
# Container-tier gate (pd-qbrf): the DELETION PATH, against a real object
# store, under the real tenant credential policy, on a VERSIONED bucket.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/deletion.sh          # ~2min after the image is warm
#   KEEP=1 bash tests/lake/deletion.sh   # leave MinIO + WORK up for poking
#
# ── WHAT THIS EXISTS TO CATCH, THAT THE RUST TIER CANNOT ────────────────────
# crates/pkdump-erase/tests/deletion.rs already proves the claim over really
# shipped holdings — the drop, the tombstone, the stray copy, and every one of
# those checks seen RED a moment before the deletion. What it cannot prove is
# everything that is only true of the deployment:
#
#   §2  the SHIPPED IMAGE carries pkdump-erase at all. A binary left out of the
#       Containerfile passes every cargo test there is.
#   §5  the delete and the listing are actually PERMITTED by the tenant IAM
#       policy. `DirStore` cannot fail a policy, and s3:DeleteObject being in
#       tenant-credentials.json is not the same as it working.
#   §5  the objects are gone as seen by the BUCKET ROOT, not merely invisible
#       to the credential that deleted them. A permissions illusion looks
#       exactly like a deletion from inside.
#   §6  a VERSIONED bucket, where the drop leaves a noncurrent version behind.
#       This is the case the whole crypto-shredding layer exists for and the
#       one no `DirStore` has: a real surviving copy of a real shipped part,
#       fetched back out, proven unopenable — and proven to have been openable
#       one step earlier.
#   §7  deploy/erase.sh maps the job's three exit statuses to the three things
#       an operator needs to hear, including 4, which nothing else reports.
#
# ── NO REAL TENANT DATA ─────────────────────────────────────────────────────
# Every holding here is an invented printing id in a throwaway collection, in a
# throwaway bucket, on a throwaway network. This item's whole job is proving a
# deletion works, which would be worth nothing demonstrated on anybody's real
# holdings. §8 tears the bucket down.
#
# Prod-safe: its own podman network, its own MinIO, its own bucket, its own
# image tag, its own data directory. It touches no pkdump-* unit, no
# pkdump-*-data volume, no real bucket, no real tenant database, and never the
# operator's master key — PKDUMP_KEYS_CONF_DIR points the wrapper at a temp
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
# shellcheck source=tests/lib/objects.sh
. "${REPO_DIR}/tests/lib/objects.sh"
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout — deploy/ci.sh runs gates in parallel and several
# polecats run whole suites beside each other from their own worktrees.
SUFFIX="${PDERASE_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
NET="pderase-net-${SUFFIX}"
MINIO_CTR="pderase-minio-${SUFFIX}"
BUCKET="pderase-${SUFFIX}"
IMAGE="localhost/pkdump:erase-${SUFFIX}"
MINIO_PORT=${MINIO_PORT:-$(free_port)}

ROOT_AK=pderaseroot
ROOT_SK=pderaserootsecret123
CAT_AK=pderasecatalog
CAT_SK=pderasecatalogsecret123
TEN_AK=pderasetenant
TEN_SK=pderasetenantsecret123

WORK=${WORK:-$(mktemp -d /tmp/pderase.XXXXXX)}
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
		echo "  bucket  ${BUCKET}   (versioning ENABLED)"
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

# mc as an arbitrary identity — the alias is always `x`, so a red run is the
# SAME command answering differently rather than a differently-spelled one.
mc_as() { # mc_as <access-key> <secret> [args...]
	local ak="$1" sk="$2"
	shift 2
	podman run --rm --network "$NET" \
		-v "$WORK/policies:/policies:ro,Z" \
		-e "MC_HOST_x=http://${ak}:${sk}@${MINIO_CTR}:9000" \
		"$MC_IMAGE" "$@"
}
mc_root() { mc_as "$ROOT_AK" "$ROOT_SK" "$@"; }

# The shipped image, as any offline job runs it. Extra mounts (the stray copy)
# come from PDERASE_MOUNTS, set by the one call that needs them.
offline_job() { # offline_job <entrypoint> [args...]
	local entrypoint="$1"
	shift
	podman run --rm --network "$NET" --pull=never \
		-v "${DATA}:/data:Z" \
		-v "${KEYS}/tenant-master.key:/keys/tenant-master.key:ro,Z" \
		${PDERASE_MOUNTS[@]+"${PDERASE_MOUNTS[@]}"} \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		-e PKDUMP_LAKE_S3_BUCKET="$BUCKET" \
		-e PKDUMP_LAKE_S3_REGION=us-west-2 \
		-e PKDUMP_LAKE_S3_ENDPOINT="$MINIO_URL" \
		-e PKDUMP_TENANT_AWS_PROFILE=pkdump-tenant-test \
		-e AWS_ACCESS_KEY_ID="$TEN_AK" -e AWS_SECRET_ACCESS_KEY="$TEN_SK" \
		--entrypoint "$entrypoint" "$IMAGE" "$@"
}
PDERASE_MOUNTS=()
ship_job() { offline_job pkdump-ship "$@"; }
erase_job() { offline_job pkdump-erase "$@"; }

# `pkdump` itself, for provisioning. No bucket: this is the ONLINE binary and
# it has no business holding a tenant-zone credential.
pkdump_cli() { # pkdump_cli [args...]
	podman run --rm --pull=never \
		-v "${DATA}:/data:Z" \
		-v "${KEYS}:/keys:rw,Z" \
		-e PKDUMP_HOME=/data \
		-e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
		--entrypoint pkdump "$IMAGE" "$@"
}

# Insert holdings straight into a collection with host python3. The triggers in
# the file are what write the outbox (pd-5m54), so this fixture needs no
# application code at all.
add_holdings() { # add_holdings <db> <n> <date>
	python3 - "$1" "$2" "$3" <<-'PY'
		import sqlite3, sys
		db, n, date = sys.argv[1], int(sys.argv[2]), sys.argv[3]
		conn = sqlite3.connect(db)
		before = conn.execute("SELECT coalesce(max(seq),0) FROM ownership_outbox").fetchone()[0]
		for i in range(n):
		    conn.execute(
		        "INSERT INTO collection (printing_id, acquired_at, source) VALUES (?, ?, 'gate')",
		        (f"invented-{date}-{i}", f"{date}T00:00:00Z"),
		    )
		conn.execute(
		    "UPDATE ownership_outbox SET occurred_at = ? || substr(occurred_at, 11) WHERE seq > ?",
		    (date, before),
		)
		conn.commit()
		conn.close()
	PY
}

# The catalog-zone object §0 seeds, and the sentinel every listing below is
# checked against: it is in this bucket from §0 until §8 asserts it is still
# there, so a whole-bucket listing that does not carry it did not see the
# bucket. See tests/lib/objects.sh.
SENTINEL_KEY="raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-14/run=01GATE/part-0000.json"

# Listed from the BUCKET ROOT as root, so what is asserted is what is THERE and
# not what a particular credential can see. A deletion that only hid the
# objects from the role that deleted them would pass any check made from
# inside.
#
# Refreshing is a STATEMENT rather than something a `check` line does inside a
# command substitution, and that is pd-cxq4's fix rather than a style choice: a
# listing that cannot be trusted has to be able to end the run, and an `exit`
# inside `$( … )` ends only the subshell. So the listing happens here, at the
# top level, and the checks below read what it found. Half of them expect ZERO
# — alice's prefix after the drop, nothing put back under it — which is exactly
# what a listing that quietly died would also produce.
ZONE_KEYS=""
CATALOG_KEYS=""
zone_refresh() {
	local listing keys
	listing="$(object_store_ls "$SENTINEL_KEY" mc_root ls --recursive "x/${BUCKET}")" ||
		die "the bucket listing could not be trusted — see above"
	keys="$(awk '{print $NF}' <<<"$listing")"
	ZONE_KEYS="$(grep '^tenant/' <<<"$keys" | sort || true)"
	CATALOG_KEYS="$(grep -E '^(raw|lake)/' <<<"$keys" | sort || true)"
}
zone_ls() { printf '%s\n' "$ZONE_KEYS"; }

# ---------------------------------------------------------------------------
log "0. MinIO, a VERSIONED bucket, and the two identities from the REAL policies"
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

# VERSIONING ON, deliberately, and it is the harder configuration on purpose.
# A partition drop on a versioned bucket does not remove bytes — it writes a
# delete marker and the old version becomes noncurrent. That is exactly the
# "stray copy or old snapshot" the design says the tombstone, not the drop, is
# what handles. §6 makes it real instead of hypothetical.
mc_root version enable "x/${BUCKET}" >/dev/null || die "could not enable versioning"

# The catalog zone, so §8's untouched-ness is about something that exists.
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
policy_install pderase-catalog catalog-credentials.json
policy_install pderase-tenant tenant-credentials.json
mc_root admin policy attach x pderase-catalog --user "$CAT_AK" >/dev/null 2>&1 ||
	die "could not attach the catalog policy"
mc_root admin policy attach x pderase-tenant --user "$TEN_AK" >/dev/null 2>&1 ||
	die "could not attach the tenant policy"
echo "  bucket ${BUCKET} (versioned); ${TEN_AK} carries tenant-credentials.json verbatim"

# ---------------------------------------------------------------------------
log "1. build the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >"${WORK}/build.log" 2>&1 ||
	{
		tail -40 "${WORK}/build.log"
		die "the image build failed"
	}

log "2. the image carries the deleter"
# A binary left out of the Containerfile passes every cargo test there is.
check "pkdump-erase is in the image" "ok" \
	"$(podman run --rm --pull=never --entrypoint pkdump-erase "$IMAGE" --help >/dev/null 2>&1 && echo ok || echo missing)"

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

add_holdings "${DATA}/tenants/${ALICE}.sqlite" 5 2026-08-13
add_holdings "${DATA}/tenants/${ALICE}.sqlite" 4 2026-08-14
add_holdings "${DATA}/tenants/${BOB}.sqlite" 3 2026-08-14

ship_job run --data-dir /data >"${WORK}/ship.log" 2>&1 ||
	{
		cat "${WORK}/ship.log"
		die "the shipper failed"
	}
zone_refresh
check "objects in the zone before any deletion" "3" "$(zone_ls | grep -c . || true)"
ALICE_KEYS="$(zone_ls | grep "database_id=${ALICE}/" || true)"
check "…two of them alice's" "2" "$(grep -c . <<<"$ALICE_KEYS")"

# A copy of a real shipped part, taken BEFORE the deletion — the evidence §5
# and §6 are about. mc runs without a TTY, so this is byte-exact.
STRAY_KEY="$(head -1 <<<"$ALICE_KEYS")"
mc_root cat "x/${BUCKET}/${STRAY_KEY}" >"${WORK}/stray.enc" 2>/dev/null ||
	die "could not take a copy of ${STRAY_KEY}"
check "the copy is a sealed object, not plaintext Parquet" "sealed" \
	"$(head -c 4 "${WORK}/stray.enc" | grep -q PAR1 && echo plaintext || echo sealed)"
check "…and no invented printing id is readable in it" "opaque" \
	"$(grep -qa 'invented-2026-08-13' "${WORK}/stray.enc" && echo readable || echo opaque)"

# ---------------------------------------------------------------------------
log "4. SEEN RED: before the deletion, every path is OPEN and verify says 4"
# A check that has only ever been seen passing is not known to check anything.
# This is the same command, one step earlier, and it must refuse to call alice
# deleted.
PDERASE_MOUNTS=(-v "${WORK}/stray.enc:/stray/copy.enc:ro,Z")
RC=0
erase_job verify --data-dir /data --tenant "$ALICE" \
	--stray /stray/copy.enc --stray-key "$STRAY_KEY" >"${WORK}/before.log" 2>&1 || RC=$?
check "verify on a LIVE tenant exits 4, not 0" "4" "$RC"
check "…the machinery is reported healthy" "1" \
	"$(grep -c '^ *ok  *machinery' "${WORK}/before.log" || true)"
check "…derivation is OPEN" "1" \
	"$(grep -c '^ *OPEN  *derivation' "${WORK}/before.log" || true)"
check "…the partition is OPEN" "1" \
	"$(grep -c '^ *OPEN  *partition' "${WORK}/before.log" || true)"
check "…and the stray copy actually OPENED rather than being inferred" "yes" \
	"$(grep -q 'OPENED' "${WORK}/before.log" && echo yes || echo no)"
zone_refresh
check "…nothing was deleted by asking" "2" \
	"$(zone_ls | grep -c "database_id=${ALICE}/" || true)"

# ---------------------------------------------------------------------------
log "5. the deletion, through the shipped binary under the TENANT policy"
RC=0
erase_job delete --data-dir /data --tenant "$ALICE" --yes --reason "account closed" \
	--stray /stray/copy.enc --stray-key "$STRAY_KEY" >"${WORK}/delete.log" 2>&1 || RC=$?
PDERASE_MOUNTS=()
if [[ "$RC" -ne 0 ]]; then
	cat "${WORK}/delete.log"
	die "the deletion exited ${RC}"
fi
check "the deletion exits 0" "0" "$RC"
check "…and reports the tombstone first" "1" \
	"$(grep -c '1\. key REVOKED' "${WORK}/delete.log" || true)"
check "…having removed both of alice's objects" "1" \
	"$(grep -c '2\. partition .* — 2 object(s) removed' "${WORK}/delete.log" || true)"

# Seen from the BUCKET ROOT, which is the assertion that matters: the objects
# are gone, not merely invisible to the credential that deleted them.
zone_refresh
check "alice's prefix is empty as seen by root" "0" \
	"$(zone_ls | grep -c "database_id=${ALICE}/" || true)"
check "bob's data is untouched" "1" \
	"$(zone_ls | grep -c "database_id=${BOB}/" || true)"

# The tenant role really performed a delete and a listing — this is what the
# Rust tier cannot show, because a DirStore has no policy to fail.
check "the tenant policy permitted the delete (nothing was skipped)" "no" \
	"$(grep -qi 'AccessDenied\|access denied' "${WORK}/delete.log" && echo yes || echo no)"

# Key custody agrees, through a different binary reading the same registry.
pkdump_cli keys list >"${WORK}/keys.txt" 2>&1 || die "keys list failed"
check "alice's key is tombstoned" "1" \
	"$(grep -c "^${ALICE} *tombstoned" "${WORK}/keys.txt" || true)"
check "…and bob's is still active" "1" \
	"$(grep -c "^${BOB} *active" "${WORK}/keys.txt" || true)"

# Bob is a control for the whole thing: the same command must still call him
# NOT deleted.
RC=0
erase_job verify --data-dir /data --tenant "$BOB" >"${WORK}/bob-verify.log" 2>&1 || RC=$?
check "bob still verifies as NOT deleted" "4" "$RC"

# ---------------------------------------------------------------------------
log "6. the VERSION that survived the drop is ciphertext nobody holds a key for"
# The case the crypto-shredding layer exists for, and the one no DirStore has.
# The bucket is versioned, so the drop wrote a delete marker and alice's part is
# still there as a noncurrent version. It is a real surviving copy of real
# shipped holdings.
#
# Read as JSON, not as columns. `mc ls --versions` puts the version id, the
# `v<N>` ordinal and PUT/DELETEMARKER in a human layout that is not a contract;
# picking a field out of it by position is how a gate starts silently matching
# the wrong thing.
object_store_ls "$SENTINEL_KEY" \
	mc_root ls --versions --recursive --json "x/${BUCKET}" >"${WORK}/versions.json" ||
	die "the versioned listing could not be trusted — see above"

# The surviving (non-delete-marker) version of the exact object §3 copied.
surviving_version_of() { # surviving_version_of <key>
	python3 - "${WORK}/versions.json" "$1" <<-'PY'
		import json, sys
		path, want = sys.argv[1], sys.argv[2]
		for line in open(path):
		    line = line.strip()
		    if not line:
		        continue
		    try:
		        row = json.loads(line)
		    except json.JSONDecodeError:
		        continue
		    if row.get("isDeleteMarker"):
		        continue
		    key = row.get("key", "")
		    # mc prints keys relative to the prefix it was given; this listing
		    # is from the bucket root, so they should be whole — accept a
		    # suffix match anyway rather than depend on that.
		    if key != want and not want.endswith(key):
		        continue
		    if row.get("versionId"):
		        print(row["versionId"])
		        break
	PY
}

check "a noncurrent version of alice's part survived the drop" "yes" \
	"$(grep -q "database_id=${ALICE}/" "${WORK}/versions.json" && echo yes || echo no)"

VERSION_ID="$(surviving_version_of "$STRAY_KEY")"
[[ -n "$VERSION_ID" ]] || die "could not find the surviving version id of ${STRAY_KEY}"
mc_root cat --version-id "$VERSION_ID" "x/${BUCKET}/${STRAY_KEY}" \
	>"${WORK}/survivor.enc" 2>/dev/null || die "could not fetch the surviving version"

check "the survivor is byte-identical to what was shipped" "same" \
	"$(cmp -s "${WORK}/stray.enc" "${WORK}/survivor.enc" && echo same || echo different)"
check "…and it is still ciphertext" "opaque" \
	"$(grep -qa 'invented-2026-08-13' "${WORK}/survivor.enc" && echo readable || echo opaque)"

# The claim, made against those exact surviving bytes rather than against the
# copy taken earlier: no key can be derived, so nothing opens it.
PDERASE_MOUNTS=(-v "${WORK}/survivor.enc:/stray/copy.enc:ro,Z")
RC=0
erase_job verify --data-dir /data --tenant "$ALICE" \
	--stray /stray/copy.enc --stray-key "$STRAY_KEY" >"${WORK}/after.log" 2>&1 || RC=$?
PDERASE_MOUNTS=()
if [[ "$RC" -ne 0 ]]; then
	cat "${WORK}/after.log"
	die "the surviving version was not proven unreadable (exit ${RC})"
fi
check "the surviving version verifies as unreadable" "0" "$RC"
check "…because derivation is a deliberate revocation" "1" \
	"$(grep -c 'deliberate revocation' "${WORK}/after.log" || true)"
check "…and every check is CLOSED" "0" \
	"$(grep -c '^ *OPEN' "${WORK}/after.log" || true)"

# The shipper is the thing that would otherwise undo all of this. New holdings
# arrive in alice's collection — the online half has not been purged, which is
# the normal window — and must not reach the zone.
add_holdings "${DATA}/tenants/${ALICE}.sqlite" 4 2026-08-15
RC=0
ship_job run --data-dir /data >"${WORK}/ship2.log" 2>&1 || RC=$?
check "the shipper calls alice revoked rather than shipping her" "1" \
	"$(grep -c "revoked — not shipped, by design" "${WORK}/ship2.log" || true)"
zone_refresh
check "…and put nothing back under the dropped prefix" "0" \
	"$(zone_ls | grep -c "database_id=${ALICE}/" || true)"

# The one thing a tombstone must never do: come back.
RC=0
pkdump_cli keys register "$ALICE" >"${WORK}/reregister.log" 2>&1 || RC=$?
check "re-registering a tombstoned tenant is refused" "1" "$RC"

# ---------------------------------------------------------------------------
log "7. the wrapper's exit statuses, through deploy/erase.sh"
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
		PKDUMP_ERASE_IMAGE="$IMAGE" \
		PKDUMP_ERASE_DATA="$DATA" \
		AWS_ACCESS_KEY_ID="$TEN_AK" AWS_SECRET_ACCESS_KEY="$TEN_SK" \
		PKDUMP_ERASE_NETWORK="$NET" \
		bash "${REPO_DIR}/deploy/erase.sh" gate "$@" >"${WORK}/wrapper.log" 2>&1
	local rc=$?
	set -e
	return "$rc"
}

RC=0
wrapper verify --tenant "$ALICE" || RC=$?
check "an already-deleted tenant -> 0" "0" "$RC"
check "…and it says so" "yes" \
	"$(grep -q 'erase: PROVEN' "${WORK}/wrapper.log" && echo yes || echo no)"

RC=0
wrapper verify --tenant "$BOB" || RC=$?
check "a live tenant -> 4" "4" "$RC"
check "…named as NOT PROVEN, not as a failure" "yes" \
	"$(grep -q 'erase: NOT PROVEN' "${WORK}/wrapper.log" && echo yes || echo no)"
check "…naming which paths are open" "yes" \
	"$(grep -q 'erase: NOT PROVEN — .*derivation' "${WORK}/wrapper.log" && echo yes || echo no)"

# A delete without --yes changes nothing and is a refusal, not a deletion.
RC=0
wrapper delete --tenant "$BOB" || RC=$?
check "delete without --yes -> 1" "1" "$RC"
zone_refresh
check "…and bob's data is still there" "1" \
	"$(zone_ls | grep -c "database_id=${BOB}/" || true)"

# An unknown subcommand never reaches podman.
RC=0
wrapper obliterate --tenant "$BOB" || RC=$?
check "an unknown subcommand -> 1" "1" "$RC"

# No master key: the wrapper refuses before podman, because without one the
# verification could not conclude anything about anybody.
mv "${KEYS}/tenant-master.key" "${WORK}/tenant-master.key.away"
RC=0
wrapper verify --tenant "$BOB" || RC=$?
check "no master key -> 1 (the wrapper refuses before podman)" "1" "$RC"
check "…naming the file and the command" "yes" \
	"$(grep -q 'deploy/keys.sh gate restore' "${WORK}/wrapper.log" && echo yes || echo no)"
mv "${WORK}/tenant-master.key.away" "${KEYS}/tenant-master.key"

# ---------------------------------------------------------------------------
log "8. the catalog zone is untouched by all of it"
zone_refresh
check "the seeded raw/ object is still the only thing under raw/" "1" \
	"$(grep -c '^raw/' <<<"$CATALOG_KEYS" || true)"
check "bob's collection database is still on disk" "yes" \
	"$([[ -f "${DATA}/tenants/${BOB}.sqlite" ]] && echo yes || echo no)"
# The offline half only: the deletion is not what removes a collection.
check "alice's collection database is ALSO still there (that is tenant purge's job)" "yes" \
	"$([[ -f "${DATA}/tenants/${ALICE}.sqlite" ]] && echo yes || echo no)"

echo
echo "==> tests/lake/deletion.sh: ${pass} passed, ${fail} failed"
[[ "$fail" -eq 0 ]] || exit 1
