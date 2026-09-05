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

# Every SQLite write in this script goes through `sq`, never bare `sqlite3` —
# the registry writes below included, for exactly the same reason as the tenant
# writes pd-znsf added it for.
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
# The Litestream version too, from that same file (pd-pfxf). A gate that
# pulled `:latest` would test whatever the registry pushed last night and
# not what any box actually runs — which is exactly how a 0.5.16 -> 0.5.17
# retag turned three gates red on a change that touched none of them.
LITESTREAM_IMAGE="${LITESTREAM_IMAGE:-$PKDUMP_LITESTREAM_IMAGE}"

# Failure diagnostics (pd-8gjs): an ERR trap that names the failing file, line,
# command and status before the EXIT trap's teardown chatter, and `diag_run`,
# which captures a command's stderr and prints it on failure through a
# descriptor no call-site `2>/dev/null` can reach. This gate once died with a
# bare exit 127 and an empty log; see tests/lib/diagnostics.sh.
# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

# Host ports, from the kernel rather than picked (pd-r0ri). One definition for
# every harness; see tests/lib/ports.sh for what a picked one costs.
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

# Waiting for CONDITIONS rather than for the clock (pd-86er). Everything this
# gate used to spend on `sleep 8` is now a bounded poll that names what it is
# waiting for; see tests/lib/wait.sh.
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"
# shellcheck source=tests/lib/objects.sh
. "${REPO_DIR}/tests/lib/objects.sh"

# Unique per checkout, exactly as deploy/ci.sh's instance name and the alarming
# gate's are. deploy/ci.sh is deliberately parallel-safe — several polecats run
# it at once from their own worktrees — but it calls THIS script, and until
# pd-8gjs these names were fixed. Run B's opening `podman rm -f` / `network rm`
# then tore down run A's MinIO and sidecar mid-suite, and run A failed
# somewhere downstream looking nothing like a name clash.
SUFFIX="${PDLS_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdls-test-net-${SUFFIX}
MINIO_CTR=pdls-test-minio-${SUFFIX}
LS_CTR=pdls-test-litestream-${SUFFIX}
PROBE_CTR=pdls-test-probe-${SUFFIX}
# From the kernel, not picked: a fixed number collides both with a concurrent
# run of this gate and with whatever else happens to hold it on the box.
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

# restore_rows — restore a tenant's replica out-of-place and say how many rows
# came back; 0 when nothing restores yet, so it is safe to poll on. The output
# file is removed first because `litestream restore` refuses to overwrite one,
# and a poll asks repeatedly by definition.
#
# This is the ground truth for "the write reached S3" — not a log line, not the
# presence of an object. §3 waits on it, §6 asserts on it.
restore_rows() { # restore_rows <out> <tenant>
	local n
	rm -f "$WORK/restore/$1"
	ls_run restore -o "/restore/$1" "$(tenant_replica_url "$2" || true)" >/dev/null 2>&1 || true
	# Always a number, whatever happened: the callers compare numerically, and an
	# empty string there is a shell error rather than "not replicated yet".
	n="$(sq "$WORK/restore/$1" 'SELECT count(*) FROM collection;' 2>/dev/null)" || n=0
	printf '%s' "${n:-0}"
}

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
#
# It goes through bucket_keys, which RETURNS NON-ZERO when the listing itself
# could not be trusted rather than printing nothing (pd-cxq4). An `mc` run that
# died under load reports the same empty result a clean bucket does, and "clean"
# is what several assertions below expect — so the callers that expect a zero
# ask whether the listing succeeded before believing one.
bucket_keys() {
	local listing
	listing="$(object_store_ls "" mc ls -r "s/$BUCKET")" || return 1
	awk '{print $NF}' <<<"$listing"
}
tenant_prefixes() {
	bucket_keys | { grep "^${LITESTREAM_S3_PATH}/" || true; } | cut -d/ -f1-3 | sort -u
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
minio_live() { curl -fsS "http://127.0.0.1:$MINIO_PORT/minio/health/live" >/dev/null 2>&1; }
wait_until 60 0.25 minio_live || true
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
	-e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_REGISTRY_PATH \
	-e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null

# WAIT for the sidecar to be replicating, rather than sleeping a guessed
# interval (pd-86er). Two conditions, both load-bearing for what follows: every
# tenant has a prefix in the bucket, which is the set §4 reads back; and
# ${VICTIM}'s replica actually HOLDS the early phase, which is what makes the
# marker below a point-in-time boundary with something behind it rather than a
# timestamp taken before there was anything to roll back to.
early_replicated() {
	[ "$(tenant_prefixes | grep -c .)" -eq 3 ] || return 1
	[ "$(restore_rows early-probe.sqlite "$VICTIM")" -eq 1 ]
}
wait_until 120 1 early_replicated || true
# Asserted, not `|| true` on its own. Everything below reads the marker as a
# boundary with the early phase behind it; if the early phase never landed,
# the point-in-time restore two sections down fails as `<no restored db>` and
# reads like a derivation bug. The late side has had this check since it was
# written — the early side simply never grew one.
check "the early writes reached ${VICTIM}'s replica, so the marker has something behind it" "yes" \
	"$(early_replicated && echo yes || echo no)"

# ── 3b. THE LINE deploy/backup-check.sh JUDGES EVERY TENANT FROM ────────────
# Stated here, first and by name, because it is a CONTRACT with the image and
# not an incidental property of it (pd-pfxf). backup-check has no other source
# for a tenant's two TXIDs — the 0.5 CLI cannot resolve a `dir:` entry — so if
# this message stops being emitted, backup-check silently stops verifying every
# tenant for the whole 1800s grace and then pages forever.
#
# That is not hypothetical. `litestream:latest` moved 0.5.16 -> 0.5.17 on
# 2026-08-31, 0.5.17 downgraded this message from INFO to DEBUG, and the failure
# arrived as three unrelated-looking container gates going red at once — a
# missing txid advance here, "checked 0 of 4 tenants" in the drill, and a
# correspondence wait timing out in the alarming gate. None of them said what was
# actually wrong. This one does, in one line, before any of them run.
#
# The remedy has two halves and this asserts the pair: the version is pinned
# (deploy/litestream-lib.sh) and the level is asked for by name
# (deploy/litestream.yml's `logging:` block). Either alone is not enough.
log "3b. the sidecar emits the message deploy/backup-check.sh parses (pd-pfxf)"
sync_line_emitted() { logs_match "$LS_CTR" 'msg="replica sync"'; }
wait_until 60 1 sync_line_emitted || true
check "the running sidecar logs msg=\"replica sync\" at the configured level" "yes" \
	"$(sync_line_emitted && echo yes || echo no)"
# ...carrying BOTH txids, in the shape sidecar_position seds them out of. A line
# that survived a rename of either field would satisfy the grep above and leave
# backup-check parsing empty strings, which it reads as "no position" — the same
# silence, one field further in.
check "and it carries txid.replica and txid.db, the pair correspondence is judged on" "yes" \
	"$(logs_match "$LS_CTR" -E 'msg="replica sync".*txid\.replica=[0-9a-f]{16}.*txid\.db=[0-9a-f]{16}' \
		&& echo yes || echo no)"
# And it is THIS image, not whatever `:latest` points at today. A gate that
# pulled a moving tag would go red on a morning nobody changed anything.
check "the pinned image is the one under test" "$PKDUMP_LITESTREAM_IMAGE" "$LITESTREAM_IMAGE"

# A marker between two write phases, for the point-in-time restore in §4.
#
# TRUNCATION CUTS BOTH WAYS, and only one side of it was handled (pd-5m54 CI,
# 2026-08-14). `date -u +…%SZ` truncates DOWN to the start of the second it was
# taken in, so a marker taken at 00:49:54.9 IS 00:49:54.000 — up to a second
# EARLIER than the instant it was meant to name. If the early phase only became
# restorable at 00:49:54.3, that marker sits BEFORE the oldest instant the
# replica covers, and Litestream refuses it outright:
#
#     Error: timestamp does not exist
#
# which surfaces as `expected 1, got <no restored db>` — the same symptom as the
# late-side race below, from the opposite cause, and equally immune to retrying.
# It is reachable exactly when replication is FAST, because that is when the
# marker lands in the same second the early phase arrived. In the run that found
# it, §4b's registry PITR passed in the same gate: its marker is taken much later,
# against a long-established replica, so it was never near the edge.
#
# So: leave the second in which the early phase became restorable before naming
# a marker. With E the instant early_replicated went true and M the moment the
# marker is taken, waiting for M >= E + 1s gives floor(M) >= M - 1s >= E — the
# marker provably sits at or after the oldest covered instant, whatever the
# truncation does.
sleep 1
MARKER=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# The other side of the same truncation, and the reason this sleep stays: the
# marker names the whole second it was taken in, so the late writes have to land
# in a LATER one or a restore at the marker may legitimately include them. There
# is no condition to poll here — the wait is on the clock's resolution itself.
sleep 2
for t in alpha bravo charlie; do
	sq "$WORK/data/tenants/$t.sqlite" \
		"INSERT INTO collection (tenant, phase) VALUES ('$t','late');" >/dev/null
done
# ...and the same wait on the other side of the marker: the late phase is in the
# replica when a restore returns both rows, not after eight seconds.
late_replicated() { [ "$(restore_rows late-probe.sqlite "$VICTIM")" -eq 2 ]; }
wait_until 120 1 late_replicated || true

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
# Poll, don't guess an interval — see the note in §5. `podman logs` is a local
# read, so it can be asked twice a second without costing the runner anything.
REG_LOGGED=0
registry_seen() {
	REG_LOGGED=$(podman logs "$LS_CTR" 2>&1 | grep -c 'db=registry.sqlite' || true)
	[ "$REG_LOGGED" -gt 0 ]
}
wait_until 90 0.5 registry_seen || true
check "the registry replicates from the SAME sidecar (one process, no restart)" "yes" \
	"$([ "$REG_LOGGED" -gt 0 ] && echo yes || echo no)"
check "sidecar still running with both entries" "running" \
	"$(podman inspect -f '{{.State.Status}}' "$LS_CTR")"

# A `path:` replica prefix is used VERBATIM — the file name is NOT appended the
# way directory mode appends it. registry_replica_url encodes that difference,
# and this is the assertion that keeps the two in step: get it wrong and a
# restore reads an empty prefix and reports "no snapshots" on the one database
# whose loss makes every other restore unattributable.
# ONE listing, feeding this assertion and the bucket-root one below (pd-cxq4).
# No sentinel: the contents of this bucket ARE the thing under test, so an empty
# replica is a finding to report rather than a listing to distrust. What is
# checked is that the listing happened.
# WAIT FOR WHAT IS ASSERTED, not for a signal that merely precedes it. The poll above
# waits for `db=registry.sqlite` in the sidecar log, which says the registry was OPENED
# — the sidecar has the entry and has initialised it. The assertion below is about
# something later and separate: that LTX files have reached S3 under its prefix.
#
# Between those two events sits a sync, and how long that takes is the image's business.
# It was fast enough to hide the gap until the sidecar was pinned (pd-pfxf): on 0.5.17
# the listing ran first and the check reported the registry replica as EMPTY, which reads
# as a broken derived prefix rather than as "not yet". Three gates went red on a change
# that touched none of their logic.
#
# One listing still feeds both assertions; it is just taken once the thing being asserted
# is true, or after the budget is spent — in which case an empty replica is reported, as
# before, because the contents of this bucket ARE the thing under test.
BUCKET_LISTED=yes
BUCKET_KEYS=""
registry_replicated() {
	BUCKET_KEYS="$(bucket_keys)" || { BUCKET_KEYS=""; BUCKET_LISTED=no; return 1; }
	BUCKET_LISTED=yes
	grep -q "^${LITESTREAM_S3_REGISTRY_PATH}/" <<<"$BUCKET_KEYS"
}
wait_until 90 1 registry_replicated || true
grep "^${LITESTREAM_S3_REGISTRY_PATH}/" <<<"$BUCKET_KEYS" |
	head -1 >"$WORK/registry-key.txt" || true
check "the registry wrote LTX files under its own derived prefix" "1" \
	"$(grep -c . "$WORK/registry-key.txt" || true)"
check "registry_replica_url is that prefix" "s3://${BUCKET}/${LITESTREAM_S3_REGISTRY_PATH}" \
	"$(registry_replica_url | sed 's/?.*//')"

# It must sit BESIDE the tenants prefix, never inside it: the total-loss
# procedure lists that parent prefix to enumerate tenants, and a registry
# underneath it would come back as one more tenant to restore.
check "the registry prefix is NOT under the tenants prefix" "outside" \
	"$(case "$LITESTREAM_S3_REGISTRY_PATH" in "${LITESTREAM_S3_PATH%/}"/*) echo inside ;; *) echo outside ;; esac)"
# Asked so that a listing which FAILED cannot answer "0". Every other count in
# this gate expects a positive number and goes red on its own when the store
# cannot be reached; these two expect a zero, which is what a dead listing
# reports (pd-cxq4).
if REGISTRY_HITS="$(tenant_prefixes)"; then
	REGISTRY_HITS="$(grep -c 'registry' <<<"$REGISTRY_HITS" || true)"
else
	REGISTRY_HITS="the bucket listing failed — see above"
fi
check "and the tenant enumeration does not see it" "0" "$REGISTRY_HITS"
# Nothing at the bucket ROOT. This is the shape the silent failure takes: an
# empty replica prefix is accepted and scatters LTX files at the top of the
# bucket, where they belong to no instance and no database. Every key must sit
# under a named prefix.
if [[ "$BUCKET_LISTED" == yes ]]; then
	ROOT_KEYS="$(grep -c '^[0-9]\{4\}/' <<<"$BUCKET_KEYS" || true)"
else
	ROOT_KEYS="the bucket listing failed — see above"
fi
check "nothing was replicated to the bucket root" "0" "$ROOT_KEYS"

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
sleep 2 # second-resolution marker, as in §3 — not a wait for replication
sq "$WORK/data/registry.sqlite" \
	"INSERT INTO user VALUES ('bravo','01k2c7hq8n0000000000bravo',datetime('now'),'active');" >/dev/null
registry_has_both() {
	rm -f "$WORK/restore/registry-latest.sqlite"
	restore_registry registry-latest.sqlite
	[ "$(sq_registry registry-latest.sqlite 'SELECT count(*) FROM user;')" = 2 ]
}
wait_until 60 1 registry_has_both || true
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
delta_seen() {
	DELTA_LOGGED=$(podman logs "$LS_CTR" 2>&1 | grep -c 'delta.sqlite' || true)
	[ "$DELTA_LOGGED" -gt 0 ]
}
wait_until 90 0.5 delta_seen || true
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
delta_has_prefix() { tenant_prefixes | grep -qx "${LITESTREAM_S3_PATH}/delta.sqlite"; }
wait_until 60 1 delta_has_prefix || true
tenant_prefixes >"$WORK/prefixes.txt"
check "delta has its own derived prefix" "1" \
	"$(grep -cx "${LITESTREAM_S3_PATH}/delta.sqlite" "$WORK/prefixes.txt" || true)"
check "four tenants, four prefixes" "4" "$(grep -c . "$WORK/prefixes.txt")"

# WAIT FOR THE LATE WRITES TO REPLICATE before restoring to ${MARKER}.
#
# §3 waits for the EARLY phase so the marker has something behind it. Nothing
# waited for the LATE phase, so the marker could sit AHEAD of the replica's newest
# transaction — and Litestream refuses a timestamp outside the range it holds:
#
#     Error: timestamp does not exist
#
# which surfaced as `expected 1, got <no restored db>` and read like a derivation
# bug. Retrying cannot fix it, and did not: the bounded retry added for the lag
# case burned all nine attempts against a timestamp the replica would never be
# able to serve.
#
# drill.sh has had this wait since it was written (`late_replicated`); run.sh
# simply never grew one. Asserted rather than `|| true`, so if the late phase
# never lands the failure says so instead of blaming the restore two lines later.
late_replicated() { [ "$(restore_rows late-probe.sqlite "$VICTIM")" -ge 2 ]; }
wait_until 180 2 late_replicated || true
check "the late writes reached ${VICTIM}'s replica, so the marker is inside its range" "yes" \
	"$(late_replicated && echo yes || echo no)"

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
	local i
	# RETRY, because a restore failing and a restore returning the wrong bytes are
	# different findings that used to look identical. A one-shot restore that loses
	# a race with replication produces no file, `sq_restored` prints
	# `<no restored db>`, and the check below reports it as a DATA mismatch —
	# "expected 1, got <no restored db>" — which reads as a derivation bug.
	#
	# Observed 2026-08-13: the same commit failed twice in a row on this box, once
	# here and once in drill/, in different assertions each time. The point-in-time
	# restore is the sensitive one — it can only succeed once the replica covers
	# ${MARKER}, and under load (three container gates at a time on a 16 GB box with
	# swap exhausted) that lag is seconds, not milliseconds.
	#
	# Waiting on the condition rather than on the clock, per pd-86er. The final
	# attempt goes through diag_run so a genuine failure still takes Litestream's
	# own words into the log rather than leaving `<no restored db>` as the clue.
	for i in 1 2 3 4 5 6 7 8 9; do
		rm -f "${WORK}/restore/${out}"
		if ls_run restore "$@" -o "/restore/${out}" "$url" >/dev/null 2>&1 \
			&& [ -s "${WORK}/restore/${out}" ]; then
			return 0
		fi
		sleep 3
	done
	rm -f "${WORK}/restore/${out}"
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
second_snapshot() {
	sq "$WORK/probe/tenants/echo.sqlite" "INSERT INTO collection (tenant) VALUES ('echo');"
	local keys
	# A listing that failed is not a bucket with no snapshot in it (pd-cxq4);
	# it is a poll that did not happen, so the poll goes round again.
	keys="$(bucket_keys)" || return 1
	SNAPSHOTS=$(grep -c 'citest/probe/echo.sqlite/0009/' <<<"$keys" || true)
	[ "$SNAPSHOTS" -gt 1 ]
}
wait_until 120 1 second_snapshot || true
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

log "9. what may name a database — both shapes, and nothing else"
# These come off an operator's command line in restore-litestream.sh and
# backup-check.sh; a `/` or a `..` would silently address a different prefix.
#
# A data volume mid-migration (pd-hqee) holds BOTH shapes at once: databases
# minted since pd-zr9n are named by a 26-character uppercase Crockford
# `database_id`, and ones predating it are still named by their handle —
# tenants/collection.sqlite is on the prod box today. So the derivation has to
# accept both, and a version that took only one would refuse to restore real
# databases. It took only handles until pd-nd6w, which is why the uppercase id
# below is an assertion and not a formality.
for good in "01J8Z9K3QW0000000000000AAA" "collection" "alice" "tenant-2"; do
	if tenant_replica_url "$good" >/dev/null 2>&1; then
		echo "  PASS  '${good}' accepted"
		pass=$((pass + 1))
	else
		echo "  FAIL  '${good}' was REJECTED — a real database could not be restored"
		fail=$((fail + 1))
	fi
done
# Traversal and separators, still. Plus two near-miss ids: 25 characters, and one
# containing `I` — Crockford excludes I/L/O/U precisely because they misread as
# 1/1/0/V, and a typo'd id must not address a prefix at all.
for bad in "../other" "a/b" "Alpha" "-flag" "" "01J8Z9K3QW0000000000000AA" "01I8Z9K3QW0000000000000AAA"; do
	if tenant_replica_url "$bad" >/dev/null 2>&1; then
		echo "  FAIL  '${bad}' was accepted"
		fail=$((fail + 1))
	else
		echo "  PASS  '${bad:-<empty>}' rejected"
		pass=$((pass + 1))
	fi
done

log "10. a database RENAMED onto an opaque id keeps replicating (pd-hqee, pd-1717)"
# `pkdump tenant migrate` renames tenants/<handle>.sqlite to
# tenants/<database_id>.sqlite. In directory mode the replica prefix is DERIVED
# FROM THE FILENAME, so that rename does not move a replica — it starts a new,
# EMPTY one, and the old prefix stops advancing and keeps everything it had.
#
# That is the step which silently broke production. After the pd-gckl migration
# Litestream came up ACTIVE, logged "snapshot complete", and replicated NOTHING:
#
#   msg="replica sync" db=collection.sqlite replica=s3 \
#       txid.replica=0000000000000000 txid.db=00000000000004e6
#
# The per-database state directory had been carried across the prefix change, so
# its LTX history began mid-stream while the new prefix was at txid 0, and it
# could not catch one up from the other ("LTX file is missing"). Removing the
# state directory was the fix, and `pkdump tenant migrate` now does it as part
# of the rename — `migrating_clears_litestream_state_rather_than_carrying_it` in
# crates/pkdump-db/src/tenants.rs is the test that it does.
#
# What THAT test cannot show is whether the resulting shape actually replicates,
# because it has no Litestream. This does: same move, real sidecar, and the
# assertion is that the new prefix ADVANCES — txid.replica non-zero, and a
# restore from it returning the rows written after the rename. "The unit is
# active" is precisely the signal that lied last time.
MIGRATED_ID=01K2C7HQ8N0000000000000CHR   # 26 chars of Crockford base32, as minted
check "the stand-in database id is one the shipped derivation accepts" "yes" \
	"$(tenant_replica_url "$MIGRATED_ID" >/dev/null 2>&1 && echo yes || echo no)"

# Stop the sidecar first — deploy/TENANTS.md's runbook does, and the migration
# refuses to move a database another process is writing to.
podman stop "$LS_CTR" >/dev/null
mv "$WORK/data/tenants/charlie.sqlite" "$WORK/data/tenants/${MIGRATED_ID}.sqlite"
# Exactly what migrate does with the state directory: removes it. Not moves it.
rm -rf "$WORK/data/tenants/.charlie.sqlite-litestream"
# A write that exists only AFTER the rename, so "the new prefix has data" cannot
# be satisfied by whatever the old prefix already held.
sqlite3 "$WORK/data/tenants/${MIGRATED_ID}.sqlite" \
	"INSERT INTO collection (tenant, phase) VALUES ('charlie','post-migration');" >/dev/null
podman start "$LS_CTR" >/dev/null

# Wait for the new prefix to hold a restorable database carrying that write.
migrated_restored() {
	rm -f "$WORK/restore/migrated.sqlite"
	restore_ok "migrated.sqlite" "$(tenant_replica_url "$MIGRATED_ID" || true)"
	[ "$(sq_restored migrated.sqlite "SELECT count(*) FROM collection WHERE phase='post-migration';")" = 1 ]
}
wait_until 90 1 migrated_restored || true
check "the renamed database restores from its NEW derived prefix" "3" \
	"$(sq_restored migrated.sqlite 'SELECT count(*) FROM collection;')"
check "including the write made after the rename" "1" \
	"$(sq_restored migrated.sqlite "SELECT count(*) FROM collection WHERE phase='post-migration';")"
check "and it is still charlie's data under its new name" "charlie" \
	"$(sq_restored migrated.sqlite 'SELECT DISTINCT tenant FROM collection;')"

# THE pd-1717 ASSERTION, read off the sidecar's own account rather than inferred
# from a successful restore: replication is ADVANCING, not stuck at zero.
#
# `txid.replica=0000000000000000` on the FIRST sync after a rename is correct and
# healthy — the new prefix is empty until the first upload lands, and Litestream
# logs the sync it is about to do. The pathology is that it never moves off zero
# while txid.db climbs, so the assertion has to be about what happens NEXT: keep
# writing, and require a sync line whose replica txid is non-zero. Looking once
# would be asserting how fast this machine is, and on a box running deploy/ci.sh
# it caught exactly that first line (2026-08-08).
ADVANCED=no
replica_advanced() {
	# `logs_match`, not `grep -q`: this is the pd-pfxf trap, and it survived the
	# ratchet that exists to catch it only because the pipeline is split over
	# two lines. Under DEBUG the sidecar's log is now big enough that grep -q
	# closes the pipe well before `podman logs` is done writing — SIGPIPE, 141,
	# pipefail, and a match reported as no-match, on the busy box and not on the
	# quiet one. It also has to name the MESSAGE: at this level most lines
	# carrying `db=<id>.sqlite` are `msg="s3 request"`, so matching the two
	# fields anywhere in the stream could pair a TXID from one line with the
	# database name from another.
	if logs_match "$LS_CTR" -E \
		"msg=\"replica sync\".*db=${MIGRATED_ID}\.sqlite.*txid\.replica=0*[1-9a-f]"; then
		ADVANCED=yes
		return 0
	fi
	# Keep giving it something to advance PAST, so a replica that is genuinely
	# stuck is distinguishable from one with nothing to do.
	sq "$WORK/data/tenants/${MIGRATED_ID}.sqlite" \
		"INSERT INTO collection (tenant, phase) VALUES ('charlie','advancing');" >/dev/null
	return 1
}
wait_until 90 1 replica_advanced || true
check "txid.replica advances on the new prefix, rather than sticking at 0" "yes" "$ADVANCED"
if [ "$ADVANCED" != yes ]; then
	echo "  --- replica sync lines for ${MIGRATED_ID}.sqlite ---"
	# The sync lines, not every line naming the database: at DEBUG the latter is
	# a wall of `msg="s3 request"` and the ten this prints would carry nothing
	# anybody could diagnose the failure from.
	podman logs "$LS_CTR" 2>&1 | grep -F 'msg="replica sync"' |
		grep -F "db=${MIGRATED_ID}.sqlite" | tail -10 | sed 's/^/  /'
fi

# The pre-cutover half of the recovery window: the old prefix stops advancing
# but keeps every object it had, which is what deploy/TENANTS.md tells an
# operator to restore from for anything before the migration.
restore_ok "pre-migration.sqlite" "$(tenant_replica_url charlie || true)"
check "the pre-migration prefix still restores, as deep as it was" "2" \
	"$(sq_restored "pre-migration.sqlite" 'SELECT count(*) FROM collection;')"
check "and the two prefixes are different objects, not one moved" "2" \
	"$(tenant_prefixes | grep -cE "/(charlie|${MIGRATED_ID})\.sqlite$" || true)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — the shipped config replicates every tenant and the shipped"
echo "         addressing restores the right one."
