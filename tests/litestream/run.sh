#!/usr/bin/env bash
# Container-tier gate (pd-fof4, pd-nd6w): the SHIPPED Litestream config
# replicates every tenant AND the user registry, and the SHIPPED restore
# addressing brings back the right one.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/litestream/run.sh          # ~90s, full run + teardown
#   KEEP=1 bash tests/litestream/run.sh   # leave MinIO + WORK up for poking
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# The gate spike (pd-o98z, deep-dives/litestream-multi-db/) proved Litestream
# CAN replicate N databases from one process. This proves that OUR config and
# OUR scripts do, and keeps proving it — because the two ways this feature fails
# are both silent:
#
#   * A tenant's data restored under another tenant's name. Litestream accepts
#     two databases pointed at one replica path with no warning and no error;
#     restoring from the collided prefix returns the other tenant's rows and
#     PRAGMA integrity_check says ok (RESULT.md §2). Directory mode makes that
#     unreachable by deriving each prefix from a filename — §4 below asserts the
#     derivation actually holds, for a NON-FIRST tenant, because restoring
#     tenant #1 would pass even with an off-by-one.
#   * A config key that does nothing. Litestream does not reject unknown keys;
#     a misspelling is dropped in silence, and a `snapshot:` block written one
#     level too deep falls back to the 24h default instead of erroring
#     (RESULT.md §1). §6 checks the retention policy behaviourally, not by
#     reading our own file back.
#   * An empty replica prefix. Measured on v0.5.16 while adding the registry
#     entry (pd-nd6w): a replica whose `path:` expands to empty is accepted and
#     writes LTX files to the BUCKET ROOT, in silence. §4b asserts the bucket
#     root stays empty, and that an instance config predating the registry
#     entry makes the sidecar refuse to start rather than half-back-up.
#
# Prod-safe: its own podman network, its own MinIO, its own temp dir, its own
# throwaway SQLite files. Touches no pkdump-* unit, no pkdump-*-data volume, no
# real S3 bucket and no application code.
set -euo pipefail

# Bare `sqlite3` has NO busy timeout: it returns SQLITE_BUSY the moment the
# replicator holds the lock, and `set -e` takes the whole run down with it. This
# gate writes to databases while a Litestream container replicates them, so
# contention is not an edge case here — it is the scenario under test. Same
# helper, same 5s, as pd-znsf added on master for the tenant writes; the registry
# writes below need it for exactly the same reason.
sq() { sqlite3 -cmd '.timeout 5000' "$@"; }

LITESTREAM_IMAGE=${LITESTREAM_IMAGE:-docker.io/litestream/litestream:latest}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SHIPPED_YML="${REPO_DIR}/deploy/litestream.yml"

# The addressing under test is the one the deploy scripts use, sourced from the
# same file they source. A test that re-derived the replica URL itself would
# agree with a broken restore-litestream.sh.
# shellcheck source=deploy/litestream-lib.sh
. "${REPO_DIR}/deploy/litestream-lib.sh"

# Every name and port here is PER-CHECKOUT, for the reason deploy/ci.sh derives
# its container instance the same way: the swarm runs several polecats per rig,
# each from its own worktree, and every one of them runs deploy/ci.sh — which
# runs this. With fixed names, run B's opening `podman rm -f` and `volume rm`
# destroyed run A's MinIO and sidecar mid-suite, and A then failed somewhere
# unrelated-looking (a tenant "not picked up by watch:", an empty bucket
# listing). Observed 2026-08-08. Hashing the whole worktree path keeps the name
# stable per checkout — so the stale-cleanup below still finds THIS checkout's
# leftovers — while making a cross-checkout collision impossible.
SUFFIX="${PDLS_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
NET=pdls-test-net-$SUFFIX
MINIO_CTR=pdls-test-minio-$SUFFIX
LS_CTR=pdls-test-litestream-$SUFFIX
PROBE_CTR=pdls-test-probe-$SUFFIX
# Published on the host, so it needs a per-checkout port too — two MinIOs on one
# port is the same collision wearing a different hat. 39000-39499 for this
# script; tests/litestream/drill.sh takes the band above it.
MINIO_PORT=${MINIO_PORT:-$(( 39000 + 16#${SUFFIX:0:3} % 500 ))}
BUCKET=pdls-test
AKID=pdlstestroot
SECRET=pdlstestsecret123

# The instance env a real deploy would provide (deploy/setup.sh writes this
# shape into ~/.config/pkdump/<instance>/litestream.env), pointed at MinIO.
export LITESTREAM_TENANTS_DIR=/data/tenants
export LITESTREAM_S3_BUCKET=$BUCKET
export LITESTREAM_S3_PATH=citest/tenants
export LITESTREAM_REGISTRY_DB=/data/registry.sqlite
export LITESTREAM_S3_REGISTRY_PATH=citest/registry.sqlite
export LITESTREAM_S3_REGION=us-west-2
export LITESTREAM_S3_ENDPOINT="http://${MINIO_CTR}:9000"

WORK=${WORK:-$(mktemp -d /tmp/pdls-test.XXXXXX)}
mkdir -p "$WORK/data/tenants" "$WORK/minio" "$WORK/restore"

# FOUR tenants (alpha, bravo, charlie, delta) so "restore a non-first tenant"
# has a real middle to pick, and so a derivation that grabbed the first or the
# last prefix is visibly wrong.
VICTIM=bravo          # #2 of 4 — deliberately neither first nor last

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
		echo "KEEP=1 — leaving containers up and WORK=$WORK in place."
		echo "  podman rm -f $LS_CTR $MINIO_CTR; podman network rm $NET; rm -rf $WORK"
		return
	fi
	# --ignore matters: podman 4.9 given one name it cannot resolve removes
	# NOTHING and still exits 0, so a best-effort list without it leaves the
	# whole test bed running and the next run fails on "network already used".
	podman rm -f --ignore "$LS_CTR" "$MINIO_CTR" "$PROBE_CTR" >/dev/null 2>&1 || true
	podman network rm -f "$NET" >/dev/null 2>&1 || true
	rm -rf "$WORK"
}
trap cleanup EXIT

mc() { podman run --rm --network "$NET" -e "MC_HOST_s=http://$AKID:$SECRET@$MINIO_CTR:9000" "$MC_IMAGE" "$@"; }

# litestream, with the same credential wiring the deploy scripts use in prod
# (a shared-credentials file selected by AWS_PROFILE — in prod that profile
# assumes a role; here it holds MinIO's static test key).
ls_run() { podman run --rm --network "$NET" --user 0 \
	-v "$WORK/restore:/restore:Z" -v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	"$LITESTREAM_IMAGE" "$@"; }

# tenant_prefixes — the tenant replica prefixes actually present in the bucket.
#
# Scoped to LITESTREAM_S3_PATH rather than listing the whole bucket, because the
# bucket now holds more than tenants: the registry replicates to its own prefix
# BESIDE this one (pd-nd6w). That separation is the point — the total-loss
# procedure lists exactly this parent prefix to enumerate tenants — so the test
# reads it the same way the procedure does. §4b asserts the registry is outside.
#
# The `|| true` on the grep is load-bearing under `set -o pipefail`: grep exits 1
# when nothing matches, which would abort the whole suite from inside a command
# substitution — before a single RESULT line is printed. "No prefixes" is a
# result this test must be able to REPORT, not a reason to vanish.
tenant_prefixes() {
	mc ls -r "s/$BUCKET" 2>/dev/null | awk '{print $NF}' \
		| { grep "^${LITESTREAM_S3_PATH}/" || true; } | cut -d/ -f1-3 | sort -u
}

log "1. throwaway MinIO"
podman rm -f --ignore "$LS_CTR" "$MINIO_CTR" "$PROBE_CTR" >/dev/null 2>&1 || true
podman network rm -f "$NET" >/dev/null 2>&1 || true
podman network create "$NET" >/dev/null
podman run -d --name "$MINIO_CTR" --network "$NET" \
	-p "127.0.0.1:$MINIO_PORT:9000" \
	-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET" \
	-v "$WORK/minio:/data:Z" \
	"$MINIO_IMAGE" server /data >/dev/null
for _ in $(seq 60); do
	curl -fsS "http://127.0.0.1:$MINIO_PORT/minio/health/live" >/dev/null 2>&1 && break
	sleep 1
done
curl -fsS "http://127.0.0.1:$MINIO_PORT/minio/health/live" >/dev/null
mc mb --ignore-existing "s/$BUCKET" >/dev/null
mkdir -p "$WORK/aws"
printf '[pkdump]\naws_access_key_id=%s\naws_secret_access_key=%s\n' "$AKID" "$SECRET" >"$WORK/aws/credentials"
echo "  minio up on 127.0.0.1:$MINIO_PORT, bucket $BUCKET"

log "2. tenant databases under tenants/, one file each"
# Every row carries its tenant name, so a restore that returns the wrong
# tenant's data is caught by reading the data — not by trusting the prefix.
seed_tenant() {
	sqlite3 "$WORK/data/tenants/$1.sqlite" \
		"PRAGMA journal_mode=WAL;
		 CREATE TABLE collection (n INTEGER PRIMARY KEY, tenant TEXT, phase TEXT);
		 INSERT INTO collection (tenant, phase) VALUES ('$1','early');" >/dev/null
}
# The catalog sits BESIDE tenants/, never inside it: it is reproducible from
# upstream and must not be replicated. Put a decoy there to prove the glob
# cannot reach it.
sqlite3 "$WORK/data/shared.sqlite" "CREATE TABLE cards (id TEXT);" >/dev/null
# alpha/bravo/charlie exist before the sidecar starts; delta is created later,
# with the sidecar already running (§5).
for t in alpha bravo charlie; do seed_tenant "$t"; echo "  $t"; done

log "3. ONE sidecar, the SHIPPED deploy/litestream.yml, no per-tenant config"
podman run -d --name "$LS_CTR" --network "$NET" --user 0 \
	-v "$WORK/data:/data:Z" -v "$SHIPPED_YML:/etc/litestream.yml:ro,Z" \
	-v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_PATH \
	-e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_REGISTRY_PATH \
	-e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null
sleep 8

# A marker between two write phases, for the point-in-time restore in §4.
MARKER=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 2
for t in alpha bravo charlie; do
	sqlite3 "$WORK/data/tenants/$t.sqlite" \
		"INSERT INTO collection (tenant, phase) VALUES ('$t','late');" >/dev/null
done
sleep 8

check "sidecar is still running (one process, N databases)" "running" \
	"$(podman inspect -f '{{.State.Status}}' "$LS_CTR")"
check "no ERROR lines from the shipped config" "0" \
	"$(podman logs "$LS_CTR" 2>&1 | grep -c 'level=ERROR' || true)"

log "4. every tenant has its OWN derived prefix — and the shipped derivation agrees"
# The set of prefixes Litestream actually created, read back out of the bucket.
tenant_prefixes >"$WORK/prefixes.txt"
# Not just "three of them" — exactly the three the shipped derivation names. A
# collided config still produces a plausible-looking count; it does not produce
# a matching SET.
for t in alpha bravo charlie; do
	printf '%s\n' "$(tenant_replica_url "$t" | sed "s|^s3://${BUCKET}/||; s/?.*//")"
done | sort >"$WORK/expected-prefixes.txt"
check "the replica prefixes in the bucket are exactly the derived ones" "same" \
	"$(cmp -s "$WORK/prefixes.txt" "$WORK/expected-prefixes.txt" && echo same || echo different)"
check "the catalog is NOT replicated (it lives outside tenants/)" "0" \
	"$(grep -c 'shared.sqlite' "$WORK/prefixes.txt" || true)"

# THE assertion behind C2: what deploy/litestream-lib.sh will point a restore at
# is byte-for-byte the prefix the sidecar wrote — checked for a NON-FIRST tenant,
# since tenant #1 would agree even with an off-by-one.
VICTIM_PREFIX="${LITESTREAM_S3_PATH}/${VICTIM}.sqlite"
check "derived URL targets the prefix the sidecar wrote (tenant #2 of 4)" "1" \
	"$(grep -cx "${VICTIM_PREFIX}" "$WORK/prefixes.txt" || true)"
check "tenant_replica_url is that prefix" "s3://${BUCKET}/${VICTIM_PREFIX}" \
	"$(tenant_replica_url "$VICTIM" | sed 's/?.*//')"
# No two tenants can share a prefix, which is what makes a wrong-tenant restore
# unreachable rather than merely unlikely.
check "no two tenants share a prefix" "3" \
	"$(for t in alpha bravo charlie; do printf '%s\n' "$(tenant_replica_url "$t" | sed 's/?.*//')"; done | sort -u | wc -l)"

log "4b. the user registry rides the same sidecar, on its own prefix (pd-nd6w)"
# The registry did NOT exist when the sidecar started, and that is deliberate:
# registry.sqlite is created lazily on first use, so on every box today the
# `path:` entry points at a file that is not there. A `path:` entry tolerates
# that — it logs `initialized db` and waits — which is what makes it safe to
# ship this entry ahead of anything that writes a registry. If that ever stops
# being true, the sidecar dies at startup and §3 fails; this section proves the
# file is then actually picked up.
sq "$WORK/data/registry.sqlite" \
	"PRAGMA journal_mode=WAL;
	 CREATE TABLE user (handle TEXT PRIMARY KEY, database_id TEXT NOT NULL UNIQUE,
	                    created_at TEXT NOT NULL, state TEXT NOT NULL);
	 INSERT INTO user VALUES ('alpha','01k2c7hq8n0000000000alpha',datetime('now'),'active');" >/dev/null
# Poll, don't guess an interval — see the note in §5.
REG_LOGGED=0
reg_deadline=$(( SECONDS + 90 ))
while [ "$SECONDS" -lt "$reg_deadline" ]; do
	REG_LOGGED=$(podman logs "$LS_CTR" 2>&1 | grep -c 'db=registry.sqlite' || true)
	[ "$REG_LOGGED" -gt 0 ] && break
	sleep 2
done
check "the registry replicates from the SAME sidecar (one process, no restart)" "yes" \
	"$([ "$REG_LOGGED" -gt 0 ] && echo yes || echo no)"
check "sidecar still running with both entries" "running" \
	"$(podman inspect -f '{{.State.Status}}' "$LS_CTR")"

# A `path:` replica prefix is used VERBATIM — the file name is NOT appended the
# way directory mode appends it. registry_replica_url encodes that difference,
# and this is the assertion that keeps the two in step: get it wrong and a
# restore reads an empty prefix and reports "no snapshots" on the one database
# whose loss makes every other restore unattributable.
mc ls -r "s/$BUCKET" 2>/dev/null | awk '{print $NF}' | grep "^${LITESTREAM_S3_REGISTRY_PATH}/" \
	| head -1 >"$WORK/registry-key.txt" || true
check "the registry wrote LTX files under its own derived prefix" "1" \
	"$(grep -c . "$WORK/registry-key.txt" || true)"
check "registry_replica_url is that prefix" "s3://${BUCKET}/${LITESTREAM_S3_REGISTRY_PATH}" \
	"$(registry_replica_url | sed 's/?.*//')"

# It must sit BESIDE the tenants prefix, never inside it: the total-loss
# procedure lists that parent prefix to enumerate tenants, and a registry
# underneath it would come back as one more tenant to restore.
check "the registry prefix is NOT under the tenants prefix" "outside" \
	"$(case "$LITESTREAM_S3_REGISTRY_PATH" in "${LITESTREAM_S3_PATH%/}"/*) echo inside ;; *) echo outside ;; esac)"
check "and the tenant enumeration does not see it" "0" \
	"$(tenant_prefixes | grep -c 'registry' || true)"
# Nothing at the bucket ROOT. This is the shape the silent failure takes: an
# empty replica prefix is accepted and scatters LTX files at the top of the
# bucket, where they belong to no instance and no database. Every key must sit
# under a named prefix.
check "nothing was replicated to the bucket root" "0" \
	"$(mc ls -r "s/$BUCKET" 2>/dev/null | awk '{print $NF}' | grep -c '^[0-9]\{4\}/' || true)"

# It restores, as itself, addressed by that URL.
restore_registry() { # restore_registry <out> [flags...] — never aborts the report
	local out="$1"; shift
	ls_run restore -integrity-check full "$@" -o "/restore/${out}" "$(registry_replica_url || true)" >/dev/null 2>&1 || true
}
sq_registry() { sqlite3 "$WORK/restore/$1" "$2" 2>/dev/null || echo '<no restored db>'; }

restore_registry registry.sqlite
check "the registry restores and still holds its mapping" "alpha 01k2c7hq8n0000000000alpha" \
	"$(sq_registry registry.sqlite "SELECT handle || ' ' || database_id FROM user;")"

# PITR, demonstrated rather than inferred from "it shares the top-level snapshot
# block". The registry is where a bad `tenant rename` or a wrong detach lands,
# and recovering from one means asking for a moment before it — so the window
# has to actually work on THIS prefix, not just on the tenant prefixes.
REG_MARKER=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 2
sq "$WORK/data/registry.sqlite" \
	"INSERT INTO user VALUES ('bravo','01k2c7hq8n0000000000bravo',datetime('now'),'active');" >/dev/null
for _ in $(seq 30); do
	restore_registry registry-latest.sqlite
	[ "$(sq_registry registry-latest.sqlite 'SELECT count(*) FROM user;')" = 2 ] && break
	rm -f "$WORK/restore/registry-latest.sqlite"; sleep 2
done
check "a later registry write reaches the replica" "2" \
	"$(sq_registry registry-latest.sqlite 'SELECT count(*) FROM user;')"
restore_registry registry-pit.sqlite -timestamp "$REG_MARKER"
check "point-in-time restore rolls the registry back to the marker" "1" \
	"$(sq_registry registry-pit.sqlite 'SELECT count(*) FROM user;')"
check "and the row it keeps is the one that existed then" "alpha" \
	"$(sq_registry registry-pit.sqlite 'SELECT handle FROM user;')"

log "4c. an instance whose litestream.env predates the registry FAILS LOUDLY"
# The upgrade hazard, measured: an unset variable expands to empty, and
# Litestream's tolerance for that is not uniform. An empty REPLICA path is not
# fatal — it replicates to the bucket ROOT, silently. An empty DB path IS fatal.
# Because the registry entry needs both, an un-backfilled litestream.env cannot
# reach the silent case: the sidecar refuses to start, systemd marks it failed,
# and OnFailure pages. `deploy/setup.sh <instance>` backfills the keys.
#
# Run with LITESTREAM_REGISTRY_DB/LITESTREAM_S3_REGISTRY_PATH deliberately absent.
OLD_ENV_RC=0
podman run --rm --network "$NET" --user 0 \
	-v "$WORK/data:/data:Z" -v "$SHIPPED_YML:/etc/litestream.yml:ro,Z" \
	-v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_PATH \
	-e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >"$WORK/old-env.log" 2>&1 || OLD_ENV_RC=$?
check "the sidecar refuses to start on a pre-registry litestream.env" "yes" \
	"$([ "$OLD_ENV_RC" -ne 0 ] && echo yes || echo no)"
check "and says why, rather than backing up the tenants and shrugging" "yes" \
	"$(grep -qi "must specify either 'path' or 'dir'" "$WORK/old-env.log" && echo yes || echo no)"
# The derivation refuses too, so a restore or a freshness check cannot be talked
# into addressing an empty prefix — which is where the silent case would land.
check "registry_replica_url refuses an unset prefix" "rejected" \
	"$(LITESTREAM_S3_REGISTRY_PATH= bash -c '. "'"${REPO_DIR}"'/deploy/litestream-lib.sh"; registry_replica_url' >/dev/null 2>&1 && echo accepted || echo rejected)"

log "5. a tenant created AFTER startup replicates with no config edit, no restart"
seed_tenant delta
# WAIT for the pickup rather than sleeping a guessed interval and looking once.
# A fixed `sleep 12` here asserted "watch: reacts within 12 seconds on whatever
# machine this is", which is not the claim — the claim is that it reacts at all,
# with no config edit and no restart. Under deploy/ci.sh this box is also running
# an app container, a MinIO and an image build, and 12s was not always enough
# (observed 2026-08-08). Polling keeps the assertion and drops the guess.
DELTA_LOGGED=0
delta_deadline=$(( SECONDS + 90 ))
while [ "$SECONDS" -lt "$delta_deadline" ]; do
	DELTA_LOGGED=$(podman logs "$LS_CTR" 2>&1 | grep -c 'delta.sqlite' || true)
	[ "$DELTA_LOGGED" -gt 0 ] && break
	sleep 2
done
check "delta was picked up live by watch:" "yes" \
	"$([ "$DELTA_LOGGED" -gt 0 ] && echo yes || echo no)"
# If it never was, the sidecar's own account of why is the only useful evidence,
# and the containers are torn down on exit — so print it here or lose it.
if [ "$DELTA_LOGGED" -eq 0 ]; then
	echo "  --- last 20 lines from ${LS_CTR} ---"
	podman logs "$LS_CTR" 2>&1 | tail -20 | sed 's/^/  /'
	echo "  --- container state: $(podman inspect -f '{{.State.Status}}' "$LS_CTR" 2>&1) ---"
fi
# Replication of that pickup is a second, slower step; give it its own window.
for _ in $(seq 30); do
	tenant_prefixes | grep -qx "${LITESTREAM_S3_PATH}/delta.sqlite" && break
	sleep 2
done
tenant_prefixes >"$WORK/prefixes.txt"
check "delta has its own derived prefix" "1" \
	"$(grep -cx "${LITESTREAM_S3_PATH}/delta.sqlite" "$WORK/prefixes.txt" || true)"
check "four tenants, four prefixes" "4" "$(grep -c . "$WORK/prefixes.txt")"

log "6. restore the NON-FIRST tenant — latest, and point-in-time"
# Restoring tenant #1 would pass even if the derivation were off by one, so the
# whole point of this section is that the victim is #2 of 4.
restore_ok() { # restore_ok <out> <url> [flags...] — never aborts the report
	local out="$1" url="$2"; shift 2
	ls_run restore "$@" -o "/restore/${out}" "$url" >/dev/null 2>&1 || true
}
sq_restored() { sqlite3 "$WORK/restore/$1" "$2" 2>/dev/null || echo '<no restored db>'; }

restore_ok "${VICTIM}-latest.sqlite" "$(tenant_replica_url "$VICTIM" || true)" -integrity-check full
check "restored tenant is ${VICTIM}, not a neighbour" "$VICTIM" \
	"$(sq_restored "${VICTIM}-latest.sqlite" 'SELECT DISTINCT tenant FROM collection;')"
check "restored ${VICTIM} has both write phases" "2" \
	"$(sq_restored "${VICTIM}-latest.sqlite" 'SELECT count(*) FROM collection;')"

restore_ok "${VICTIM}-pit.sqlite" "$(tenant_replica_url "$VICTIM" || true)" -timestamp "$MARKER"
check "point-in-time restore rolls ${VICTIM} back to the marker" "1" \
	"$(sq_restored "${VICTIM}-pit.sqlite" 'SELECT count(*) FROM collection;')"
check "and it is still ${VICTIM}'s data" "$VICTIM" \
	"$(sq_restored "${VICTIM}-pit.sqlite" 'SELECT DISTINCT tenant FROM collection;')"

# Rolling one tenant back neither consumes nor invalidates another's replica.
restore_ok "charlie-latest.sqlite" "$(tenant_replica_url charlie || true)"
check "an untouched tenant still restores to current" "2" \
	"$(sq_restored "charlie-latest.sqlite" 'SELECT count(*) FROM collection;')"
check "and it is charlie's data" "charlie" \
	"$(sq_restored "charlie-latest.sqlite" 'SELECT DISTINCT tenant FROM collection;')"

log "7. retention policy: the values, and that Litestream honours where they sit"
# Litestream silently ignores a snapshot block written inside a dbs: entry and
# falls back to the 24h default, so "the file says 4320h" is not evidence. Two
# halves: the shipped file declares the production window at the top level, and
# a copy of that same file with only the interval shortened produces repeated
# level-9 snapshots — which it would not if the block were being ignored.
check "shipped config keeps the 180-day recovery horizon" "retention: 4320h" \
	"$(awk '/^snapshot:/{f=1;next} /^[^ ]/{f=0} f&&/retention:/{$1=$1;print;exit}' "$SHIPPED_YML")"
check "shipped config keeps the daily snapshot cadence" "interval: 24h" \
	"$(awk '/^snapshot:/{f=1;next} /^[^ ]/{f=0} f&&/interval:/{$1=$1;print;exit}' "$SHIPPED_YML")"
check "snapshot block is top-level, not inside a dbs: entry" "1" \
	"$(grep -c '^snapshot:' "$SHIPPED_YML")"
# Every replica block pins its region, whatever shape the config takes: without
# it Litestream calls s3:GetBucketLocation, which the backup role does not grant.
check "every s3 replica pins its region (no s3:GetBucketLocation)" \
	"$(grep -c '^ *- type: s3' "$SHIPPED_YML")" \
	"$(grep -c '^ *region: ' "$SHIPPED_YML")"

# Behavioural half: same file, interval rewritten to 2s, a database written to
# throughout. Ignored block => the 24h default => one initial level-9 object.
sed 's/^  interval: 24h/  interval: 2s/' "$SHIPPED_YML" >"$WORK/fast-snapshot.yml"
mkdir -p "$WORK/probe/tenants"
sqlite3 "$WORK/probe/tenants/echo.sqlite" \
	"PRAGMA journal_mode=WAL; CREATE TABLE collection (n INTEGER PRIMARY KEY, tenant TEXT);" >/dev/null
podman run -d --name "$PROBE_CTR" --network "$NET" --user 0 \
	-v "$WORK/probe:/data:Z" -v "$WORK/fast-snapshot.yml:/etc/litestream.yml:ro,Z" \
	-v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_REGION \
	-e LITESTREAM_S3_ENDPOINT -e LITESTREAM_S3_PATH=citest/probe \
	-e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_REGISTRY_PATH=citest/probe-registry.sqlite \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null
# Keep writing until a SECOND level-9 object appears, rather than writing for a
# fixed 15s and looking once. The claim is that a top-level `snapshot:` block is
# honoured — if it were being ignored, the 24h default applies and no amount of
# waiting produces a second snapshot, so a bounded wait keeps the assertion just
# as sharp while dropping the dependency on how fast this machine is today.
# deploy/ci.sh is parallel-safe by design and several polecats run it at once;
# this box was at load average 7-11 when the 15s form started failing (2026-08-08).
SNAPSHOTS=0
probe_deadline=$(( SECONDS + 120 ))
while [ "$SECONDS" -lt "$probe_deadline" ]; do
	sq "$WORK/probe/tenants/echo.sqlite" "INSERT INTO collection (tenant) VALUES ('echo');"
	SNAPSHOTS=$(mc ls -r "s/$BUCKET" 2>/dev/null | grep -c 'citest/probe/echo.sqlite/0009/' || true)
	[ "$SNAPSHOTS" -gt 1 ] && break
	sleep 1
done
check "the top-level snapshot interval is actually honoured (>1 snapshot)" "yes" \
	"$([ "$SNAPSHOTS" -gt 1 ] && echo yes || echo no)"
if [ "$SNAPSHOTS" -le 1 ]; then
	echo "  --- last 20 lines from ${PROBE_CTR} ---"
	podman logs "$PROBE_CTR" 2>&1 | tail -20 | sed 's/^/  /'
	echo "  --- container state: $(podman inspect -f '{{.State.Status}}' "$PROBE_CTR" 2>&1) ---"
fi
podman rm -f --ignore "$PROBE_CTR" >/dev/null 2>&1 || true

log "8. the tenant set the checker enumerates is the set the sidecar replicates"
# deploy/backup-check.sh asks tenants_on_volume() which tenants exist and then
# checks each one's replica. If that glob and Litestream's `pattern:` ever
# disagree, a tenant stops being backed up and the dead-man's switch does not
# notice — it would simply never ask about it.
check "tenants_on_volume matches the replicated set" "alpha bravo charlie delta" \
	"$(tenants_on_volume "$WORK/data" | sort | tr '\n' ' ' | sed 's/ $//')"
check "and it does not pick up the catalog" "0" \
	"$(tenants_on_volume "$WORK/data" | grep -c shared || true)"
# Nor the registry. It lives at the data root and is replicated as itself; if
# this glob ever reached it, backup-check would check it twice under the wrong
# URL and the tenant set would gain a member that is not a person.
check "nor the user registry" "0" \
	"$(tenants_on_volume "$WORK/data" | grep -c registry || true)"

log "9. malformed tenant names cannot retarget the S3 prefix"
# The names come off an operator's command line in restore-litestream.sh and
# backup-check.sh; a `/` or a `..` would silently address a different prefix.
for bad in "../other" "a/b" "Alpha" "-flag" ""; do
	if tenant_replica_url "$bad" >/dev/null 2>&1; then
		echo "  FAIL  tenant name '${bad}' was accepted"
		fail=$((fail + 1))
	else
		echo "  PASS  tenant name '${bad}' rejected"
		pass=$((pass + 1))
	fi
done

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — the shipped config replicates every tenant and the shipped"
echo "         addressing restores the right one."
