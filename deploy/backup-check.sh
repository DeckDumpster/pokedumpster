#!/usr/bin/env bash
#
# Layer 1 (PRIMARY) — backup-freshness dead-man's switch (pokedumpster-ivq.2,
# re-scoped for S3-only Litestream, multi-tenant since pd-fof4).
#
# The old design pinged a monitor from inside backup.sh on a successful nightly
# run. There is no backup.sh anymore: backups are a CONTINUOUS Litestream sidecar
# replicating every tenants/*.sqlite to S3. The canonical silent failure (observed
# during a Jun 2026 key rotation) is the sidecar showing systemd `active` while
# error-looping on AccessDenied and NOT replicating — liveness is NOT freshness.
#
# So this checker VERIFIES REPLICATION FRESHNESS against S3, then pings the
# off-box monitor (healthchecks.io) only when it is genuinely fresh:
#   1. Enumerate EVERY tenant database on the data volume — the same glob the
#      sidecar's `dir:` entry replicates. A tenant whose replica is dead is
#      exactly as unbacked-up as the only tenant used to be, and in directory
#      mode nothing else would ever notice: a tenant that never reached S3
#      produces no error anywhere, just an absent prefix.
#   2. List each tenant's replica LTX files via the litestream image (same creds
#      / secret / addressing as deploy/restore-litestream.sh).
#   3. If any list FAILS (broken creds, network, missing replica) -> NOT fresh.
#   4. If any tenant's newest replica write is older than the threshold -> NOT fresh.
#   5. All fresh -> ping the monitor (it expects a ping every run; a miss alerts).
#      Any stale -> ping <url>/fail (trip immediately) + push Pushover with detail.
#
# Because the monitor lives OFF the box, a dead checker / dead box / disabled
# timer ALSO stops the pings and trips the alert — this is the layer that would
# have caught the 11-day outage. The Pushover push is the fast, detailed signal
# while the box is up; the monitor is the backstop for box-down.
#
# Env-driven, and it FAILS when it cannot verify (pd-1717). This script used to
# print "skipping" and exit 0 when PKDUMP_BACKUP_PING_URL was unset — which is
# how alarming ends up looking armed while being off: a green systemd unit, a
# green `systemctl status`, and nothing whatsoever watching the backups. Running
# at all is the operator asserting this instance is supposed to be alarmed (the
# timer is opt-in, per instance), so an unconfigured monitor is a configuration
# FAILURE, not a quiet pass. Dev/test boxes are unaffected because they never
# enable the timer — not because the checker lies for them.
#
# No new runtime deps beyond curl + the litestream image.
#
# Usage: backup-check.sh <instance> [tenant ...]   (default: every tenant on the volume)
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: backup-check.sh <instance> [tenant ...]}"
shift || true

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
LS_IMG="docker.io/litestream/litestream:latest"
VOLUME="pkdump-${INSTANCE}-data"

# shellcheck source=deploy/litestream-lib.sh
. "${SCRIPT_DIR}/litestream-lib.sh"

# Host-wide Pushover creds, then per-instance ping URL / threshold / S3 target.
# PKDUMP_ALERTS_ENV names the host-wide file; production never sets it. Only
# tests point it elsewhere (tests/alarming/run.sh), for the same reason
# LITESTREAM_S3_ENDPOINT exists — so a gate can exercise the shipped script
# without writing to the operator's real credentials.
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"
[ -f "$ALERTS_ENV" ]                && { set -a; . "$ALERTS_ENV";                set +a; }
[ -f "${CONF_DIR}/alerts.env" ]     && { set -a; . "${CONF_DIR}/alerts.env";     set +a; }
[ -f "${CONF_DIR}/litestream.env" ] && { set -a; . "${CONF_DIR}/litestream.env"; set +a; }

PING="${PKDUMP_BACKUP_PING_URL:-}"
# Snapshots are daily (litestream.yml interval=24h); the threshold must clear one
# full interval plus margin, so a single late snapshot doesn't false-alarm.
MAX_AGE_HOURS="${PKDUMP_BACKUP_MAX_AGE_HOURS:-36}"

# Asked to verify, unable to verify -> FAIL. There is no "skip" outcome: the
# whole value of a dead-man's switch is that its silence means something, and a
# checker that exits 0 without an off-box monitor to ping is silent in exactly
# the way a working one is. Exiting non-zero also fires this unit's
# OnFailure=pkdump-alert@ (Layer 2) and shows as `failed` in systemctl, so the
# unarmed state is visible from three directions instead of none.
if [ -z "$PING" ]; then
    cat >&2 <<EOF
backup-check: FAILED — no off-box monitor configured for instance '${INSTANCE}'.
    PKDUMP_BACKUP_PING_URL is empty or unset, so nothing off this box is
    watching whether the backups are fresh. This is a configuration failure,
    not a pass.
    Fix: put the healthchecks.io ping URL in ${CONF_DIR}/alerts.env, then
         bash ${SCRIPT_DIR}/alarm-status.sh ${INSTANCE}
    (If this instance is not meant to be alarmed, disable its timer:
     systemctl --user disable --now pkdump-backup-check@${INSTANCE}.timer)
EOF
    exit 1
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

# --- Which tenants to check ------------------------------------------------
# Default: every tenant database on the volume. Explicit names are accepted so a
# restore drill can check one without waiting for the timer.
TENANTS=("$@")
if [ ${#TENANTS[@]} -eq 0 ]; then
    [ -n "$MOUNTPOINT" ] || stale "data volume '${VOLUME}' not found — cannot enumerate tenants"
    mapfile -t TENANTS < <(tenants_on_volume "$MOUNTPOINT")
fi
[ ${#TENANTS[@]} -gt 0 ] || stale "no tenant databases found under ${MOUNTPOINT}/tenants/"

NOW="$(date +%s)"
MAX_AGE_SECONDS=$(( MAX_AGE_HOURS * 3600 ))

# --- Query S3 for each tenant's replica (read-only) ------------------------
# Mirrors restore-litestream.sh's invocation: assume-role profile + bootstrap
# secret, region pinned in the derived replica URL. A read/list op — so broken
# creds surface here exactly as they would for replication.
for TENANT in "${TENANTS[@]}"; do
    REPLICA_URL="$(tenant_replica_url "$TENANT")" \
        || stale "tenant '${TENANT}': could not derive a replica URL (check litestream.env)"

    LTX_OUT="$(podman run --rm --user 0:0 \
        -v "${CONF_DIR}/aws/config:/aws/config:ro" \
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
        -e AWS_CONFIG_FILE=/aws/config \
        -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
        -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
        "$LS_IMG" ltx -level all "$REPLICA_URL" 2>&1)" \
        || stale "tenant '${TENANT}': litestream ltx failed (creds/network/S3): $(printf '%s' "$LTX_OUT" | tail -n1)"

    # Newest RFC3339 'created' timestamp, parsed format-agnostically (the column
    # order has shifted across litestream versions). Zulu RFC3339 sorts
    # lexicographically == chronologically.
    #
    # `-level all` is load-bearing: `ltx` defaults to level 0, and level 0 gets
    # compacted away into higher levels. A tenant nobody has written to today
    # would list nothing at level 0 and read as dead when it is merely idle.
    # `|| true` because "no timestamps at all" is a case this handles below, not
    # a reason to abort: grep exits 1 on no match and pipefail would kill the
    # script before it could report — silently, with the dead-man's switch
    # neither pinged nor tripped.
    NEWEST="$(printf '%s\n' "$LTX_OUT" \
        | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z' \
        | sort | tail -n1 || true)"

    if [ -z "$NEWEST" ]; then
        # Nothing has ever been replicated for this tenant. That is the alarm
        # condition — unless the database itself is younger than the threshold,
        # in which case it was only just provisioned and has not had a full
        # window to reach S3 yet.
        DB_FILE="${MOUNTPOINT}/tenants/${TENANT}.sqlite"
        DB_AGE=0
        [ -e "$DB_FILE" ] && DB_AGE=$(( NOW - $(stat -c %Y "$DB_FILE") ))
        if [ "$DB_AGE" -gt "$MAX_AGE_SECONDS" ]; then
            stale "tenant '${TENANT}': no replica data at ${REPLICA_URL%%\?*} — it is NOT backed up"
        fi
        echo "backup-check: tenant '${TENANT}' has no replica yet but was provisioned $(( DB_AGE / 60 ))m ago — not judged"
        continue
    fi

    NEWEST_EPOCH="$(date -d "$NEWEST" +%s 2>/dev/null)" \
        || stale "tenant '${TENANT}': could not parse replica timestamp: ${NEWEST}"
    AGE_H=$(( ( NOW - NEWEST_EPOCH ) / 3600 ))

    if [ "$AGE_H" -gt "$MAX_AGE_HOURS" ]; then
        stale "tenant '${TENANT}': newest S3 replica write is ${AGE_H}h old (> ${MAX_AGE_HOURS}h threshold)"
    fi
    echo "backup-check: tenant '${TENANT}' OK — newest S3 replica write ${AGE_H}h old (<= ${MAX_AGE_HOURS}h)"
done

# --- Fresh: ping the monitor + record the marker ---------------------------
echo "backup-check: OK — ${#TENANTS[@]} tenant(s) fresh; pinging monitor"
mark_fresh
curl -fsS -m 10 "$PING" >/dev/null 2>&1 || echo "backup-check: WARNING — monitor ping failed (will retry next run)" >&2
