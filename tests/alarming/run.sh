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

MINIO_CTR="pkdump-${INSTANCE}-minio"
LS_CTR="pkdump-${INSTANCE}-litestream"

# Ports are taken from the kernel, not picked. Fixed numbers fail two ways here:
# several polecats run this gate concurrently from their own worktrees, and this
# box already had an unrelated `python3 -m http.server` sitting on the first
# number chosen — which surfaced as "address already in use" three sections
# later, looking nothing like a port clash.
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()'; }
MINIO_PORT=${MINIO_PORT:-$(free_port)}
SINK_PORT=${SINK_PORT:-$(free_port)}

WORK=${WORK:-$(mktemp -d /tmp/pkdump-alarm.XXXXXX)}
SINK_LOG="${WORK}/sink.jsonl"
SINK_PID=""

# The ping path carries a random token so an assertion cannot match a stray
# request from anything else that happens to be talking to this port.
HC_TOKEN="hc-$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
PING_URL="http://127.0.0.1:${SINK_PORT}/${HC_TOKEN}"
PUSH_PATH="/pushover/messages.json"

# The host-wide alerts file, redirected. Ryan's real one is never read or
# written by this gate.
TEST_ALERTS_ENV="${WORK}/alerts-host.env"
export PKDUMP_ALERTS_ENV="$TEST_ALERTS_ENV"

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
		echo "  rm -f ${SYSTEMD_USER_DIR}/${P}-* ${QUADLET_DIR}/${P}-litestream-${INSTANCE}.container"
		echo "  podman volume rm -f ${VOLUME}; podman secret rm ${SECRET_NAME}; rm -rf ${CONF_DIR} ${WORK}"
		return $rc
	fi
	[ -n "$SINK_PID" ] && kill "$SINK_PID" >/dev/null 2>&1
	# Disable before removing the files, or systemd keeps the symlinks around.
	systemctl --user disable --now "${P}-backup-check@${INSTANCE}.timer" >/dev/null 2>&1
	systemctl --user disable --now "${P}-diskcheck.timer" >/dev/null 2>&1
	systemctl --user stop "${P}-litestream-${INSTANCE}.service" >/dev/null 2>&1
	rm -f "${SYSTEMD_USER_DIR}/${P}-"* "${QUADLET_DIR}/${P}-litestream-${INSTANCE}.container"
	systemctl --user daemon-reload >/dev/null 2>&1
	systemctl --user reset-failed "${P}-"'*' >/dev/null 2>&1
	podman rm -f --ignore "$LS_CTR" "$MINIO_CTR" >/dev/null 2>&1
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
run_check() { PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}${PUSH_PATH}" \
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
check "the bucket exists" "1" "$(mc ls s/ 2>/dev/null | grep -c "$LITESTREAM_S3_BUCKET" || true)"

podman secret rm "$SECRET_NAME" >/dev/null 2>&1
mkdir -p "${CONF_DIR}/aws"
chmod 700 "$CONF_DIR" "${CONF_DIR}/aws"
# MinIO cannot assume a role, so the profile carries the static test key. The
# assume-role path is covered by tests/litestream/drill.sh (DRILL_REAL_S3=1).
printf '[profile pkdump]\nregion=%s\n' "$LITESTREAM_S3_REGION" >"${CONF_DIR}/aws/config"
printf '[pkdump]\naws_access_key_id=%s\naws_secret_access_key=%s\n' "$AKID" "$SECRET_KEY" \
	| podman secret create "$SECRET_NAME" - >/dev/null

write_litestream_env() { # write_litestream_env [s3-path-override]
	cat >"${CONF_DIR}/litestream.env" <<EOF
LITESTREAM_S3_BUCKET=${LITESTREAM_S3_BUCKET}
LITESTREAM_S3_REGION=${LITESTREAM_S3_REGION}
LITESTREAM_S3_PATH=${1:-$LITESTREAM_S3_PATH}
LITESTREAM_TENANTS_DIR=/data/tenants
LITESTREAM_S3_ENDPOINT=${LITESTREAM_S3_ENDPOINT}
AWS_PROFILE=pkdump
EOF
	chmod 600 "${CONF_DIR}/litestream.env"
}
write_alerts_env() { # write_alerts_env <ping-url> [max-age-hours]
	cat >"${CONF_DIR}/alerts.env" <<EOF
PKDUMP_BACKUP_PING_URL=${1}
PKDUMP_BACKUP_MAX_AGE_HOURS=${2:-36}
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
EOF

podman volume rm -f "$VOLUME" >/dev/null 2>&1
podman volume create "$VOLUME" >/dev/null
MP="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"
mkdir -p "${MP}/tenants"
for t in "${TENANTS[@]}"; do
	sqlite3 -cmd '.timeout 5000' "${MP}/tenants/${t}.sqlite" \
		"PRAGMA journal_mode=WAL;
		 CREATE TABLE collection (id INTEGER PRIMARY KEY, tenant TEXT);
		 INSERT INTO collection (tenant) VALUES ('${t}');" >/dev/null
done
check "the data volume carries ${#TENANTS[@]} tenants" "${TENANTS[*]}" \
	"$(tenants_on_volume "$MP" | sort | tr '\n' ' ' | sed 's/ $//')"

# The SHIPPED sidecar config, replicating for real, so the freshness query the
# checker runs is a real S3 query against real replica data.
podman run -d --name "$LS_CTR" --user 0 \
	-v "${MP}:/data:Z" -v "${REPO_DIR}/deploy/litestream.yml:/etc/litestream.yml:ro,Z" \
	-v "${CONF_DIR}/aws/config:/aws/config:ro" \
	--secret "${SECRET_NAME},type=mount,target=/aws/credentials" \
	-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
	-e AWS_PROFILE -e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET \
	-e LITESTREAM_S3_PATH -e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null

replicating=0
for t in "${TENANTS[@]}"; do
	url="$(tenant_replica_url "$t")"
	deadline=$(( SECONDS + 90 ))
	while [ "$SECONDS" -lt "$deadline" ]; do
		if podman run --rm --user 0:0 \
			-v "${CONF_DIR}/aws/config:/aws/config:ro" \
			--secret "${SECRET_NAME},type=mount,target=/aws/credentials" \
			-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
			-e AWS_PROFILE=pkdump "$LITESTREAM_IMAGE" ltx -level all "$url" 2>/dev/null | grep -q .
		then replicating=$((replicating + 1)); break; fi
		sleep 3
	done
done
check "both tenants are replicating to S3" "2" "$replicating"

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

# ── 5. THE SKIP (pd-1717): unable to verify is a FAILURE, not a pass ────────
log "5. the skip is gone — no monitor configured is a FAILURE (pd-1717)"
# This is the bug that let alarming look armed while being off: with no ping URL
# the checker printed 'skipping', exited 0, and every dashboard read green.
# Mutation in both directions, because only the pair is evidence.
write_alerts_env ""
BEFORE=$(sink_total)
OUT="$(run_check)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "unset ping URL -> non-zero exit" "1" "$RC"
check "it does not use the word 'skipping'" "0" "$(printf '%s' "$OUT" | grep -ci 'skipping' || true)"
check "it says FAILED" "1" "$(printf '%s' "$OUT" | grep -c 'FAILED' || true)"
check "and nothing at all was sent" "0" "$(( $(sink_total) - BEFORE ))"

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
	"$(grep -c 'journalctl --user -u %i -n 20 --no-pager | bash' "${SYSTEMD_USER_DIR}/${P}-alert@.service")"

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

# ── 7. Layer 4: low disk pushes ─────────────────────────────────────────────
log "7. Layer 4 fires — low disk pushes, and only over the threshold"
disk_run() { # disk_run <threshold>
	sed -i "s|^PKDUMP_DISK_THRESHOLD=.*|PKDUMP_DISK_THRESHOLD=${1}|" "$TEST_ALERTS_ENV"
	bash "${REPO_DIR}/deploy/diskcheck.sh" 2>&1
}
BEFORE=$(sink_total)
OUT="$(disk_run 0)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "diskcheck exits 0 having alerted" "0" "$RC"
check "a LOW DISK push was sent" "1" "$(since_grep "$BEFORE" 'LOW DISK')"

BEFORE=$(sink_total)
disk_run 101 >/dev/null; RC=$?
check "under the threshold it exits 0" "0" "$RC"
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
	"$(printf '%s' "$OUT" | grep -q "${P}-backup-check@${INSTANCE}.timer" && echo yes || echo no)"
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

check "the Quadlet sidecar's OnFailure survives generation" "${P}-alert@${P}-litestream-${INSTANCE}.service.service" \
	"$(systemctl --user show "${P}-litestream-${INSTANCE}.service" -p OnFailure --value 2>/dev/null)"

# The timer alone is not enough — the checker's success under systemd is still
# unproven. Running it proves the unit's ExecStart as well as the plumbing.
BEFORE=$(sink_total)
systemctl --user start "${P}-backup-check@${INSTANCE}.service" >/dev/null 2>&1
check "the systemd unit's own run pings the monitor" "1" "$(since_grep "$BEFORE" "$GREEN_PING")"

OUT="$(status_run)"; RC=$?
printf '%s\n' "$OUT" | sed 's/^/    /'
check "fully armed instance reports ARMED" "0" "$RC"
check "and says so in as many words" "1" "$(printf '%s' "$OUT" | grep -c 'ALARMING: ARMED' || true)"

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
