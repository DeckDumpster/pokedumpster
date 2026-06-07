#!/usr/bin/env bash
#
# Layer 1 (PRIMARY) — backup-freshness dead-man's switch (pokedumpster-ivq.2,
# re-scoped for S3-only Litestream).
#
# The old design pinged a monitor from inside backup.sh on a successful nightly
# run. There is no backup.sh anymore: backups are a CONTINUOUS Litestream sidecar
# replicating collection.sqlite to S3. The canonical silent failure (observed
# during a Jun 2026 key rotation) is the sidecar showing systemd `active` while
# error-looping on AccessDenied and NOT replicating — liveness is NOT freshness.
#
# So this checker VERIFIES REPLICATION FRESHNESS against S3, then pings the
# off-box monitor (healthchecks.io) only when it is genuinely fresh:
#   1. List the S3 replica's snapshots via the litestream image (same creds /
#      secret / config as deploy/restore-litestream.sh).
#   2. If the list FAILS (broken creds, network, missing replica) -> NOT fresh.
#   3. If the newest snapshot is older than the threshold -> NOT fresh.
#   4. Fresh  -> ping the monitor (it expects a ping every run; a miss alerts).
#      Stale  -> ping <url>/fail (trip immediately) + push Pushover with detail.
#
# Because the monitor lives OFF the box, a dead checker / dead box / disabled
# timer ALSO stops the pings and trips the alert — this is the layer that would
# have caught the 11-day outage. The Pushover push is the fast, detailed signal
# while the box is up; the monitor is the backstop for box-down.
#
# Env-driven; PKDUMP_BACKUP_PING_URL unset = no-op (dev/test/unconfigured boxes
# are unaffected). No new runtime deps beyond curl + the litestream image.
#
# Usage: backup-check.sh <instance> [user-db-name]   (default user-db: collection)
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: backup-check.sh <instance> [user-db-name]}"
USER_DB="${2:-collection}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
LS_YML="${REPO_DIR}/deploy/litestream.yml"
LS_IMG="docker.io/litestream/litestream:latest"
VOLUME="pkdump-${INSTANCE}-data"

# Host-wide Pushover creds, then per-instance ping URL / threshold / S3 target.
[ -f "${HOME}/.config/pkdump/alerts.env" ] && { set -a; . "${HOME}/.config/pkdump/alerts.env"; set +a; }
[ -f "${CONF_DIR}/alerts.env" ]            && { set -a; . "${CONF_DIR}/alerts.env";            set +a; }
[ -f "${CONF_DIR}/litestream.env" ]        && { set -a; . "${CONF_DIR}/litestream.env";        set +a; }

PING="${PKDUMP_BACKUP_PING_URL:-}"
# Snapshots are daily (litestream.yml interval=24h); the threshold must clear one
# full interval plus margin, so a single late snapshot doesn't false-alarm.
MAX_AGE_HOURS="${PKDUMP_BACKUP_MAX_AGE_HOURS:-36}"

# No monitor configured -> nothing to do. Keep dev/test silent.
if [ -z "$PING" ]; then
    echo "backup-check: PKDUMP_BACKUP_PING_URL unset — skipping (instance: ${INSTANCE})"
    exit 0
fi

# Mark the latest confirmed-fresh time on the data volume so the app can surface
# staleness in-app (Layer 3 / ivq.5) without needing S3 creds of its own.
MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME" 2>/dev/null || true)"
mark_fresh() {
    [ -n "$MOUNTPOINT" ] || return 0
    date +%s > "${MOUNTPOINT}/.backup-last-ok" 2>/dev/null || true
}

stale() {
    local reason="$1"
    echo "backup-check: STALE — ${reason}" >&2
    # Trip the off-box dead-man immediately rather than waiting for the grace
    # window to expire on a missed ping.
    curl -fsS -m 10 "${PING}/fail" >/dev/null 2>&1 || true
    # Fast, detailed push (only reaches you while the box is up; the monitor is
    # the backstop for box-down).
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster backup STALE (${INSTANCE})" \
        "Litestream S3 freshness check failed: ${reason}" || true
    exit 1
}

# --- Query S3 for the replica's snapshots (read-only) ----------------------
# Mirrors restore-litestream.sh's invocation: assume-role profile + bootstrap
# secret, region pinned via litestream.yml. A read/list op — so broken creds
# surface here exactly as they would for replication.
SNAP_OUT="$(podman run --rm --user 0:0 \
    -v "${VOLUME}:/data:ro" \
    -v "${LS_YML}:/etc/litestream.yml:ro" \
    -v "${CONF_DIR}/aws/config:/aws/config:ro" \
    --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
    -e AWS_CONFIG_FILE=/aws/config \
    -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
    -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
    -e LITESTREAM_S3_BUCKET="${LITESTREAM_S3_BUCKET:-}" \
    -e LITESTREAM_S3_REGION="${LITESTREAM_S3_REGION:-}" \
    -e LITESTREAM_S3_PATH="${LITESTREAM_S3_PATH:-}" \
    -e LITESTREAM_DB_PATH="/data/${USER_DB}.sqlite" \
    "$LS_IMG" snapshots -config /etc/litestream.yml "/data/${USER_DB}.sqlite" 2>&1)" \
    || stale "litestream snapshots failed (creds/network/S3): $(printf '%s' "$SNAP_OUT" | tail -n1)"

# Newest RFC3339 'created' timestamp, parsed format-agnostically (the column
# order has shifted across litestream versions). Zulu RFC3339 sorts
# lexicographically == chronologically.
NEWEST="$(printf '%s\n' "$SNAP_OUT" \
    | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z' \
    | sort | tail -n1)"
[ -n "$NEWEST" ] || stale "no snapshots found in S3 replica (output: $(printf '%s' "$SNAP_OUT" | tail -n1))"

NEWEST_EPOCH="$(date -d "$NEWEST" +%s 2>/dev/null)" || stale "could not parse snapshot timestamp: ${NEWEST}"
AGE_H=$(( ( $(date +%s) - NEWEST_EPOCH ) / 3600 ))

if [ "$AGE_H" -gt "$MAX_AGE_HOURS" ]; then
    stale "newest S3 snapshot is ${AGE_H}h old (> ${MAX_AGE_HOURS}h threshold)"
fi

# --- Fresh: ping the monitor + record the marker ---------------------------
echo "backup-check: OK — newest S3 snapshot ${AGE_H}h old (<= ${MAX_AGE_HOURS}h); pinging monitor"
mark_fresh
curl -fsS -m 10 "$PING" >/dev/null 2>&1 || echo "backup-check: WARNING — monitor ping failed (will retry next run)" >&2
