#!/usr/bin/env bash
# Container-tier gate (pd-fof4): the SHIPPED Litestream config replicates every
# tenant, and the SHIPPED restore addressing brings back the right one.
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
#
# Prod-safe: its own podman network, its own MinIO, its own temp dir, its own
# throwaway SQLite files. Touches no pkdump-* unit, no pkdump-*-data volume, no
# real S3 bucket and no application code.
set -euo pipefail

# Every SQLite write in this script goes through `sq`, never bare `sqlite3`.
#
# The whole point of this gate is that Litestream is replicating these databases
# WHILE we write to them, so contention is not an edge case here — it is the
# scenario under test. A bare `sqlite3` has no busy timeout: it returns
# SQLITE_BUSY the instant the replicator holds the lock, and with `set -e` that
# takes the entire CI run down with `Error: stepping, database is locked (5)`.
# Intermittent by nature — the same commit passed one run and failed the next.
#
# The application already gets this right (pkdump-db sets a 5s busy_timeout,
# which is why pd-jgd4 measured zero SQLITE_BUSY across every concurrency arm).
# Only this harness was missing it, so a correct product looked flaky.
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

# Failure diagnostics (pd-8gjs): an ERR trap that names the failing file, line,
# command and status before the EXIT trap's teardown chatter, and `diag_run`,
# which captures a command's stderr and prints it on failure through a
# descriptor no call-site `2>/dev/null` can reach. This gate once died with a
# bare exit 127 and an empty log; see tests/lib/diagnostics.sh.
# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

# Unique per checkout, exactly as deploy/ci.sh's instance name and the alarming
# gate's are. deploy/ci.sh is deliberately parallel-safe — several polecats run
# it at once from their own worktrees — but it calls THIS script, and until
# pd-8gjs these names were fixed. Run B's opening `podman rm -f` / `network rm`
# then tore down run A's MinIO and sidecar mid-suite, and run A failed
# somewhere downstream looking nothing like a name clash.
SUFFIX="$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)"
NET=pdls-test-net-${SUFFIX}
MINIO_CTR=pdls-test-minio-${SUFFIX}
LS_CTR=pdls-test-litestream-${SUFFIX}
PROBE_CTR=pdls-test-probe-${SUFFIX}
# From the kernel, not picked: a fixed number collides both with a concurrent
# run of this gate and with whatever else happens to hold it on the box.
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()'; }
MINIO_PORT=${MINIO_PORT:-$(free_port)}
# The bucket is inside this run's own MinIO, but keep it distinct too so that a
# stray `mc` pointed at the wrong endpoint cannot silently share state.
BUCKET=pdls-test-${SUFFIX}
AKID=pdlstestroot
SECRET=pdlstestsecret123

# The instance env a real deploy would provide (deploy/setup.sh writes this
# shape into ~/.config/pkdump/<instance>/litestream.env), pointed at MinIO.
export LITESTREAM_TENANTS_DIR=/data/tenants
export LITESTREAM_S3_BUCKET=$BUCKET
export LITESTREAM_S3_PATH=citest/tenants
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
	local rc=$?
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
	# Said last, after the teardown noise, so the status is never something the
	# reader has to infer from where the log happens to stop.
	[[ $rc -eq 0 ]] || diag "!! tests/litestream/run.sh exiting with status ${rc}"
}
trap cleanup EXIT

# Every mc invocation goes through diag_run: silent while it works, and on
# failure it prints the podman command, its exit status and everything mc wrote
# to stderr — through $PD_DIAG_FD, so a caller's own `2>/dev/null` cannot eat
# the explanation for that caller's own death. This is the exact call path that
# produced a bare, unexplained 127 in pd-8gjs.
mc() { diag_run mc podman run --rm --network "$NET" -e "MC_HOST_s=http://$AKID:$SECRET@$MINIO_CTR:9000" "$MC_IMAGE" "$@"; }

# litestream, with the same credential wiring the deploy scripts use in prod
# (a shared-credentials file selected by AWS_PROFILE — in prod that profile
# assumes a role; here it holds MinIO's static test key).
ls_run() { podman run --rm --network "$NET" --user 0 \
	-v "$WORK/restore:/restore:Z" -v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	"$LITESTREAM_IMAGE" "$@"; }

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
	sq "$WORK/data/tenants/$1.sqlite" \
		"PRAGMA journal_mode=WAL;
		 CREATE TABLE collection (n INTEGER PRIMARY KEY, tenant TEXT, phase TEXT);
		 INSERT INTO collection (tenant, phase) VALUES ('$1','early');" >/dev/null
}
# The catalog sits BESIDE tenants/, never inside it: it is reproducible from
# upstream and must not be replicated. Put a decoy there to prove the glob
# cannot reach it.
sq "$WORK/data/shared.sqlite" "CREATE TABLE cards (id TEXT);" >/dev/null
# alpha/bravo/charlie exist before the sidecar starts; delta is created later,
# with the sidecar already running (§5).
for t in alpha bravo charlie; do seed_tenant "$t"; echo "  $t"; done

log "3. ONE sidecar, the SHIPPED deploy/litestream.yml, no per-tenant config"
podman run -d --name "$LS_CTR" --network "$NET" --user 0 \
	-v "$WORK/data:/data:Z" -v "$SHIPPED_YML:/etc/litestream.yml:ro,Z" \
	-v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_PATH \
	-e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null
sleep 8

# A marker between two write phases, for the point-in-time restore in §4.
MARKER=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 2
for t in alpha bravo charlie; do
	sq "$WORK/data/tenants/$t.sqlite" \
		"INSERT INTO collection (tenant, phase) VALUES ('$t','late');" >/dev/null
done
sleep 8

check "sidecar is still running (one process, N databases)" "running" \
	"$(podman inspect -f '{{.State.Status}}' "$LS_CTR")"
check "no ERROR lines from the shipped config" "0" \
	"$(podman logs "$LS_CTR" 2>&1 | grep -c 'level=ERROR' || true)"

log "4. every tenant has its OWN derived prefix — and the shipped derivation agrees"
# The set of prefixes Litestream actually created, read back out of the bucket.
mc ls -r "s/$BUCKET" | awk '{print $NF}' | cut -d/ -f1-3 | sort -u >"$WORK/prefixes.txt"
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

log "5. a tenant created AFTER startup replicates with no config edit, no restart"
seed_tenant delta
sleep 12
DELTA_LOGGED=$(podman logs "$LS_CTR" 2>&1 | grep -c 'delta.sqlite' || true)
check "delta was picked up live by watch:" "yes" \
	"$([ "$DELTA_LOGGED" -gt 0 ] && echo yes || echo no)"
mc ls -r "s/$BUCKET" | awk '{print $NF}' | cut -d/ -f1-3 | sort -u >"$WORK/prefixes.txt"
check "delta has its own derived prefix" "1" \
	"$(grep -cx "${LITESTREAM_S3_PATH}/delta.sqlite" "$WORK/prefixes.txt" || true)"
check "four tenants, four prefixes" "4" "$(grep -c . "$WORK/prefixes.txt")"

log "6. restore the NON-FIRST tenant — latest, and point-in-time"
# Restoring tenant #1 would pass even if the derivation were off by one, so the
# whole point of this section is that the victim is #2 of 4.
# A failed restore must not abort the report — the checks below turn it into a
# FAIL line instead. But "not fatal" is not the same as "not worth mentioning":
# both of these route the failure through diag_run, so a restore that dies takes
# litestream's (or sqlite's) own words with it into the log rather than leaving
# `<no restored db>` as the only clue.
restore_ok() { # restore_ok <out> <url> [flags...] — never aborts the report
	local out="$1" url="$2"; shift 2
	diag_run "restore ${out}" ls_run restore "$@" -o "/restore/${out}" "$url" >/dev/null || true
}
sq_restored() { diag_run "sqlite ${1}" sq "$WORK/restore/$1" "$2" || echo '<no restored db>'; }

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
sq "$WORK/probe/tenants/echo.sqlite" \
	"PRAGMA journal_mode=WAL; CREATE TABLE collection (n INTEGER PRIMARY KEY, tenant TEXT);" >/dev/null
podman run -d --name "$PROBE_CTR" --network "$NET" --user 0 \
	-v "$WORK/probe:/data:Z" -v "$WORK/fast-snapshot.yml:/etc/litestream.yml:ro,Z" \
	-v "$WORK/aws:/aws:ro,Z" \
	-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_REGION \
	-e LITESTREAM_S3_ENDPOINT -e LITESTREAM_S3_PATH=citest/probe \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null
for i in $(seq 15); do
	sq "$WORK/probe/tenants/echo.sqlite" "INSERT INTO collection (tenant) VALUES ('echo');"
	sleep 1
done
podman rm -f --ignore "$PROBE_CTR" >/dev/null 2>&1 || true
SNAPSHOTS=$(mc ls -r "s/$BUCKET" | grep -c 'citest/probe/echo.sqlite/0009/' || true)
check "the top-level snapshot interval is actually honoured (>1 snapshot in 15s)" "yes" \
	"$([ "$SNAPSHOTS" -gt 1 ] && echo yes || echo no)"

log "8. the tenant set the checker enumerates is the set the sidecar replicates"
# deploy/backup-check.sh asks tenants_on_volume() which tenants exist and then
# checks each one's replica. If that glob and Litestream's `pattern:` ever
# disagree, a tenant stops being backed up and the dead-man's switch does not
# notice — it would simply never ask about it.
check "tenants_on_volume matches the replicated set" "alpha bravo charlie delta" \
	"$(tenants_on_volume "$WORK/data" | sort | tr '\n' ' ' | sed 's/ $//')"
check "and it does not pick up the catalog" "0" \
	"$(tenants_on_volume "$WORK/data" | grep -c shared || true)"

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
