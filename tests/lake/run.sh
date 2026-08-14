#!/usr/bin/env bash
# Container-tier gate (pd-fzeb): the lakehouse substrate works end to end.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/run.sh          # ~2min, full run + teardown
#   KEEP=1 bash tests/lake/run.sh   # leave MinIO + Nessie + WORK up for poking
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# Iceberg + Nessie is deliberately overkill for this data size (design,
# 2026-08-10, per Ryan: "that will be overkill but give us the best primitives
# to move rapidly on offline data concerns"). **Time travel is the primitive
# being bought.** A stack that stands up, accepts writes and reads them back
# looks entirely healthy while failing at the one thing it was chosen for, so
# the round trip below does not stop at read-back:
#
#   * §3 writes, reads, writes again, and reads the table AS OF the first
#     commit — both by Nessie ref (the whole catalog at a commit) and by
#     Iceberg snapshot id (one table's history). See lake/src/pkdump_lake/
#     roundtrip.py; the assertions live there, in the job, not here.
#   * §4 RESTARTS Nessie and re-reads. The version store is configured ROCKSDB
#     on a host directory, and the difference between that and Nessie's default
#     IN_MEMORY store is invisible until something restarts — at which point
#     every table ever created is gone and the bucket is full of orphaned
#     metadata. A gate that never restarts the catalog cannot tell the two
#     apart.
#   * §5 asserts the objects landed under the `lake/` prefix, because the
#     warehouse location is server config that a client cannot see it got wrong.
#
# It also MEASURES rather than assumes (the bead asks for an honest report):
# Nessie's resident memory under its cap, and the on-disk size of the version
# store, are printed at the end.
#
# Prod-safe: its own podman network, its own MinIO, its own Nessie, its own temp
# dir, its own image tag. Touches no pkdump-* unit, no pkdump-*-data volume, no
# real S3 bucket, and no tenant database — the lake never holds tenant data at
# all, which is the standing decision this whole design rests on. That decision
# has its own gate now: tests/lake/tenant_isolation_test.sh, in the lint tier,
# because asserting it needs no container and it should fail in a second rather
# than behind this two-minute run (pd-cgi9).
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

# `diag` writes through a descriptor no call-site 2>/dev/null can silence; this
# is the failure-and-stop form of it.
die() {
	diag "!! $*"
	exit 1
}

# Host ports from the kernel, never picked (pd-r0ri).
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

# Bounded condition polling, in one place for every harness (pd-86er).
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"

# Non-prod container storage belongs on the data disk, not the disk prod runs
# from — same resolution deploy/ci.sh uses, so running this gate standalone
# lands in the same store as running it through CI.
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout: deploy/ci.sh is parallel-safe and several polecats run it
# at once from their own worktrees. A fixed name means run B's opening `rm -f`
# tears down run A's Nessie mid-suite.
SUFFIX="${PDLAKE_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdlake-test-net-${SUFFIX}
MINIO_CTR=pdlake-test-minio-${SUFFIX}
NESSIE_CTR=pdlake-test-nessie-${SUFFIX}
JOB_IMAGE=pkdump-lake:test-${SUFFIX}
BUCKET=pdlake-test-${SUFFIX}
MINIO_PORT=${MINIO_PORT:-$(free_port)}
NESSIE_PORT=${NESSIE_PORT:-$(free_port)}
AKID=pdlaketestroot
SECRET=pdlaketestsecret123

# The warehouse prefix inside the bucket. `lake/` for Iceberg tables, beside
# `raw/` for the landing zone (pd-1ojt) — one bucket, two prefixes that never
# overlap.
WAREHOUSE="s3://${BUCKET}/lake"

WORK=${WORK:-$(mktemp -d /tmp/pdlake-test.XXXXXX)}
# Nessie's version store. On a real box this is a directory on /workspaces; here
# it is a temp dir, but it is a HOST directory either way — that is what §4
# restarts across.
mkdir -p "$WORK/nessie" "$WORK/minio"
chmod 777 "$WORK/nessie" "$WORK/minio"

cleanup() {
	if [ -n "${KEEP:-}" ]; then
		echo ""
		echo "KEEP=1 — leaving everything up:"
		echo "  MinIO   http://localhost:${MINIO_PORT}  (${AKID} / ${SECRET})"
		echo "  Nessie  http://localhost:${NESSIE_PORT}"
		echo "  work    ${WORK}"
		echo "  tear down: podman rm -f ${MINIO_CTR} ${NESSIE_CTR}; podman network rm ${NET}"
		return
	fi
	podman rm -f "$MINIO_CTR" "$NESSIE_CTR" >/dev/null 2>&1 || true
	podman network rm "$NET" >/dev/null 2>&1 || true
	podman rmi -f "$JOB_IMAGE" >/dev/null 2>&1 || true
	# Nessie writes its version store as the image's own uid, which rootless
	# podman maps to a SUBUID — the invoking user cannot unlink those files. A
	# plain `rm -rf` leaves the whole work dir behind, silently, every run.
	podman unshare rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

mc() { podman run --rm --network "$NET" -e MC_HOST_m="http://${AKID}:${SECRET}@${MINIO_CTR}:9000" "$MC_IMAGE" "$@"; }

echo "==> §0  Build the lake job image (PyIceberg, no JVM)"
# The job runtime is a container like everything else here. pd-1ojt's table
# build gets the same image; this gate is what keeps it buildable.
podman build -t "$JOB_IMAGE" -f "${REPO_DIR}/lake/Containerfile" "${REPO_DIR}/lake" >/dev/null
echo "    ${JOB_IMAGE}"

echo "==> §1  Object store (MinIO stands in for the lake bucket)"
# The real bucket's name is host config — ~/.config/pkdump/lake.env, created by
# Ryan. A test must never reach a real bucket anyway, so this gate uses its own
# MinIO exactly as every other S3 gate in this repo does.
podman network create "$NET" >/dev/null
podman run -d --name "$MINIO_CTR" --network "$NET" \
	-p "127.0.0.1:${MINIO_PORT}:9000" \
	-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET" \
	-v "$WORK/minio:/data:Z" \
	"$MINIO_IMAGE" server /data >/dev/null

minio_live() { curl -sf -o /dev/null "http://localhost:${MINIO_PORT}/minio/health/live"; }
# Four times a second against the same 30s bound: a loopback curl costs nothing
# and the one-second interval was latency, not patience (pd-86er).
wait_until 30 0.25 minio_live || true
curl -sf -o /dev/null "http://localhost:${MINIO_PORT}/minio/health/live" ||
	die "MinIO never became healthy on ${MINIO_PORT}"
mc mb "m/${BUCKET}" >/dev/null
echo "    bucket ${BUCKET}, warehouse ${WAREHOUSE}"

echo "==> §2  Nessie (ROCKSDB version store on a host directory, memory-capped)"
# --memory caps the container; MaxRAMPercentage inside the image then sizes the
# heap from that. Nessie's object cache is sized as a FRACTION OF THE HEAP by
# default and reported 6.7 GB on this box unconfigured, so it is pinned too —
# an uncapped catalog server on a box that also runs prod is not acceptable.
podman run -d --name "$NESSIE_CTR" --network "$NET" \
	-p "127.0.0.1:${NESSIE_PORT}:19120" \
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

nessie_answering() { curl -sf -o /dev/null "http://localhost:${NESSIE_PORT}/api/v2/config"; }
nessie_wait() {
	wait_until 60 0.25 nessie_answering && return 0
	podman logs "$NESSIE_CTR" 2>&1 | tail -40
	die "Nessie never answered on ${NESSIE_PORT}"
}
nessie_wait
echo "    Nessie up: $(curl -s "http://localhost:${NESSIE_PORT}/api/v2/config" | tr -d '\n ' | head -c 120)"

# The Iceberg REST endpoint has to be advertising OUR warehouse. If it is not,
# every write below still succeeds — somewhere else.
ICEBERG_URI="http://${NESSIE_CTR}:19120/iceberg/"
ADVERTISED=$(curl -s "http://localhost:${NESSIE_PORT}/iceberg/v1/config" | grep -o '"warehouse"[^,]*' | head -1)
case "$ADVERTISED" in
*"$WAREHOUSE"*) echo "    Iceberg REST advertises ${WAREHOUSE}" ;;
*) die "Nessie advertises the wrong warehouse: ${ADVERTISED}" ;;
esac

run_job() {
	podman run --rm --network "$NET" \
		-e PKDUMP_LAKE_NESSIE_URI="$ICEBERG_URI" \
		-e PKDUMP_LAKE_S3_ENDPOINT="http://${MINIO_CTR}:9000" \
		-e PKDUMP_LAKE_S3_ACCESS_KEY_ID="$AKID" \
		-e PKDUMP_LAKE_S3_SECRET_ACCESS_KEY="$SECRET" \
		-e PKDUMP_LAKE_S3_REGION=us-west-2 \
		-e PKDUMP_LAKE_S3_PATH_STYLE=1 \
		"$JOB_IMAGE" "$@"
}

echo "==> §3  Round trip: create, write, read, commit again, TIME TRAVEL"
# The assertions are in the job (lake/src/pkdump_lake/roundtrip.py), not here:
# it is the documented proof an operator runs against a live instance, so it has
# to fail on its own rather than depend on a harness reading its output.
run_job pkdump-lake-roundtrip

echo "==> §4  Restart Nessie — the version store must be durable"
# ROCKSDB vs Nessie's default IN_MEMORY store is invisible until this moment.
podman restart "$NESSIE_CTR" >/dev/null
nessie_wait
SURVIVED=$(run_job python -c '
from pkdump_lake.catalog import catalog
c = catalog()
t = c.load_table("proof.roundtrip")
print(len(t.scan().to_arrow()))
' | tail -1)
[ "$SURVIVED" = "2" ] ||
	die "after restart the table had ${SURVIVED} rows, expected 2 — the version store is not durable"
echo "    ok   proof.roundtrip survived a catalog restart with both rows"

echo "==> §5  The bytes are under the lake/ prefix"
OBJECTS=$(mc ls --recursive "m/${BUCKET}" | wc -l)
[ "$OBJECTS" -gt 0 ] || die "nothing was written to ${BUCKET}"
STRAY=$(mc ls --recursive "m/${BUCKET}" | grep -cv ' lake/' || true)
[ "$STRAY" -eq 0 ] ||
	die "${STRAY} object(s) landed outside the lake/ prefix"
echo "    ok   ${OBJECTS} objects, all under lake/"

echo "==> §6  deploy/setup-lake.sh refuses rather than guesses"
# The two refusals worth gating, both cheap: they fire before the script touches
# podman or systemd, so this needs nothing but a throwaway HOME.
#
# The rest of the install path is deliberately NOT gated here — it needs a real
# lake bucket, which does not exist yet (pd-fzeb reports that rather than
# inventing one). What IS gated is the half that protects the backup bucket.
FAKE_HOME="$WORK/home"
mkdir -p "$FAKE_HOME/.config/pkdump/lakeinst"

# `|| rc=$?` rather than `set +e`: the ERR trap diagnostics.sh installs fires on
# a failing command regardless of set -e, so a bare `set +e` here would print
# four "!! FAILED" blocks for the two failures this section is asserting. A
# failure a `||` handles is not a failure bash reports.
NO_ENV_RC=0
NO_ENV_OUT=$(HOME="$FAKE_HOME" bash "${REPO_DIR}/deploy/setup-lake.sh" lakeinst 2>&1) || NO_ENV_RC=$?
[ "$NO_ENV_RC" -ne 0 ] || die "setup-lake.sh accepted a missing lake.env"
case "$NO_ENV_OUT" in
*"lake.env does not exist"*) echo "    ok   no lake.env -> refuses, and names the file" ;;
*) die "setup-lake.sh refused without naming lake.env: ${NO_ENV_OUT}" ;;
esac

# The lake bucket and the Litestream backup bucket must never be the same one:
# everything in the lake is reproducible, and everything in the backup bucket is
# the opposite. A lifecycle rule written for one must not be able to reach the
# other.
printf 'PKDUMP_LAKE_S3_BUCKET=same-bucket\nPKDUMP_LAKE_S3_REGION=us-west-2\nAWS_PROFILE=pkdump\nPKDUMP_LAKE_NESSIE_DATA=%s/nessie-unused\n' \
	"$WORK" >"$FAKE_HOME/.config/pkdump/lake.env"
printf 'LITESTREAM_S3_BUCKET=same-bucket\n' \
	>"$FAKE_HOME/.config/pkdump/lakeinst/litestream.env"
SAME_RC=0
SAME_OUT=$(HOME="$FAKE_HOME" bash "${REPO_DIR}/deploy/setup-lake.sh" lakeinst 2>&1) || SAME_RC=$?
[ "$SAME_RC" -ne 0 ] || die "setup-lake.sh accepted the backup bucket as the lake bucket"
case "$SAME_OUT" in
*"must be separate"*) echo "    ok   lake bucket == backup bucket -> refuses" ;;
*) die "setup-lake.sh refused for the wrong reason: ${SAME_OUT}" ;;
esac

echo ""
echo "==> Measured (pd-fzeb asked for numbers, not adjectives)"
# Resident memory of the JVM under a 1 GiB container cap, and the size the
# version store reached for a two-commit toy table.
RSS_KB=$(podman exec "$NESSIE_CTR" sh -c "awk '/VmRSS/{print \$2}' /proc/1/status" 2>/dev/null || echo 0)
echo "    Nessie RSS:        $((RSS_KB / 1024)) MiB  (container capped at 1024 MiB)"
echo "    version store:     $(podman exec "$NESSIE_CTR" du -sh /nessie | cut -f1)  at ${WORK}/nessie"
echo "    warehouse objects: ${OBJECTS} under ${WAREHOUSE}"
echo ""
echo "==> tests/lake/run.sh PASSED"
