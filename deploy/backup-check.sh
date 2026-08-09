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
#   0. Check the USER REGISTRY's replica (pd-nd6w). It is checked separately
#      because it IS separate: its own file at the data root, its own `path:`
#      replica prefix. And it is checked at all because its silent loss is the
#      one this script's tenant loop cannot see — every tenant would still be
#      fresh, and the table saying whose database is whose would be gone.
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
# ── NO CHECK MAY PASS BY SKIPPING (pd-1717, then pd-7f46) ───────────────────
# This script used to print "skipping" and exit 0 without asking S3 anything
# when PKDUMP_BACKUP_PING_URL was unset. That is a pass — indistinguishable, to
# a caller, a CI tier or an operator reading a green unit, from "every replica
# is fresh". A check that cannot fail is not evidence, and this project already
# owns the scar: prod ran ACTIVE and replicating nothing while every
# backup-shaped signal was green (pd-1717).
#
# So the VERIFICATION always runs. What the ping URL controls is the PING, and
# nothing else: with no monitor configured, freshness is still checked against
# S3 and a stale replica still fails, it just cannot arm the off-box dead-man.
# The absence of that URL is reported on the way past, because an unarmed Layer
# 1 is worth saying out loud on a box that has real backups.
#
# Note that "is this instance armed?" is a different question from "are the
# backups fresh?", and it has its own truthful answer in deploy/alarm-status.sh
# — which reports NOT ARMED and exits non-zero for exactly this configuration.
# Answering it here as well, by failing, would cost the freshness verification
# its own exit status.
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

# No monitor configured. The check still runs — see the header. Only the ping at
# the end is skipped, and the operator is told which half they are getting, and
# how to arm the other one.
if [ -z "$PING" ]; then
    echo "backup-check: PKDUMP_BACKUP_PING_URL unset — verifying freshness anyway;" \
         "the off-box dead-man's switch is NOT armed (instance: ${INSTANCE})." \
         "To arm it: put the healthchecks.io ping URL in ${CONF_DIR}/alerts.env," \
         "then bash ${SCRIPT_DIR}/alarm-status.sh ${INSTANCE}"
fi

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

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
    # window to expire on a missed ping. Only if there is one to trip: an
    # unarmed monitor changes what this failure can NOTIFY, never whether it
    # is a failure.
    if [ -n "$PING" ]; then
        curl -fsS -m 10 "${PING}/fail" >/dev/null 2>&1 || true
    fi
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

# ltx_newest <replica-url> <what> — the newest RFC3339 'created' timestamp in a
# replica, or empty if it has none. Returns NON-ZERO if the query itself failed,
# because "we could not ask" and "the answer is fine" must never be the same
# outcome — and every caller substitutes this, so it cannot trip the switch
# itself: `exit` inside `$(...)` leaves only the subshell, and the check would
# sail on with an empty answer. The caller does the tripping.
#
# Parsed format-agnostically: the column order has shifted across litestream
# versions. Zulu RFC3339 sorts lexicographically == chronologically.
#
# `-level all` is load-bearing: `ltx` defaults to level 0, and level 0 gets
# compacted away into higher levels. A database nobody has written to today
# would list nothing at level 0 and read as dead when it is merely idle.
# `|| true` because "no timestamps at all" is a case the callers handle, not a
# reason to abort: grep exits 1 on no match and pipefail would kill the script
# before it could report — silently, with the dead-man's switch neither pinged
# nor tripped.
ltx_newest() {
    local url="$1" what="$2" out
    out="$(podman run --rm --user 0:0 \
        -v "${CONF_DIR}/aws/config:/aws/config:ro" \
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
        -e AWS_CONFIG_FILE=/aws/config \
        -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
        -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
        "$LS_IMG" ltx -level all "$url" 2>&1)" || {
        echo "backup-check: ${what}: litestream ltx failed (creds/network/S3): $(printf '%s' "$out" | tail -n1)" >&2
        return 1
    }

    printf '%s\n' "$out" \
        | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z' \
        | sort | tail -n1 || true
}

# judge_freshness <what> <newest> <db-file> <replica-url> — shared by the
# registry and every tenant. Either it returns quietly or it trips the switch.
judge_freshness() {
    local what="$1" newest="$2" db_file="$3" url="$4" age_h db_age=0 newest_epoch

    if [ -z "$newest" ]; then
        # Nothing has ever been replicated. That is the alarm condition —
        # unless the database itself is younger than the threshold, in which
        # case it was only just created and has not had a full window to reach
        # S3 yet.
        [ -e "$db_file" ] && db_age=$(( NOW - $(stat -c %Y "$db_file") ))
        if [ "$db_age" -gt "$MAX_AGE_SECONDS" ]; then
            stale "${what}: no replica data at ${url%%\?*} — it is NOT backed up"
        fi
        echo "backup-check: ${what} has no replica yet but was created $(( db_age / 60 ))m ago — not judged"
        return 0
    fi

    newest_epoch="$(date -d "$newest" +%s 2>/dev/null)" \
        || stale "${what}: could not parse replica timestamp: ${newest}"
    age_h=$(( ( NOW - newest_epoch ) / 3600 ))

    if [ "$age_h" -gt "$MAX_AGE_HOURS" ]; then
        stale "${what}: newest S3 replica write is ${age_h}h old (> ${MAX_AGE_HOURS}h threshold)"
    fi
    echo "backup-check: ${what} OK — newest S3 replica write ${age_h}h old (<= ${MAX_AGE_HOURS}h)"
}

# --- The user registry (pd-nd6w) -------------------------------------------
# Checked FIRST, and checked at all, because a registry that quietly stopped
# replicating is the one failure the tenant loop below cannot see: every tenant
# would still be fresh, and the thing that says whose database is whose would be
# gone. That is the DR gap this project rejected libSQL/sqld over.
#
# It is NOT checked as a tenant. The registry lives at the data root with its own
# `path:` replica prefix, so it needs its own URL — and passing "registry" to
# tenant_replica_url would silently address a tenant prefix that does not exist.
#
# A box that has never had a registry file (nothing writes one until the resolver
# lands) is not a failure: absent file AND absent replica is the pre-registry
# state, and it is judged only once a registry exists to be backed up.
REGISTRY_FILE="${MOUNTPOINT}/registry.sqlite"
REGISTRY_URL="$(registry_replica_url)" \
    || stale "could not derive the registry replica URL — run deploy/setup.sh ${INSTANCE} to backfill litestream.env"
REGISTRY_NEWEST="$(ltx_newest "$REGISTRY_URL" "the user registry")" \
    || stale "the user registry: could not read its replica at ${REGISTRY_URL%%\?*}"
if [ -z "$REGISTRY_NEWEST" ] && [ ! -e "$REGISTRY_FILE" ]; then
    echo "backup-check: no user registry on this instance yet — nothing to back up"
else
    judge_freshness "the user registry" "$REGISTRY_NEWEST" "$REGISTRY_FILE" "$REGISTRY_URL"
fi

# --- Query S3 for each tenant's replica (read-only) ------------------------
# Mirrors restore-litestream.sh's invocation: assume-role profile + bootstrap
# secret, region pinned in the derived replica URL. A read/list op — so broken
# creds surface here exactly as they would for replication.
for TENANT in "${TENANTS[@]}"; do
    REPLICA_URL="$(tenant_replica_url "$TENANT")" \
        || stale "tenant '${TENANT}': could not derive a replica URL (check litestream.env)"
    NEWEST="$(ltx_newest "$REPLICA_URL" "tenant '${TENANT}'")" \
        || stale "tenant '${TENANT}': could not read its replica at ${REPLICA_URL%%\?*}"
    judge_freshness "tenant '${TENANT}'" "$NEWEST" \
        "${MOUNTPOINT}/tenants/${TENANT}.sqlite" "$REPLICA_URL"
done

# --- Fresh: ping the monitor + record the marker ---------------------------
mark_fresh
if [ -z "$PING" ]; then
    echo "backup-check: OK — the registry + ${#TENANTS[@]} tenant(s) fresh; no monitor to ping" \
         "(PKDUMP_BACKUP_PING_URL unset — Layer 1 cannot alert on a dead box)"
else
    echo "backup-check: OK — the registry + ${#TENANTS[@]} tenant(s) fresh; pinging monitor"
    curl -fsS -m 10 "$PING" >/dev/null 2>&1 || echo "backup-check: WARNING — monitor ping failed (will retry next run)" >&2
fi
