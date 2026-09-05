#!/usr/bin/env bash
# Container-tier gate (pd-dfb2118c / pd-1717): every backup-alarming layer is
# made to FIRE, at a sink that records what it received.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/alarming/run.sh          # ~2min, throwaway everything
#   KEEP=1 bash tests/alarming/run.sh   # leave the test bed up for poking
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# Alarming that is installed, reviewed, documented — and inert. All five layers
# were implemented in Jun 2026 and none had ever fired; on 2026-08-08 prod's
# Litestream replication stopped for 20 minutes with the unit `active` and the
# log saying "snapshot complete", and a human noticed by looking. Reading the
# scripts is what produced that state, so this gate does not read them: it
# points PKDUMP_BACKUP_PING_URL and PUSHOVER_API_URL at a local HTTP recorder
# and asserts on the requests that arrive.
#
# The specific regression it locks down is the SKIP (pd-1717): backup-check.sh
# used to print "skipping" and exit 0 when no monitor was configured, which is
# indistinguishable from a pass in `systemctl status`, in the journal, and in
# any dashboard. §5 mutates the config both ways and asserts the exit codes.
#
# The second one is the FALSE ALARM (pd-me6h). An alarm that cries wolf on a
# schedule trains the operator to ignore it, which is strictly worse than no
# alarm at all — and prod's very first armed run did exactly that, judging the
# static user registry on freshness. §4c proves it stopped; §4d and §4e prove it
# did not stop by going quiet.
#
# ── PROD-SAFE BY CONSTRUCTION ───────────────────────────────────────────────
# Its own instance name (never 'prod'), its own data volume, its own MinIO, its
# own podman secret, its own config dir, its own HTTP sink on 127.0.0.1. The
# systemd units are PREFIXED copies of the shipped templates (PKDUMP_UNIT_PREFIX)
# so enabling and firing them cannot touch pkdump-*@prod. Pushover and
# healthchecks.io are never contacted: PUSHOVER_API_URL and the ping URL both
# resolve to the local sink, and no real credential is read — PKDUMP_ALERTS_ENV
# redirects the host-wide alerts file to a throwaway one.
set -uo pipefail   # NOT -e: a failed assertion must be reported, not fatal

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

LITESTREAM_IMAGE=${LITESTREAM_IMAGE:-docker.io/litestream/litestream:latest}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=deploy/litestream-lib.sh
. "${REPO_DIR}/deploy/litestream-lib.sh"
# For pkdump_store_stamp_unit — this gate has to record the store its instance
# was built in, the same way setup.sh does. Sourcing does not activate anything.
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"

# Host ports, from the kernel rather than picked (pd-r0ri). One definition for
# every harness; see tests/lib/ports.sh for what a picked one costs.
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

# Ask before sleeping (pd-86er). §5.4 waits for a container to age past a grace,
# which is a condition and not a duration.
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"
# shellcheck source=tests/lib/objects.sh
. "${REPO_DIR}/tests/lib/objects.sh"

# Unique per checkout, like deploy/ci.sh's: several polecats run this gate from
# their own worktrees at the same time, and a shared name means run B's teardown
# destroys run A's volume mid-suite.
SUFFIX="$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)"
INSTANCE="${ALARM_INSTANCE:-alarm${SUFFIX}}"
case "$INSTANCE" in
	prod|"") echo "refusing to run the alarming gate against instance '${INSTANCE}'" >&2; exit 1 ;;
esac

# The unit-name prefix every unit this gate installs, enables and fires lives
# under. `pkdump-` is the operator's namespace and this gate never writes into
# it — that is what makes enabling timers here safe on the prod box.
P="pdalarm${SUFFIX}"
export PKDUMP_UNIT_PREFIX="$P"

VOLUME="pkdump-${INSTANCE}-data"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
SECRET_NAME="pkdump-${INSTANCE}-s3-bootstrap"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
QUADLET_DIR="${HOME}/.config/containers/systemd"
# The instance's own Quadlet unit. Named for the instance, so it cannot collide
# with the operator's — INSTANCE is refused above if it is 'prod'. Written in §2
# and removed by cleanup; see the comment there for why it has to exist at all.
INSTANCE_UNIT="${QUADLET_DIR}/pkdump-${INSTANCE}.container"

MINIO_CTR="pkdump-${INSTANCE}-minio"
LS_CTR="pkdump-${INSTANCE}-litestream"

# Ports are taken from the kernel, not picked. Fixed numbers fail two ways here:
# several polecats run this gate concurrently from their own worktrees, and this
# box already had an unrelated `python3 -m http.server` sitting on the first
# number chosen — which surfaced as "address already in use" three sections
# later, looking nothing like a port clash.
MINIO_PORT=${MINIO_PORT:-$(free_port)}
SINK_PORT=${SINK_PORT:-$(free_port)}

WORK=${WORK:-$(mktemp -d /tmp/pkdump-alarm.XXXXXX)}
SINK_LOG="${WORK}/sink.jsonl"
SINK_PID=""

# The ping path carries a random token so an assertion cannot match a stray
# request from anything else that happens to be talking to this port.
HC_TOKEN="hc-$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
PING_URL="http://127.0.0.1:${SINK_PORT}/${HC_TOKEN}"
# Layer 0's monitor. A DISTINCT token, not a variant of the one above: the two
# checks watch unrelated things, and alarm-status.sh treats a shared URL as a
# fault because that sharing is what masked a real outage on 2026-08-16.
UPTIME_TOKEN="up-$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
UPTIME_PING_URL="http://127.0.0.1:${SINK_PORT}/${UPTIME_TOKEN}"
PUSH_PATH="/pushover/messages.json"

# The host-wide alerts file, redirected. Ryan's real one is never read or
# written by this gate.
TEST_ALERTS_ENV="${WORK}/alerts-host.env"
export PKDUMP_ALERTS_ENV="$TEST_ALERTS_ENV"

# This gate's whole claim is "every layer FIRES", and it proves it by provoking
# the same failure repeatedly — which is precisely what pd-hqdt's repeat
# suppression exists to send once. Two runs of this gate on one box within 24h
# would otherwise assert a push that alert.sh correctly withheld. So the window
# is switched off here, both for what this shell invokes and — via the alerts
# file below, which is the only environment a systemd unit here inherits — for
# §6's shipped OnFailure= path. Suppression has its own gate, and it asserts
# the shipped default separately: tests/alarming/alert_suppress_test.sh.
export PKDUMP_ALERT_SUPPRESS_SECONDS=0
# Belt and braces: nothing may write to ~/.local/state either, whatever the
# window says.
export PKDUMP_ALERT_STATE_DIR="${WORK}/alert-state"

TENANTS=(alpha bravo)

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
		echo "KEEP=1 — leaving the test bed up. WORK=$WORK"
		echo "  kill ${SINK_PID:-}; podman rm -f --ignore ${LS_CTR} ${MINIO_CTR}"
		echo "  systemctl --user disable --now ${P}-backup-check@${INSTANCE}.timer ${P}-diskcheck.timer"
		echo "  rm -f ${SYSTEMD_USER_DIR}/${P}-* ${QUADLET_DIR}/${P}-litestream-${INSTANCE}.container ${INSTANCE_UNIT}"
		echo "  podman volume rm -f ${VOLUME}; podman secret rm ${SECRET_NAME}; rm -rf ${CONF_DIR} ${WORK}"
		return $rc
	fi
	[ -n "$SINK_PID" ] && kill "$SINK_PID" >/dev/null 2>&1
	# Disable before removing the files, or systemd keeps the symlinks around.
	systemctl --user disable --now "${P}-backup-check@${INSTANCE}.timer" >/dev/null 2>&1
	systemctl --user disable --now "${P}-diskcheck.timer" >/dev/null 2>&1
	systemctl --user stop "${P}-litestream-${INSTANCE}.service" >/dev/null 2>&1
	# Generated by the Quadlet generator from INSTANCE_UNIT, never started here.
	systemctl --user stop "pkdump-${INSTANCE}.service" >/dev/null 2>&1
	rm -f "${SYSTEMD_USER_DIR}/${P}-"* "${QUADLET_DIR}/${P}-litestream-${INSTANCE}.container" \
		"$INSTANCE_UNIT"
	systemctl --user daemon-reload >/dev/null 2>&1
	systemctl --user reset-failed "${P}-"'*' >/dev/null 2>&1
	podman rm -f --ignore "$LS_CTR" "$MINIO_CTR" "pkdump-${INSTANCE}-diverged" \
		"pkdump-${INSTANCE}-silent" "pkdump-${INSTANCE}-catchup" >/dev/null 2>&1
	podman volume rm -f "$VOLUME" >/dev/null 2>&1
	podman secret rm "$SECRET_NAME" >/dev/null 2>&1
	rm -rf "$CONF_DIR" "$WORK"
	return $rc
}
trap cleanup EXIT

# --- sink queries ----------------------------------------------------------
# Every assertion below is "what did the sink receive", never "what does the
# script say it would send".
# `grep -c` already prints 0 when nothing matches — it just exits 1 while doing
# it. An `|| echo 0` on the end therefore prints the count TWICE, which lands in
# the next $(( )) as a syntax error and silently zeroes every assertion after it.
sink_total() { wc -l <"$SINK_LOG" 2>/dev/null | tr -d ' '; }
sink_grep()  { grep -c -- "$1" "$SINK_LOG" 2>/dev/null || true; }
# Requests recorded after line N — so a section can assert on ITS OWN traffic.
sink_since() { tail -n "+$(( $1 + 1 ))" "$SINK_LOG" 2>/dev/null; }
since_grep() { sink_since "$1" | grep -c -- "$2" || true; }

GREEN_PING="\"path\": \"/${HC_TOKEN}\""
FAIL_PING="\"path\": \"/${HC_TOKEN}/fail\""
PUSH="\"path\": \"${PUSH_PATH}\""

marker_epoch() { cat "${MP}/.backup-last-ok" 2>/dev/null || echo none; }

# backup-check, invoked exactly as its systemd unit invokes it, with the sink
# standing in for the two external services.
#
# The correspondence re-ask window is shortened to one second: §4d and §4e
# provoke divergences that are PERMANENT, and sitting through the production
# window twice over would buy nothing. Where the window is the point — a lag
# that resolves itself — the gate uses run_check_default_grace instead.
# PKDUMP_BACKUP_SIDECAR_CTR: the tenant leg reads replication positions out of the
# sidecar's own `replica sync` log lines, and this gate's replicator is the bare
# detached container started in §2 rather than the deployed Quadlet. Without this
# the tenant leg finds no position, treats every tenant as "too new to judge", and
# the gate goes green having verified nothing about tenants — which is exactly how
# it caught the first draft of that change.
run_check() { PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_BACKUP_CORRESPONDENCE_GRACE_SECONDS=1 \
	PKDUMP_BACKUP_SIDECAR_CTR="$LS_CTR" \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" 2>&1; }
run_check_default_grace() { PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_BACKUP_SIDECAR_CTR="$LS_CTR" \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" 2>&1; }

# ── 1. the recorder ─────────────────────────────────────────────────────────
log "1. local HTTP sink standing in for healthchecks.io + Pushover"
: >"$SINK_LOG"
python3 "${SCRIPT_DIR}/sink.py" "$SINK_PORT" "$SINK_LOG" >"${WORK}/sink.err" 2>&1 &
SINK_PID=$!
for _ in $(seq 40); do
	curl -fsS -m 2 "http://127.0.0.1:${SINK_PORT}/ready" >/dev/null 2>&1 && break
	sleep 0.25
done
check "the sink is up and recording" "1" "$(sink_grep '"path": "/ready"')"
# Every later assertion is "what did the sink receive", so a sink that dies
# mid-run turns real failures into fake ones. Assert it is still alive whenever
# a section expects traffic.
sink_alive() { kill -0 "$SINK_PID" 2>/dev/null && echo alive || { sed 's/^/    sink: /' "${WORK}/sink.err"; echo dead; }; }
: >"$SINK_LOG"
echo "  ping URL ${PING_URL}"

# ── 2. an instance shaped like a deployed one, with a real S3 to check ──────
log "2. instance '${INSTANCE}': volume, tenants, MinIO, sidecar, config"
AKID="alarm$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
SECRET_KEY="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"

export LITESTREAM_S3_BUCKET=pkdump-alarm
export LITESTREAM_S3_REGION=us-west-2
export LITESTREAM_S3_PATH="alarm/${INSTANCE}/tenants"
export LITESTREAM_TENANTS_DIR=/data/tenants
# The user registry (pd-nd6w) is part of the replicated set, and the SHIPPED
# litestream.yml this gate mounts names it — an unset LITESTREAM_REGISTRY_DB is
# fatal at sidecar startup by design (tests/litestream/run.sh §4c). Beside the
# tenants prefix, never inside it, exactly as deploy/setup.sh writes it.
export LITESTREAM_REGISTRY_DB=/data/registry.sqlite
export LITESTREAM_S3_REGISTRY_PATH="alarm/${INSTANCE}/registry.sqlite"
# backup-check.sh runs its `litestream ltx` container with no --network, exactly
# as it does in production, so MinIO has to be reachable at the address
# host.containers.internal names (same reasoning as tests/litestream/drill.sh).
export LITESTREAM_S3_ENDPOINT="http://host.containers.internal:${MINIO_PORT}"
export AWS_PROFILE=pkdump

podman rm -f --ignore "$MINIO_CTR" "$LS_CTR" >/dev/null 2>&1
mkdir -p "${WORK}/minio"
podman run -d --name "$MINIO_CTR" -p "${MINIO_PORT}:9000" \
	-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET_KEY" \
	-v "${WORK}/minio:/data:Z" "$MINIO_IMAGE" server /data >/dev/null
for _ in $(seq 60); do
	curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null 2>&1 && break
	sleep 1
done
check "minio is up" "0" "$(curl -fsS -o /dev/null "http://127.0.0.1:${MINIO_PORT}/minio/health/live"; echo $?)"
# Litestream does not create buckets, and a missing one surfaces as NoSuchBucket
# from the freshness query — which reads identically to the broken-creds failure
# the gate deliberately provokes in §4b. Create it up front so the two cases
# stay distinguishable.
mc() { podman run --rm -e "MC_HOST_s=http://${AKID}:${SECRET_KEY}@host.containers.internal:${MINIO_PORT}" "$MC_IMAGE" "$@"; }
mc mb --ignore-existing "s/${LITESTREAM_S3_BUCKET}" >/dev/null 2>&1
# Through the verified listing (pd-cxq4): `mc ls` with its stderr thrown away
# reports a store it could not reach and a store with nothing in it the same
# way, and only one of those is an answer to "does the bucket exist".
BUCKETS="$(object_store_ls "" mc ls s/)" || BUCKETS=""
check "the bucket exists" "1" "$(grep -c "$LITESTREAM_S3_BUCKET" <<<"$BUCKETS" || true)"

podman secret rm "$SECRET_NAME" >/dev/null 2>&1
mkdir -p "${CONF_DIR}/aws"
chmod 700 "$CONF_DIR" "${CONF_DIR}/aws"
# MinIO cannot assume a role, so the profile carries the static test key. The
# assume-role path is covered by tests/litestream/drill.sh (DRILL_REAL_S3=1).
printf '[profile pkdump]\nregion=%s\n' "$LITESTREAM_S3_REGION" >"${CONF_DIR}/aws/config"
printf '[pkdump]\naws_access_key_id=%s\naws_secret_access_key=%s\n' "$AKID" "$SECRET_KEY" \
	| podman secret create "$SECRET_NAME" - >/dev/null

# Each override re-points ONE prefix and leaves the other alone, so a section
# provokes exactly one failure: §4a wants a tenant that never reached S3, not a
# simultaneous registry failure that would answer the assertion first (the
# registry is judged before any tenant), and §4d/§4e want the reverse.
write_litestream_env() { # write_litestream_env [tenants-path] [registry-path]
	cat >"${CONF_DIR}/litestream.env" <<EOF
LITESTREAM_S3_BUCKET=${LITESTREAM_S3_BUCKET}
LITESTREAM_S3_REGION=${LITESTREAM_S3_REGION}
LITESTREAM_S3_PATH=${1:-$LITESTREAM_S3_PATH}
LITESTREAM_TENANTS_DIR=/data/tenants
LITESTREAM_REGISTRY_DB=${LITESTREAM_REGISTRY_DB}
LITESTREAM_S3_REGISTRY_PATH=${2:-$LITESTREAM_S3_REGISTRY_PATH}
LITESTREAM_S3_ENDPOINT=${LITESTREAM_S3_ENDPOINT}
AWS_PROFILE=pkdump
EOF
	chmod 600 "${CONF_DIR}/litestream.env"
}
write_alerts_env() { # write_alerts_env <ping-url> [max-age-hours]
	cat >"${CONF_DIR}/alerts.env" <<EOF
PKDUMP_BACKUP_PING_URL=${1}
PKDUMP_BACKUP_MAX_AGE_HOURS=${2:-36}
# Layer 0 points at the same sink but a DIFFERENT path, which is the point:
# alarm-status.sh fails an instance whose uptime and backup checks are the same
# URL, because sharing one is what let a four-day-stuck backup alarm swallow a
# real outage on 2026-08-16. The probe target is the sink's own /ready, so a
# heartbeat run in this gate gets a genuine 200 without needing an app.
PKDUMP_UPTIME_PING_URL=${UPTIME_PING_URL}
PKDUMP_HEARTBEAT_URL=http://127.0.0.1:${SINK_PORT}/ready
# Also here, not just in run_check(), so the run §8 drives through the SHIPPED
# systemd unit judges tenants too. A unit run that quietly skipped them would
# still ping green, and "the unit works" would mean less than it looks like.
PKDUMP_BACKUP_SIDECAR_CTR=${LS_CTR}
EOF
	chmod 600 "${CONF_DIR}/alerts.env"
}
write_litestream_env
write_alerts_env "$PING_URL"

# The host-wide file, redirected: fake Pushover creds + the sink endpoint, so
# every push this gate provokes lands in the log instead of on a phone.
cat >"$TEST_ALERTS_ENV" <<EOF
PUSHOVER_TOKEN=alarmgate-token
PUSHOVER_USER=alarmgate-user
PUSHOVER_API_URL=http://127.0.0.1:${SINK_PORT}${PUSH_PATH}
PKDUMP_DISK_THRESHOLD=90
PKDUMP_DISK_PATH=${HOME}
PKDUMP_ALERT_SUPPRESS_SECONDS=0
EOF

podman volume rm -f "$VOLUME" >/dev/null 2>&1
podman volume create "$VOLUME" >/dev/null
MP="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"

# This gate builds its instance by hand rather than through setup.sh, so it has
# to leave behind the one artefact of setup.sh's that outlives this shell: the
# instance's own Quadlet unit, carrying the store the volume was just created in
# (pd-fite/pd-rf7c). §8 runs the checker under SYSTEMD, and a systemd unit
# inherits none of this shell's environment — not PKDUMP_STORE_ROOT, not the
# podman shim on PATH. backup-check.sh recovers the store from this file
# (pkdump_store_adopt_instance), which is exactly how a real alternate-store
# instance survives the same trip. Without it the checker looks in the default
# store, cannot find the volume, and reports a backup failure that is really a
# store mismatch. Generated from the shipped template and stamped the way
# setup.sh stamps it; never started, and a no-op on a box with no alternate
# store — which is prod.
mkdir -p "$QUADLET_DIR"
sed -e "s|{{INSTANCE}}|${INSTANCE}|g" -e "s|{{PORT}}:8080|$(free_port):8080|" \
	"${REPO_DIR}/deploy/pkdump.container" >"$INSTANCE_UNIT"
pkdump_store_stamp_unit "$INSTANCE_UNIT"
systemctl --user daemon-reload
mkdir -p "${MP}/tenants"
for t in "${TENANTS[@]}"; do
	sqlite3 -cmd '.timeout 5000' "${MP}/tenants/${t}.sqlite" \
		"PRAGMA journal_mode=WAL;
		 CREATE TABLE collection (id INTEGER PRIMARY KEY, tenant TEXT);
		 INSERT INTO collection (tenant) VALUES ('${t}');" >/dev/null
done
check "the data volume carries ${#TENANTS[@]} tenants" "${TENANTS[*]}" \
	"$(tenants_on_volume "$MP" | sort | tr '\n' ' ' | sed 's/ $//')"
# The registry sits at the data ROOT, outside tenants/, so the glob above cannot
# see it and it is replicated as itself. A deployed instance has one; without it
# here, backup-check takes its "no registry on this instance yet" branch and the
# whole registry half of Layer 1 would go unexercised by this gate.
sqlite3 -cmd '.timeout 5000' "${MP}/registry.sqlite" \
	"PRAGMA journal_mode=WAL;
	 CREATE TABLE users (handle TEXT PRIMARY KEY, database_id TEXT NOT NULL);
	 INSERT INTO users (handle, database_id) VALUES ('alpha', 'alpha'), ('bravo', 'bravo');" >/dev/null
check "and a user registry beside them, not among them" "yes" \
	"$([ -e "${MP}/registry.sqlite" ] && ! grep -qx registry <<<"$(tenants_on_volume "$MP")" && echo yes || echo no)"

# The SHIPPED sidecar config, replicating for real, so the freshness query the
# checker runs is a real S3 query against real replica data.
podman run -d --name "$LS_CTR" --user 0 \
	-v "${MP}:/data:Z" -v "${REPO_DIR}/deploy/litestream.yml:/etc/litestream.yml:ro,Z" \
	-v "${CONF_DIR}/aws/config:/aws/config:ro" \
	--secret "${SECRET_NAME},type=mount,target=/aws/credentials" \
	-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
	-e AWS_PROFILE -e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET \
	-e LITESTREAM_S3_PATH -e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	-e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_REGISTRY_PATH \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null

# awaiting_replica <url> — poll until that replica holds real transaction data.
#
# "Holds data" is a TIMESTAMP, not any output at all. `ltx` prints its column
# header even for a prefix that has never been written to, so `grep -q .` here
# said "replicating" while the sidecar had exited at startup and nothing had
# ever reached the bucket (measured, pd-nt1k) — which is how a dead sidecar
# reached §3 wearing a green §2. Match what backup-check's ltx_newest parses.
awaiting_replica() { # awaiting_replica <replica-url>
	local url="$1" deadline=$(( SECONDS + 90 ))
	while [ "$SECONDS" -lt "$deadline" ]; do
		if grep -qE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z' \
			<<<"$(ltx_all "$url")"
		then return 0; fi
		# One second, not three: the body of this loop is itself a `podman run`
		# that takes the better part of a second, so a three-second interval was
		# ~1.5s of pure latency on every wait and this function is called once per
		# tenant (pd-86er). The 90s bound is unchanged — nothing here waits less
		# long for the condition, it just notices sooner when it holds.
		sleep 1
	done
	return 1
}

ltx_all() { # ltx_all <replica-url> — what backup-check's own query sees
	podman run --rm --user 0:0 \
		-v "${CONF_DIR}/aws/config:/aws/config:ro" \
		--secret "${SECRET_NAME},type=mount,target=/aws/credentials" \
		-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
		-e AWS_PROFILE=pkdump "$LITESTREAM_IMAGE" ltx -level all "$1" 2>/dev/null
}
# The two halves of the correspondence pair, read the way backup-check reads
# them: the furthest TXID in the replica, and the furthest Litestream has
# ingested locally. Both are 16 hex characters, so the largest token in the
# listing is the largest TXID.
replica_max_txid() { # replica_max_txid <replica-url>
	ltx_all "$1" | grep -oE '\b[0-9a-fA-F]{16}\b' | tr 'A-F' 'a-f' | LC_ALL=C sort | tail -n1
}
registry_local_txid() {
	podman run --rm --user 0:0 -v "${MP}:/data:ro" \
		-v "${REPO_DIR}/deploy/litestream.yml:/etc/litestream.yml:ro" \
		-e LITESTREAM_TENANTS_DIR -e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_BUCKET \
		-e LITESTREAM_S3_PATH -e LITESTREAM_S3_REGISTRY_PATH -e LITESTREAM_S3_REGION \
		-e LITESTREAM_S3_ENDPOINT \
		"$LITESTREAM_IMAGE" status -json "$LITESTREAM_REGISTRY_DB" 2>/dev/null \
		| grep -oE '\b[0-9a-fA-F]{16}\b' | tr 'A-F' 'a-f' | head -n1
}

# awaiting_correspondence <db-basename> — poll until the sidecar reports this
# database's replica CAUGHT UP with it, not merely written to once.
#
# "An object exists in the bucket" was a sufficient readiness condition while
# tenants were judged on freshness. Under correspondence it is not: the objects
# from the FIRST write appear while a later commit is still in flight, so §3 ran
# against a genuinely-behind tenant and its healthy-path assertions failed —
# correctly, and for a reason that was this gate's timing rather than the
# checker's behaviour. Waiting for the condition §3 actually depends on removes
# the race instead of sleeping at it.
awaiting_correspondence() { # awaiting_correspondence <db-basename>
	local base="$1" deadline=$(( SECONDS + 90 )) r d
	while [ "$SECONDS" -lt "$deadline" ]; do
		read -r r d <<<"$(podman logs --since 120s "$LS_CTR" 2>&1 |
			grep -F "db=${base}" | tail -n1 |
			sed -n 's/.*txid\.replica=\([0-9a-f]\{16\}\).*txid\.db=\([0-9a-f]\{16\}\).*/\1 \2/p')"
		if [ -n "${d:-}" ] && [ "${r:-}" = "${d:-}" ]; then return 0; fi
		sleep 1
	done
	return 1
}

replicating=0
for t in "${TENANTS[@]}"; do
	awaiting_replica "$(tenant_replica_url "$t")" \
		&& awaiting_correspondence "${t}.sqlite" \
		&& replicating=$((replicating + 1))
done
check "both tenants are replicating to S3" "2" "$replicating"
# Separately, because losing it is the failure the tenant loop cannot see: every
# tenant stays fresh and the table saying whose database is whose is gone.
check "the user registry is replicating to S3 too" "yes" \
	"$(awaiting_replica "$(registry_replica_url)" && echo yes || echo no)"

# ── 3. Layer 1 GREEN: a fresh backup pings the off-box monitor ──────────────
log "3. Layer 1 fires GREEN — the monitor is pinged only when S3 is actually fresh"
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "backup-check exits 0 when every replica is fresh" "0" "$RC"
check "the sink is still recording" "alive" "$(sink_alive)"
check "the monitor received its heartbeat" "1" "$(since_grep "$BEFORE" "$GREEN_PING")"
check "and NOT the /fail trip" "0" "$(since_grep "$BEFORE" "$FAIL_PING")"
check "no Pushover push on a healthy run" "0" "$(since_grep "$BEFORE" "$PUSH")"
check "Layer 3's freshness marker was written" "yes" \
	"$([ -f "${MP}/.backup-last-ok" ] && echo yes || echo no)"
GREEN_MARKER="$(marker_epoch)"

# ── 4. Layer 1 RED: the two silent failures, each made to page ──────────────
log "4a. Layer 1 fires RED — a tenant that never reached S3 (the pd-fof4 silent mode)"
# Nothing errors when a tenant is simply absent from the bucket: no unit fails,
# no log line appears. Re-point the instance at an empty prefix to reproduce it.
write_litestream_env "alarm/${INSTANCE}/never-replicated"
write_alerts_env "$PING_URL" 0
BEFORE=$(sink_total)
GREEN_MARKER=$(marker_epoch)   # re-baseline: assert THIS run did not refresh it
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "backup-check exits non-zero" "1" "$RC"
check "it says NOT backed up" "1" "$(printf '%s' "$OUT" | grep -c 'NOT backed up' || true)"
check "the dead-man's switch was TRIPPED immediately (/fail)" "1" "$(since_grep "$BEFORE" "$FAIL_PING")"
check "a Pushover push carried the detail" "1" "$(since_grep "$BEFORE" "$PUSH")"
check "the push names the instance" "1" "$(since_grep "$BEFORE" "backup STALE (${INSTANCE})")"
check "no green heartbeat was sent" "0" "$(since_grep "$BEFORE" "$GREEN_PING")"
check "the freshness marker was NOT refreshed" "$GREEN_MARKER" "$(marker_epoch)"

log "4b. Layer 1 fires RED — broken creds/network (the 2026-08-08 silent mode)"
# The sidecar keeps saying 'active' and logging snapshots while nothing reaches
# S3. From the checker's side that is a failing freshness QUERY, so kill MinIO.
write_litestream_env
write_alerts_env "$PING_URL"
podman stop -t 2 "$MINIO_CTR" >/dev/null 2>&1
GREEN_MARKER=$(marker_epoch)   # re-baseline: assert THIS run did not refresh it
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | tail -n 3 | sed 's/^/    /'
check "backup-check exits non-zero" "1" "$RC"
check "it names the query failure" "1" "$(printf '%s' "$OUT" | grep -c 'litestream ltx failed' || true)"
check "the dead-man's switch was TRIPPED (/fail)" "1" "$(since_grep "$BEFORE" "$FAIL_PING")"
check "a Pushover push carried the detail" "1" "$(since_grep "$BEFORE" "$PUSH")"
check "the freshness marker was NOT refreshed" "$GREEN_MARKER" "$(marker_epoch)"
podman start "$MINIO_CTR" >/dev/null 2>&1
for _ in $(seq 60); do
	curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null 2>&1 && break
	sleep 1
done

# A TRANSIENT OUTAGE MUST NOT LEAVE THE REGISTRY ALARMING (pd-me6h). Measured
# here, on this bed: the outage above left Litestream holding a checkpoint it
# could not upload, so the moment MinIO returned the registry read local txid 2
# against replica txid 1 — a real lag, on a database nobody had written to. It
# cleared itself within 30s, at Litestream's next compaction tick.
#
# That is precisely what the checker's re-ask window is sized for, and this is
# the assertion that keeps the two in step: run it at the SHIPPED window and it
# has to come back green. Shorten the window and this goes red — which is how
# the number stays honest rather than becoming decoration.
OUT="$(run_check_default_grace)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "a transient S3 outage does not leave the registry alarming" "0" "$RC"
check "the checker waited the lag out rather than paging over it" "1" \
	"$(printf '%s' "$OUT" | grep -c 'the user registry OK — replica in correspondence' || true)"
GREEN_MARKER="$(marker_epoch)"

# ── 4c-e. THE REGISTRY IS NOT JUDGED ON AGE (pd-me6h) ───────────────────────
# prod's backup alarming was armed for the first time on 2026-08-11, and the
# very first thing it did was fire a FALSE alarm: the registry's newest S3
# object was 65h old, past the 36h freshness threshold, while Litestream was
# demonstrably replicating it every two seconds with no errors.
#
# Both facts were true. The registry is STATIC by design — handle ->
# database_id, changed only when a tenant is added, removed or renamed — and a
# database nobody writes produces no new S3 objects however diligently the
# sidecar syncs. Freshness is simply the wrong question about it, and a bigger
# threshold only moves the false positive further out.
#
# These three sections are the replacement test in both directions: it must
# stop firing on age (4c), and it must still fire on a replica that is missing
# (4d) or behind (4e). An alarm never observed failing is not known to work.
log "4c. the registry PASSES an age it would have failed — correspondence, not freshness (pd-me6h)"
# A threshold BELOW every possible age: the same relation prod was in at 65h >
# 36h, without waiting 65 hours for it. The tenants are judged by that
# threshold and must fail on it; the registry is judged by correspondence and
# must not be reached by it at all. The registry is checked FIRST, so before
# this fix the run died on the registry and never got to a tenant.
write_litestream_env
write_alerts_env "$PING_URL" -1
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "the registry is judged on correspondence with its replica" "1" \
	"$(printf '%s' "$OUT" | grep -c 'the user registry OK — replica in correspondence' || true)"
check "and it says so at a threshold every age exceeds" "0" \
	"$(printf '%s' "$OUT" | grep -c 'STALE — the user registry' || true)"
# This run passed (every tenant still fresh, registry in correspondence), so it
# legitimately reached mark_fresh and refreshed the on-disk marker. Re-baseline
# before the failing runs below (the touched tenant, then 4d/4e), or their "was
# NOT refreshed" checks compare against a marker value from BEFORE this run
# instead of after it — and fail by however many seconds this run took, not
# because anything failing wrote the marker.
GREEN_MARKER="$(marker_epoch)"
# The other half of the constraint: the check must still FIRE. This is the
# assertion that keeps the fix from being "the check got quieter".
#
# The trigger is CORRESPONDENCE — the same question pd-me6h settled for the
# registry, extended to tenants on 2026-08-16 after two proxies in a row false-
# paged. A tenant nobody edits produces no S3 objects, exactly as the registry
# does not; judged on replica AGE that paged Ryan three times for a healthy
# backup, and judged on file MTIME it paged again, because `.sqlite` mtime moves
# on checkpoint and on open.
#
# This gate used to `touch` a tenant database and require a failure. That is the
# mtime proxy written down as a test, and it now deliberately passes: **a touch
# is not a lost write.** Nothing was committed, so nothing is missing from S3,
# and restoring the replica still reproduces the database byte for byte.
#
# What must fail is a real transaction that cannot reach S3. Stop the object
# store, commit to a tenant, and let the sidecar tick: it keeps ingesting locally
# so txid.db advances while txid.replica cannot, which is "writes are not
# reaching S3" stated in the only terms that actually mean it.
LAG_TENANT="$(ls "${MP}/tenants" | head -1)"; LAG_TENANT="${LAG_TENANT%.sqlite}"
podman stop -t 2 "$MINIO_CTR" >/dev/null 2>&1
sqlite3 -cmd '.timeout 5000' "${MP}/tenants/${LAG_TENANT}.sqlite" \
	"INSERT INTO collection (tenant) VALUES ('${LAG_TENANT}-unreplicated');" >/dev/null
# Wait for the sidecar to publish a DIVERGED pair rather than sleeping a guess:
# the two txids compared in the shell, because grep -E has no back-reference
# negation and a regex that looked like it did would quietly always match.
tenant_pair() { # -> "<replica> <db>" from the sidecar's newest line for this tenant
	podman logs --since 120s "$LS_CTR" 2>&1 | grep -F "db=${LAG_TENANT}.sqlite" | tail -n1 |
		sed -n 's/.*txid\.replica=\([0-9a-f]\{16\}\).*txid\.db=\([0-9a-f]\{16\}\).*/\1 \2/p'
}
for _ in $(seq 40); do
	read -r _r _d <<<"$(tenant_pair)"
	[ -n "${_d:-}" ] && [ "${_r:-}" != "${_d:-}" ] && break
	sleep 1
done
check "the sidecar reports the tenant's replica behind its database" "behind" \
	"$(read -r _r _d <<<"$(tenant_pair)"; [ -n "${_d:-}" ] && [ "${_r:-}" != "${_d:-}" ] && echo behind || echo "caught-up(${_r:-?}/${_d:-?})")"
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
# Not asserted here: that the STALE line names the TENANT. With the object store
# down the registry is judged first and fails first, so the run never reaches the
# tenant loop. The tenant's own divergence branch is isolated below instead, with
# S3 healthy — the two halves of the evidence are "litestream really does report
# a diverged pair when writes cannot land" (above) and "a diverged pair really
# does fail the tenant leg" (below).
check "the run fails while a committed tenant write cannot reach S3" "1" "$RC"

# Put the object store back and let replication catch up: 4d and 4e both need a
# live S3, and a tenant left diverged would fail every later run for a reason
# that has nothing to do with what those sections are testing.
podman start "$MINIO_CTR" >/dev/null 2>&1
for _ in $(seq 60); do
	read -r _r _d <<<"$(tenant_pair)"
	[ -n "${_d:-}" ] && [ "${_r:-}" = "${_d:-}" ] && break
	sleep 1
done
check "and it recovers once S3 is back" "0" "$(run_check >/dev/null 2>&1; echo $?)"
# That recovery run passed, so it legitimately reached mark_fresh. Re-baseline,
# or 4d's "was NOT refreshed" compares against a value from before it and fails
# by however long this section took rather than because a failing run wrote it.
GREEN_MARKER="$(marker_epoch)"

log "4d. the registry fires RED — its replica is not there at all"
# Correspondence has to be a test, not an exemption. Point the registry at a
# prefix nothing has ever written to; with the grace window closed (0h), a
# registry with no replica is a registry that is not backed up.
write_litestream_env "$LITESTREAM_S3_PATH" "alarm/${INSTANCE}/no-such-registry"
write_alerts_env "$PING_URL" 0
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "backup-check exits non-zero" "1" "$RC"
check "and it says the registry is NOT backed up" "1" \
	"$(printf '%s' "$OUT" | grep -c 'STALE — the user registry: no replica data' || true)"
check "the dead-man's switch was TRIPPED (/fail)" "1" "$(since_grep "$BEFORE" "$FAIL_PING")"
check "a Pushover push carried the detail" "1" "$(since_grep "$BEFORE" "$PUSH")"
check "the freshness marker was NOT refreshed" "$GREEN_MARKER" "$(marker_epoch)"

log "4e. the registry fires RED — its replica has fallen BEHIND the local database"
# The failure correspondence exists to catch, built for real rather than
# described: freeze a copy of the replica at today's TXID, write to the
# registry, let the live replica advance past it, then point the checker at the
# frozen copy. Local and replica now genuinely disagree, with no clock involved
# — the frozen copy's objects are seconds old.
write_litestream_env
write_alerts_env "$PING_URL"
FROZEN_PATH="alarm/${INSTANCE}/registry-frozen.sqlite"
REG_URL="$(registry_replica_url)"
BEFORE_LOCAL="$(registry_local_txid)"
mc mirror --quiet "s/${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_REGISTRY_PATH}" \
	"s/${LITESTREAM_S3_BUCKET}/${FROZEN_PATH}" >/dev/null 2>&1
# Addressed through the shipped derivation, not a hand-built URL, so the frozen
# copy is read exactly the way backup-check will read it.
FROZEN_URL="$( LITESTREAM_S3_REGISTRY_PATH="$FROZEN_PATH"; registry_replica_url )"
FROZEN_TXID="$(replica_max_txid "$FROZEN_URL")"
ltx_all "$FROZEN_URL" | sed 's/^/    frozen: /'
# It has to be a REAL replica holding real transactions — that is what separates
# this section from §4d. An empty prefix would fail for the other reason.
check "the frozen copy holds transactions of its own" "yes" \
	"$([ -n "$FROZEN_TXID" ] && echo yes || echo no)"

sqlite3 -cmd '.timeout 5000' "${MP}/registry.sqlite" \
	"INSERT INTO users (handle, database_id) VALUES ('charlie', 'charlie');" >/dev/null
# Wait for the write to land on BOTH sides of the live pair. Only then is the
# frozen copy demonstrably behind a database that is itself correctly backed up
# — so the failure below can only be the divergence, never a slow sidecar.
LOCAL_AFTER=""; LIVE_AFTER=""
for _ in $(seq 60); do
	LOCAL_AFTER="$(registry_local_txid)"
	LIVE_AFTER="$(replica_max_txid "$REG_URL")"
	[ -n "$LOCAL_AFTER" ] && [ "$LOCAL_AFTER" != "$BEFORE_LOCAL" ] \
		&& [ "$LIVE_AFTER" = "$LOCAL_AFTER" ] && break
	sleep 2
done
check "the write reached the local database and its live replica alike" "$LOCAL_AFTER" "$LIVE_AFTER"
check "and left the frozen copy behind them" "yes" \
	"$([ -n "$FROZEN_TXID" ] && [ "$FROZEN_TXID" != "$LOCAL_AFTER" ] \
		&& [ "$(printf '%s\n%s\n' "$FROZEN_TXID" "$LOCAL_AFTER" | LC_ALL=C sort | head -n1)" = "$FROZEN_TXID" ] \
		&& echo yes || echo no)"

write_litestream_env "$LITESTREAM_S3_PATH" "$FROZEN_PATH"
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "backup-check exits non-zero" "1" "$RC"
check "and it says the replica is BEHIND the local database" "1" \
	"$(printf '%s' "$OUT" | grep -c 'its replica is BEHIND the local database' || true)"
check "it names both positions" "1" \
	"$(printf '%s' "$OUT" | grep -c "S3 holds up to txid ${FROZEN_TXID}, the local database is at ${LOCAL_AFTER}" || true)"
check "the dead-man's switch was TRIPPED (/fail)" "1" "$(since_grep "$BEFORE" "$FAIL_PING")"
check "a Pushover push carried the detail" "1" "$(since_grep "$BEFORE" "$PUSH")"
check "the freshness marker was NOT refreshed" "$GREEN_MARKER" "$(marker_epoch)"

# Back to the live replica: the verdict has to follow the replica, not the
# mutation. A registry that is in correspondence again passes again, at the
# new TXID, with the freshness threshold left at its default.
write_litestream_env
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
check "pointed back at the live replica it is green again" "0" "$RC"
# Anchored on "the user registry", because tenants are judged on correspondence
# too now and their OK line carries the same phrase — an unanchored count read 2
# the moment a tenant happened to sit at the same txid, which is a test that was
# only ever passing by accident.
check "in correspondence at the NEW txid" "1" \
	"$(printf '%s' "$OUT" | grep -c "the user registry OK — replica in correspondence at txid ${LOCAL_AFTER}" || true)"
check "and the heartbeat resumes" "1" "$(since_grep "$BEFORE" "$GREEN_PING")"
GREEN_MARKER="$(marker_epoch)"

# ── 5. THE SKIP (pd-1717, then pd-7f46): unarming costs the PING, not the ───
#      VERIFICATION
log "5. the skip is gone — no monitor still verifies against S3 (pd-1717/pd-7f46)"
# This is the bug that let alarming look armed while being off: with no ping URL
# the checker printed 'skipping', exited 0, and every dashboard read green —
# having asked S3 nothing at all. What is gone is the SKIP, not the exit code:
# an unarmed monitor costs the ping and nothing else, so the freshness query
# still runs and a stale replica still fails (proved in the arm below, and in
# tests/litestream/drill.sh §6). "Is this instance armed?" is a different
# question and alarm-status.sh answers it — see §8.
write_alerts_env ""
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "unset ping URL still verifies -> zero exit" "0" "$RC"
check "it does not use the word 'skipping'" "0" "$(printf '%s' "$OUT" | grep -ci 'skipping' || true)"
# One judgement per database it is responsible for, not "at least one line". A
# count that only proved SOMETHING was asked would still read green if the
# registry dropped out of the checked set, which is the whole failure pd-nd6w
# added it to catch — so the registry is counted SEPARATELY, by its own verdict,
# which since pd-me6h is a different sentence because it is a different test.
check "it asked S3 about every tenant anyway" "${#TENANTS[@]}" \
	"$(printf '%s' "$OUT" | grep -cE "^backup-check: tenant .* OK — replica" || true)"
check "and about the registry" "1" \
	"$(printf '%s' "$OUT" | grep -c 'the user registry OK — replica in correspondence' || true)"
check "and it says the dead-man's switch is NOT armed" "1" \
	"$(printf '%s' "$OUT" | grep -c 'NOT armed' || true)"
check "and nothing at all was sent" "0" "$(( $(sink_total) - BEFORE ))"

# ── the tenant leg's own two faults, proved rather than assumed ─────────────
# Tenants are judged on CORRESPONDENCE against the sidecar's `replica sync`
# positions, so the two ways that can fail need to be seen failing. Both are
# driven deterministically, because the alternative — waiting for a real stall —
# is a gate that takes hours and still proves nothing on a good day.
#
# 1. The sidecar is not reporting on this database at all. Pointed at a
#    container that does not exist, which is what a dead sidecar and a tenants
#    glob that stopped matching both look like from here. This is the fault
#    worth the most: it means writes are reaching nothing.
#    The grace is closed as well: a database that was genuinely created moments
#    ago is allowed to have no position yet, and every tenant in this gate is
#    seconds old, so without this the newborn allowance answers first and the
#    fault never gets asked about.
#    Through a fixture CONF_DIR, not an env override: alerts.env is sourced with
#    `set -a` and names PKDUMP_BACKUP_SIDECAR_CTR, so it CLOBBERS the environment
#    rather than deferring to it. An env-only version of this gate passed while
#    silently testing the real sidecar — the exit code was 1 for an unrelated
#    reason and only the message assertion noticed.
NOCTR_CONF="${WORK}/conf-no-sidecar"
mkdir -p "$NOCTR_CONF"
cp "${CONF_DIR}/litestream.env" "$NOCTR_CONF/"
cp -a "${CONF_DIR}/aws" "$NOCTR_CONF/" 2>/dev/null || true
sed "s|^PKDUMP_BACKUP_SIDECAR_CTR=.*|PKDUMP_BACKUP_SIDECAR_CTR=no-such-sidecar-${SUFFIX}|" \
	"${CONF_DIR}/alerts.env" >"${NOCTR_CONF}/alerts.env"
OUT="$(PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_CONF_DIR="$NOCTR_CONF" \
	PKDUMP_BACKUP_SIDECAR_GRACE_SECONDS=-1 \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" 2>&1)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "a sidecar reporting nothing fails the tenant leg" "1" "$RC"
check "and it says no replication position was reported" "1" \
	"$(printf '%s' "$OUT" | grep -c 'has not reported a replication position' || true)"

# 2. The sidecar is reporting, but its newest position is older than the grace —
#    replication stopped while the process stayed up, the exact silent mode
#    Layer 1 exists for. A negative grace makes every position too old, so the
#    branch is exercised without needing a sidecar that has genuinely stalled.
OUT="$(PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_BACKUP_SIDECAR_CTR="$LS_CTR" PKDUMP_BACKUP_SIDECAR_GRACE_SECONDS=-1 \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" 2>&1)"; RC=$?
check "a stalled sidecar position fails the tenant leg" "1" "$RC"
check "and it says replication has stopped" "1" \
	"$(printf '%s' "$OUT" | grep -c 'Replication has stopped' || true)"

# 3. The sidecar is reporting, recently, and its two txids DISAGREE — the replica
#    is behind the database. This is the fault the whole correspondence design is
#    built around, and the §4c block above cannot isolate it: killing the object
#    store fails the registry first, so the run ends before any tenant is judged.
#
#    So the position comes from a stand-in container that publishes exactly one
#    line in the sidecar's format. That is not a mock of the thing under test —
#    `judge_correspondence` and its txid comparison run for real, on real tenant
#    files, against a live S3. Only the sidecar's ACCOUNT of where it stands is
#    substituted, and §4c above is what proves litestream really does say this
#    when writes cannot land.
DIVERGED_CTR="pkdump-${INSTANCE}-diverged"
DIVERGED_LINE="time=$(date -u +%Y-%m-%dT%H:%M:%S.000Z) level=INFO msg=\"replica sync\" system=store db=${LAG_TENANT}.sqlite replica=s3 txid.replica=0000000000000001 txid.db=00000000000000ff"
#    `--entrypoint sh` on an image this gate already pulls, rather than adding
#    alpine as a dependency for one echo. The mc image runs `mc` by default, so
#    without the override the shell command arrives as mc arguments and the
#    container logs a usage error instead of the line — which looks exactly like
#    a sidecar that said nothing, and sends this gate down the wrong branch.
#    It prints the line and EXITS. `podman logs` reads a stopped container's
#    output just as well as a running one's, so there is nothing to keep alive —
#    and keeping it alive would have meant a `sleep` long enough to trip the
#    harness's own no-long-sleeps gate (pd-86er), which is right to object even
#    though this one would have been inside the container rather than in the
#    harness. `--rm` is deliberately absent: it would take the logs with it.
podman rm -f --ignore "$DIVERGED_CTR" >/dev/null 2>&1
podman run -d --name "$DIVERGED_CTR" --entrypoint sh "$MC_IMAGE" \
	-c "echo '${DIVERGED_LINE}'" >/dev/null 2>&1
# The line has to be on stdout before the checker reads it; a container that has
# not been scheduled yet reads as a silent sidecar.
for _ in $(seq 20); do
	# Counted, not -q: an early match here costs the `&& break`, so the wait loop runs to
	# its timeout and the gate reports "never saw replica sync" on a healthy sidecar.
	[ "$(podman logs "$DIVERGED_CTR" 2>&1 | grep -cF 'replica sync' || true)" -gt 0 ] && break
	sleep 1
done
DIV_CONF="${WORK}/conf-diverged"
mkdir -p "$DIV_CONF"
cp "${CONF_DIR}/litestream.env" "$DIV_CONF/"
cp -a "${CONF_DIR}/aws" "$DIV_CONF/" 2>/dev/null || true
sed "s|^PKDUMP_BACKUP_SIDECAR_CTR=.*|PKDUMP_BACKUP_SIDECAR_CTR=${DIVERGED_CTR}|" \
	"${CONF_DIR}/alerts.env" >"${DIV_CONF}/alerts.env"
OUT="$(PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_CONF_DIR="$DIV_CONF" \
	PKDUMP_BACKUP_CORRESPONDENCE_GRACE_SECONDS=1 \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" "$LAG_TENANT" 2>&1)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
podman rm -f --ignore "$DIVERGED_CTR" >/dev/null 2>&1
check "a replica BEHIND the local database fails the tenant leg" "1" "$RC"
check "and the STALE line names the tenant, not the registry" "1" \
	"$(printf '%s' "$OUT" | grep -c "STALE — tenant '" || true)"
check "and it names both txids so the gap is legible" "1" \
	"$(printf '%s' "$OUT" | grep -c 'S3 holds up to txid 0000000000000001, Litestream has ingested 00000000000000ff' || true)"

# 4. The sidecar is UP, has been up for longer than the grace, and has simply
#    never named this database. That is an orphan — its tenants glob is not
#    matching the file, so every write to it reaches nothing — and it was SILENT
#    (pd-30yy): the grace was computed from the tenant FILE's timestamp, so a
#    database created recently was excused however long the sidecar had been
#    watching. tests/litestream/drill.sh §6 covers the other half of the pair (a
#    sidecar that is not running at all); this is the half nothing tested.
#
#    The stand-in is a container that RUNS and says nothing, rather than the real
#    sidecar and a new tenant file: the real one globs the tenants directory and
#    would name a new file within a second, which is a race this assertion cannot
#    afford. What is under test is the ABSENCE of a position for a database that
#    is otherwise perfectly healthy — ${LAG_TENANT} is replicating, its replica
#    holds LTX files, and its file is minutes old, so nothing else here is wrong.
#    `--entrypoint sleep … infinity` is how a container is kept alive; it is not a
#    delay (tests/lib/wait_test.sh §6 excludes it by name).
SILENT_CTR="pkdump-${INSTANCE}-silent"
podman rm -f --ignore "$SILENT_CTR" >/dev/null 2>&1
podman run -d --name "$SILENT_CTR" --entrypoint sleep "$MC_IMAGE" infinity >/dev/null 2>&1
SILENT_CONF="${WORK}/conf-silent"
mkdir -p "$SILENT_CONF"
cp "${CONF_DIR}/litestream.env" "$SILENT_CONF/"
cp -a "${CONF_DIR}/aws" "$SILENT_CONF/" 2>/dev/null || true
sed "s|^PKDUMP_BACKUP_SIDECAR_CTR=.*|PKDUMP_BACKUP_SIDECAR_CTR=${SILENT_CTR}|" \
	"${CONF_DIR}/alerts.env" >"${SILENT_CONF}/alerts.env"
# The grace is the sidecar's UPTIME, so the gate waits for the stand-in to pass
# it rather than assuming it has. Four seconds against a three-second grace, asked
# of podman rather than counted here, because the container's clock is the one the
# checker reads.
silent_ctr_uptime() {
	local started
	started="$(podman inspect -f '{{.State.StartedAt.Unix}}' "$SILENT_CTR" 2>/dev/null)" || return 1
	case "$started" in ''|*[!0-9]*) return 1 ;; esac
	echo $(( $(date +%s) - started ))
}
silent_ctr_past_grace() { [ "$(silent_ctr_uptime || echo 0)" -ge 4 ]; }
wait_until 30 1 silent_ctr_past_grace || true
check "the silent stand-in sidecar is up and past the grace" "yes" \
	"$(silent_ctr_past_grace && echo yes || echo no)"
OUT="$(PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_CONF_DIR="$SILENT_CONF" \
	PKDUMP_BACKUP_SIDECAR_GRACE_SECONDS=3 \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" "$LAG_TENANT" 2>&1)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "a running sidecar that never named a tenant fails the tenant leg" "1" "$RC"
check "and it says no replication position was reported" "1" \
	"$(printf '%s' "$OUT" | grep -c 'has not reported a replication position' || true)"
check "and it names how long the sidecar has been up, not how old the file is" "1" \
	"$(printf '%s' "$OUT" | grep -c 'it has been up' || true)"

# The mutation, and the reason this is a test of the SIDECAR's uptime rather than
# of nothing: same tenant, same untouched file, same silent container — only the
# grace moves, and the verdict moves with it. A grace the stand-in cannot have
# outlived is a sidecar that has genuinely not had a chance yet, which is the one
# state that may pass.
OUT="$(PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_CONF_DIR="$SILENT_CONF" \
	PKDUMP_BACKUP_SIDECAR_GRACE_SECONDS=86400 \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" "$LAG_TENANT" 2>&1)"; RC=$?
check "a sidecar still inside its grace is not judged, on the same file" "0" "$RC"
check "and it says the grace is the sidecar's uptime" "1" \
	"$(printf '%s' "$OUT" | grep -c 'the sidecar has only been up' || true)"
podman rm -f --ignore "$SILENT_CTR" >/dev/null 2>&1

# 5. ...and the SAME pair, when it is a lag that resolves itself, must NOT page
#    (pd-yglw). The registry has re-asked a behind-reading replica over a bounded
#    window since pd-me6h — Litestream holds an un-uploaded checkpoint across a
#    transient S3 error and clears it at the next compaction tick — and the
#    tenant leg did not, so one outage produced two different verdicts about one
#    sidecar. §4b is where that surfaced: it takes the object store away, gives
#    it back and re-runs the checker, and on a loaded box (CI, 2026-08-16) the
#    registry waited its lag out on the same run that paged over the tenant's.
#
#    Driven with the same stand-in, publishing the two readings in order: behind,
#    then caught up. Nothing here is a mock of the thing under test — the tenant
#    is a real file, the replica is real S3, judge_correspondence and its txid
#    comparison run for real. Only the sidecar's ACCOUNT of where it stands is
#    substituted, which is the one thing a gate cannot provoke on demand.
#
#    The catch-up must not land until the checker has actually READ the diverged
#    pair, or the first reading is already the caught-up one and this arm proves
#    nothing. So the second line is released by CONDITION rather than after a
#    delay: the stand-in blocks until it is signalled, the harness signals it on
#    seeing the checker announce its re-ask, and neither side waits on the clock
#    (pd-86er — and a timed gap long enough to be safe on a loaded runner is
#    exactly the fixed sleep tests/lib/wait_test.sh exists to keep out).
CATCHUP_CTR="pkdump-${INSTANCE}-catchup"
CATCHUP_STAMP="time=$(date -u +%Y-%m-%dT%H:%M:%S.000Z) level=INFO msg=\"replica sync\" system=store db=${LAG_TENANT}.sqlite replica=s3"
CATCHUP_OUT="${WORK}/catchup.out"
podman rm -f --ignore "$CATCHUP_CTR" >/dev/null 2>&1
podman run -d --name "$CATCHUP_CTR" --user 0 --entrypoint sh "$MC_IMAGE" -c \
	"echo '${CATCHUP_STAMP} txid.replica=0000000000000001 txid.db=00000000000000ff'; \
	 until [ -e /tmp/caught-up ]; do sleep 1; done; \
	 echo '${CATCHUP_STAMP} txid.replica=00000000000000ff txid.db=00000000000000ff'; \
	 sleep infinity" >/dev/null 2>&1
for _ in $(seq 20); do
	[ "$(podman logs "$CATCHUP_CTR" 2>&1 | grep -cF 'replica sync' || true)" -gt 0 ] && break
	sleep 1
done
CATCHUP_CONF="${WORK}/conf-catchup"
mkdir -p "$CATCHUP_CONF"
cp "${CONF_DIR}/litestream.env" "$CATCHUP_CONF/"
cp -a "${CONF_DIR}/aws" "$CATCHUP_CONF/" 2>/dev/null || true
sed "s|^PKDUMP_BACKUP_SIDECAR_CTR=.*|PKDUMP_BACKUP_SIDECAR_CTR=${CATCHUP_CTR}|" \
	"${CONF_DIR}/alerts.env" >"${CATCHUP_CONF}/alerts.env"
BEFORE=$(sink_total)
: >"$CATCHUP_OUT"
PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	PKDUMP_CONF_DIR="$CATCHUP_CONF" \
	PKDUMP_BACKUP_CORRESPONDENCE_GRACE_SECONDS=30 \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" "$LAG_TENANT" >"$CATCHUP_OUT" 2>&1 &
CATCHUP_PID=$!
#    Also stop waiting the moment the checker has EXITED: without the re-ask it
#    fails in the first second, and this loop would otherwise sit out its whole
#    deadline before reporting a failure it already knows about.
CATCHUP_DEADLINE=$(( SECONDS + 60 ))
while [ "$SECONDS" -lt "$CATCHUP_DEADLINE" ]; do
	grep -qF 're-asking the sidecar for up to' "$CATCHUP_OUT" && break
	kill -0 "$CATCHUP_PID" 2>/dev/null || break
	sleep 1
done
podman exec "$CATCHUP_CTR" sh -c ': >/tmp/caught-up' >/dev/null 2>&1 || true
wait "$CATCHUP_PID" && RC=0 || RC=$?
OUT="$(cat "$CATCHUP_OUT")"
printf '%s\n' "$OUT" | sed 's/^/    /'
podman rm -f --ignore "$CATCHUP_CTR" >/dev/null 2>&1
check "a lag that clears itself does NOT fail the tenant leg" "0" "$RC"
check "the checker said it was re-asking rather than judging the first reading" "1" \
	"$(printf '%s' "$OUT" | grep -c "re-asking the sidecar for up to" || true)"
check "and that the replica caught up" "1" \
	"$(printf '%s' "$OUT" | grep -c "its replica caught up after" || true)"
check "and nobody was paged over it" "0" "$(since_grep "$BEFORE" "$PUSH")"

# Mutation in the other direction, because only the pair is evidence: with the
# monitor still unarmed, a replica that is genuinely stale must FAIL. An unarmed
# checker that could not fail would be the old skip wearing a new message.
podman stop -t 2 "$MINIO_CTR" >/dev/null 2>&1
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | tail -n 3 | sed 's/^/    /'
check "unarmed, a failing freshness query is STILL a failure" "1" "$RC"
check "and no ping was sent, there being nothing to ping" "0" "$(since_grep "$BEFORE" "$FAIL_PING")"
podman start "$MINIO_CTR" >/dev/null 2>&1
for _ in $(seq 60); do
	curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null 2>&1 && break
	sleep 1
done

write_alerts_env "$PING_URL"
BEFORE=$(sink_total)
run_check >/dev/null; RC=$?
check "set ping URL -> zero exit again" "0" "$RC"
check "and the heartbeat resumes" "1" "$(since_grep "$BEFORE" "$GREEN_PING")"

# alert.sh has the same defect class: asked to alert, unable to alert.
BEFORE=$(sink_total)
PUSHOVER_TOKEN= PUSHOVER_USER= PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	bash "${REPO_DIR}/deploy/alert.sh" "gate" "unconfigured" >/dev/null 2>&1
check "alert.sh with no credentials exits non-zero" "1" "$?"
PUSHOVER_TOKEN=CHANGE_ME PUSHOVER_USER=CHANGE_ME PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
	bash "${REPO_DIR}/deploy/alert.sh" "gate" "placeholder" >/dev/null 2>&1
check "alert.sh with CHANGE_ME placeholders exits non-zero" "1" "$?"
check "neither attempt sent anything" "0" "$(( $(sink_total) - BEFORE ))"

# ── 6. Layer 2: a failed unit pushes its journal tail ───────────────────────
log "6. Layer 2 fires — OnFailure pushes the failed unit's journal tail"
# The SHIPPED pkdump-alert@.service, installed under this gate's prefix and
# pointed at the throwaway alerts file. Everything else about it — the
# journalctl tail, the pipe into alert.sh — is the shipped ExecStart verbatim.
mkdir -p "$SYSTEMD_USER_DIR"
sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
	-e "s|EnvironmentFile=-%h/.config/pkdump/alerts.env|EnvironmentFile=-${TEST_ALERTS_ENV}|" \
	"${REPO_DIR}/deploy/pkdump-alert@.service" >"${SYSTEMD_USER_DIR}/${P}-alert@.service"
check "the alert unit still pipes a journal tail into alert.sh" "1" \
	"$(grep -c 'journalctl --user -u %i .* | bash .*alert\.sh' "${SYSTEMD_USER_DIR}/${P}-alert@.service")"
check "and summarises it on the way (pd-pwk8)" "1" \
	"$(grep -c 'journal-summary\.sh %i | bash' "${SYSTEMD_USER_DIR}/${P}-alert@.service")"

cat >"${SYSTEMD_USER_DIR}/${P}-boom.service" <<EOF
[Unit]
Description=Alarming gate: a unit that fails on purpose
OnFailure=${P}-alert@%n.service
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'echo "gate: simulated backup unit failure"; exit 7'
EOF
systemctl --user daemon-reload
BEFORE=$(sink_total)
systemctl --user start "${P}-boom.service" >/dev/null 2>&1
for _ in $(seq 40); do
	[ "$(since_grep "$BEFORE" "$PUSH")" -gt 0 ] && break
	sleep 0.5
done
check "the sink is still recording" "alive" "$(sink_alive)"
check "the failing unit produced a Pushover push" "1" "$(since_grep "$BEFORE" "$PUSH")"
check "the push names the unit that failed" "1" "$(since_grep "$BEFORE" "unit FAILED: ${P}-boom.service")"
check "and carries its journal tail" "1" "$(since_grep "$BEFORE" 'simulated backup unit failure')"
# pd-pwk8: the push HAPPENING is not the same as the push being READABLE. It
# fired correctly for weeks and said nothing — the body was the last 900 bytes
# of the journal, which is systemd talking about the unit.
check "the body leads with the unit and its exit status" "1" \
	"$(since_grep "$BEFORE" "${P}-boom FAILED (exit 7)")"
check "and not with systemd's boilerplate" "0" "$(since_grep "$BEFORE" 'Main process exited')"
check "nor with the OnFailure= line that fired this very alert" "0" \
	"$(since_grep "$BEFORE" 'Triggering OnFailure')"

# The case that produced the unreadable page: a unit that runs a container. The
# journal tail then carries podman's event log — a container id and every OCI
# label on the image — between the service's own output and systemd's.
# An absolute podman, resolved here: a systemd unit inherits none of this
# shell's PATH, and on a box with an alternate store that PATH is where the
# store lives (deploy/store-lib.sh writes a shim onto it). --pull=never keeps
# the gate offline — §2 has already put this image in whichever store is active.
PODMAN_BIN="$(command -v podman)"
cat >"${SYSTEMD_USER_DIR}/${P}-podboom.service" <<EOF
[Unit]
Description=Alarming gate: a podman-backed unit that fails on purpose
OnFailure=${P}-alert@%n.service
[Service]
Type=oneshot
ExecStart=/bin/sh -c '${PODMAN_BIN} run --rm --pull=never ${LITESTREAM_IMAGE} version >/dev/null 2>&1; echo "gate: podman-backed check FAILED, no replica data at s3://nowhere"; exit 1'
EOF
systemctl --user daemon-reload
BEFORE=$(sink_total)
systemctl --user start "${P}-podboom.service" >/dev/null 2>&1
for _ in $(seq 60); do
	[ "$(since_grep "$BEFORE" "$PUSH")" -gt 0 ] && break
	sleep 0.5
done
# Assert the noise was really there before asserting it is gone — otherwise a
# podman that stopped logging events would turn this into a test of nothing.
OCI_LINES="$(journalctl --user -u "${P}-podboom.service" -n 80 --no-pager 2>/dev/null |
	grep -c 'org.opencontainers' || true)"
check "the podman-backed unit's own journal really carries OCI metadata" "yes" \
	"$([ "${OCI_LINES:-0}" -gt 0 ] && echo yes || echo no)"
check "the podman-backed unit produced a Pushover push" "1" "$(since_grep "$BEFORE" "$PUSH")"
# Two greps rather than one: the sink records bodies with json.dumps, so the
# em-dash between them arrives as the escape \\u2014, not as itself.
check "whose body leads with the unit and its exit status" "1" \
	"$(since_grep "$BEFORE" "${P}-podboom FAILED (exit 1)")"
check "and with the line the service itself printed" "1" \
	"$(since_grep "$BEFORE" 'gate: podman-backed check FAILED, no replica data')"
check "and carries no OCI metadata at all" "0" "$(since_grep "$BEFORE" 'org.opencontainers')"

# ── 7. Layer 4: low disk pushes ─────────────────────────────────────────────
log "7. Layer 4 fires — low disk pushes, and only over one of its two arms"
# Both arms are pinned on every call, never left to whatever $HOME happens to
# have free today: the free-space arm added by pd-smcp fires off real numbers, so
# an unpinned "sends nothing" case would pass or fail with the box's weather.
# tests/deploy/run.sh owns the decision table hermetically; what this gate adds is
# that the decision reaches the wire — a real diskcheck, a real alert.sh, a real
# HTTP push — and that the two arms arrive as DIFFERENT pages.
disk_run() { # disk_run <threshold> <floor-gb> <warn-gb>
	sed -i "s|^PKDUMP_DISK_THRESHOLD=.*|PKDUMP_DISK_THRESHOLD=${1}|" "$TEST_ALERTS_ENV"
	PKDUMP_DISK_FLOOR_GB="$2" PKDUMP_DISK_WARN_GB="$3" \
		bash "${REPO_DIR}/deploy/diskcheck.sh" 2>&1
}
# Percent arm alone: a threshold nothing clears, a warn line every disk clears.
BEFORE=$(sink_total)
OUT="$(disk_run 0 0 1)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "diskcheck exits 0 having alerted" "0" "$RC"
check "a LOW DISK push was sent" "1" "$(since_grep "$BEFORE" 'LOW DISK')"

# Free-space arm alone: a warn line no disk clears, a threshold every disk clears.
# This is the arm that buys lead time — the percent one cannot fire before the
# gate refuses, because the gate is denominated in gigabytes (pd-smcp).
BEFORE=$(sink_total)
OUT="$(disk_run 101 1 999999999)"; RC=$?
check "the free-space arm exits 0 having alerted" "0" "$RC"
check "and pushes a page naming its warn line" "1" \
	"$(since_grep "$BEFORE" 'under 999999999G free')"

# Below the floor is a distinct page. The titles carry the CONFIGURED limit
# rather than today's percentage, so pd-hqdt's suppression collapses a disk
# sitting still into one page while this escalation still gets through.
BEFORE=$(sink_total)
disk_run 101 999999999 1999999998 >/dev/null; RC=$?
check "below the floor it exits 0 having escalated" "0" "$RC"
check "and the escalation is its own page" "1" \
	"$(since_grep "$BEFORE" 'DISK BELOW FLOOR')"

# A warn line that does not clear the floor cannot page before the gate blocks
# builds. Refused, not clamped — and the refusal reaches nobody by itself, which
# is why pkdump-diskcheck.service carries OnFailure=.
BEFORE=$(sink_total)
disk_run 101 10 10 >/dev/null; RC=$?
check "a warn line under the floor is refused" "1" "$RC"
check "and the refusal itself pushes nothing" "0" "$(( $(sink_total) - BEFORE ))"

BEFORE=$(sink_total)
disk_run 101 0 1 >/dev/null; RC=$?
check "under both arms it exits 0" "0" "$RC"
check "and sends nothing" "0" "$(( $(sink_total) - BEFORE ))"
sed -i "s|^PKDUMP_DISK_THRESHOLD=.*|PKDUMP_DISK_THRESHOLD=90|" "$TEST_ALERTS_ENV"

# ── 8. the armed/inert question has a truthful answer ───────────────────────
log "8. alarm-status.sh distinguishes ARMED from configured-but-inert"
status_run() { bash "${REPO_DIR}/deploy/alarm-status.sh" "$INSTANCE" 2>&1; }

# Nothing installed under this prefix yet except the alert unit: fully
# configured on disk (§2 wrote every config file, §3 proved the checker works)
# and still not armed, because no timer is running it.
OUT="$(status_run)"; RC=$?
check "inert instance reports NOT ARMED" "1" "$RC"
check "and says so in as many words" "1" "$(printf '%s' "$OUT" | grep -c 'NOT ARMED' || true)"
check "it names the missing timer as the reason" "yes" \
	"$(grep -q "${P}-backup-check@${INSTANCE}.timer" <<<"$OUT" && echo yes || echo no)"
# Configuration is not arming: §3 proved the checker works when run by hand, and
# the answer here is still no, because nothing is running it on a schedule.
check "and reports the checker as never having run under systemd" "1" \
	"$(printf '%s' "$OUT" | grep -c 'never run' || true)"

# Now install and enable the SHIPPED units, prefixed. Only the OnFailure target
# is renamed into this gate's namespace; every other line is the shipped file.
for EXT in service timer; do
	sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" -e "s|pkdump-alert@|${P}-alert@|g" \
		"${REPO_DIR}/deploy/pkdump-backup-check.${EXT}" >"${SYSTEMD_USER_DIR}/${P}-backup-check@.${EXT}"
	sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" -e "s|pkdump-alert@|${P}-alert@|g" \
		"${REPO_DIR}/deploy/pkdump-diskcheck.${EXT}" >"${SYSTEMD_USER_DIR}/${P}-diskcheck.${EXT}"
	# Layer 0, the uptime heartbeat. "Fully armed" has to include the layer that
	# watches whether the site serves at all, or this gate goes on certifying the
	# exact configuration that let a hard-down box page nobody on 2026-08-16.
	sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" -e "s|pkdump-alert@|${P}-alert@|g" \
		"${REPO_DIR}/deploy/pkdump-heartbeat.${EXT}" >"${SYSTEMD_USER_DIR}/${P}-heartbeat@.${EXT}"
done
sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" -e "s|pkdump-alert@|${P}-alert@|g" \
	"${REPO_DIR}/deploy/pkdump-refresh.service" >"${SYSTEMD_USER_DIR}/${P}-refresh@.service"
# The sidecar is a Quadlet unit; its OnFailure only reaches systemd through the
# generator, which is exactly the wiring §8 is checking.
mkdir -p "$QUADLET_DIR"
sed -e "s|{{INSTANCE}}|${INSTANCE}|g" -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
	-e "s|pkdump-alert@|${P}-alert@|g" \
	"${REPO_DIR}/deploy/pkdump-litestream.container" \
	>"${QUADLET_DIR}/${P}-litestream-${INSTANCE}.container"
systemctl --user daemon-reload
# Generated, never started: this gate already runs its own replicator (§2), and
# two sidecars on one set of databases is not a state worth creating.
systemctl --user stop "${P}-litestream-${INSTANCE}.service" >/dev/null 2>&1
systemctl --user enable --now "${P}-backup-check@${INSTANCE}.timer" >/dev/null 2>&1
systemctl --user enable --now "${P}-diskcheck.timer" >/dev/null 2>&1
systemctl --user enable --now "${P}-heartbeat@${INSTANCE}.timer" >/dev/null 2>&1

check "the Quadlet sidecar's OnFailure survives generation" "${P}-alert@${P}-litestream-${INSTANCE}.service.service" \
	"$(systemctl --user show "${P}-litestream-${INSTANCE}.service" -p OnFailure --value 2>/dev/null)"

# The timer alone is not enough — the checker's success under systemd is still
# unproven. Running it proves the unit's ExecStart as well as the plumbing.
BEFORE=$(sink_total)
systemctl --user start "${P}-backup-check@${INSTANCE}.service" >/dev/null 2>&1
check "the systemd unit's own run pings the monitor" "1" "$(since_grep "$BEFORE" "$GREEN_PING")"

# Layer 0 the same way: the timer existing proves nothing, running it does. A
# serving target must ping its OWN monitor and must not touch Layer 1's.
UPTIME_PING="\"path\": \"/${UPTIME_TOKEN}\""
BEFORE=$(sink_total)
systemctl --user start "${P}-heartbeat@${INSTANCE}.service" >/dev/null 2>&1
check "the heartbeat unit pings the uptime monitor when the site serves" "1" \
	"$(since_grep "$BEFORE" "$UPTIME_PING")"
check "and does NOT touch the backup monitor" "0" "$(since_grep "$BEFORE" "$GREEN_PING")"

# The load-bearing half: an unreachable site must send NOTHING. A dead box
# cannot transmit an alarm, so silence is the signal and the monitor's grace
# window is what converts it into a page. A heartbeat that pinged /fail here
# would work only in the cases that were never the problem.
# Through a fixture CONF_DIR rather than an env override, because alerts.env
# deliberately wins over the environment — the same precedence backup-check.sh
# has, and a gate that quietly relied on the opposite would be testing a script
# nobody runs.
DOWN_CONF="${WORK}/conf-down"
mkdir -p "$DOWN_CONF"
cat >"${DOWN_CONF}/alerts.env" <<EOF
PKDUMP_UPTIME_PING_URL=${UPTIME_PING_URL}
PKDUMP_HEARTBEAT_URL=http://127.0.0.1:1/
PKDUMP_HEARTBEAT_TIMEOUT=2
EOF
BEFORE=$(sink_total)
PKDUMP_CONF_DIR="$DOWN_CONF" bash "${REPO_DIR}/deploy/heartbeat.sh" "$INSTANCE" >/dev/null 2>&1
HB_DOWN_RC=$?
check "a down site makes the heartbeat exit non-zero" "1" "$HB_DOWN_RC"
check "and it sends the monitor nothing at all" "0" "$(sink_since "$BEFORE" | wc -l | tr -d ' ')"

OUT="$(status_run)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "fully armed instance reports ARMED" "0" "$RC"
check "and says so in as many words" "1" "$(printf '%s' "$OUT" | grep -c 'ALARMING: ARMED' || true)"

# ── the unit is installed, enabled, and executing nothing (pd-onyd) ─────────
# These units are HOST-WIDE — one copy of each on the box, shared by every
# instance — and until pd-onyd any checkout that ran setup.sh rewrote their
# {{REPO_DIR}}. A polecat/CI worktree is DELETED the moment its work lands, so
# prod's alerting was routinely left pointing at a directory that no longer
# existed: installed, enabled, green to every other check here, and 203/EXEC.
# "Installed" cannot be the whole question, so this asks the other half.
GONE_DIR="${WORK}/deleted-worktree"
sed -i "s|${REPO_DIR}/deploy/diskcheck.sh|${GONE_DIR}/deploy/diskcheck.sh|g" \
	"${SYSTEMD_USER_DIR}/${P}-diskcheck.service"
OUT="$(status_run)"; RC=$?
check "an ExecStart pointing at a deleted checkout flips it to NOT ARMED" "1" "$RC"
check "and it names the file that is gone" "1" \
	"$(printf '%s' "$OUT" | grep -c "${GONE_DIR}/deploy/diskcheck.sh" || true)"
check "and says the unit would fail 203/EXEC" "1" \
	"$(printf '%s' "$OUT" | grep -c '203/EXEC' || true)"
sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" -e "s|pkdump-alert@|${P}-alert@|g" \
	"${REPO_DIR}/deploy/pkdump-diskcheck.service" >"${SYSTEMD_USER_DIR}/${P}-diskcheck.service"
OUT="$(status_run)"; RC=$?
check "restoring the ExecStart restores ARMED" "0" "$RC"

# Truthfulness runs both ways: take one thing away and the answer must flip.
write_alerts_env ""
OUT="$(status_run)"; RC=$?
check "removing the monitor URL flips it back to NOT ARMED" "1" "$RC"
check "and it names the monitor as the reason" "1" \
	"$(printf '%s' "$OUT" | grep -c 'off-box monitor NOT configured' || true)"
write_alerts_env "$PING_URL"

sed -i 's|^PUSHOVER_TOKEN=.*|PUSHOVER_TOKEN=CHANGE_ME|' "$TEST_ALERTS_ENV"
OUT="$(status_run)"; RC=$?
check "a CHANGE_ME placeholder is not 'configured'" "1" "$RC"
check "and it says the pushes would be dropped" "1" \
	"$(printf '%s' "$OUT" | grep -c 'still CHANGE_ME' || true)"
sed -i 's|^PUSHOVER_TOKEN=.*|PUSHOVER_TOKEN=alarmgate-token|' "$TEST_ALERTS_ENV"

# ── 9. --verify actually delivers ───────────────────────────────────────────
# The last step of the arming runbook, and the one an operator judges the whole
# feature by. Reading configuration back is not what it claims to do, so assert
# on the two requests it is supposed to produce.
log "9. alarm-status.sh --verify fires both channels"
BEFORE=$(sink_total)
OUT="$(bash "${REPO_DIR}/deploy/alarm-status.sh" "$INSTANCE" --verify 2>&1)"; RC=$?
printf '%s\n' "$OUT" | tail -n 6 | sed 's/^/    /'
check "--verify exits 0 on an armed instance" "0" "$RC"
check "it pinged the off-box monitor for real" "1" "$(since_grep "$BEFORE" "$GREEN_PING")"
check "it sent a real push" "1" "$(since_grep "$BEFORE" "$PUSH")"
check "the push is labelled a test" "1" "$(since_grep "$BEFORE" "alarm test (${INSTANCE})")"
check "and it reports both channels delivered" "1" \
	"$(printf '%s' "$OUT" | grep -c 'both channels delivered' || true)"

# A channel that cannot deliver must fail the verify, not decorate it.
sed -i 's|^PUSHOVER_API_URL=.*|PUSHOVER_API_URL=http://127.0.0.1:1/gone|' "$TEST_ALERTS_ENV"
OUT="$(bash "${REPO_DIR}/deploy/alarm-status.sh" "$INSTANCE" --verify 2>&1)"; RC=$?
check "a dead push channel makes --verify exit non-zero" "1" "$RC"
check "and it says the push failed" "1" "$(printf '%s' "$OUT" | grep -c 'PUSH FAILED' || true)"

# ── RESULT ──────────────────────────────────────────────────────────────────
log "RESULT"
echo "  ${pass} passed, ${fail} failed"
echo "  sink recorded $(sink_total) requests in total"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — every layer was made to fire, and 'armed' is an answerable question."
