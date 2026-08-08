#!/usr/bin/env bash
#
# Disaster recovery: restore ONE tenant's collection database from its Litestream
# S3 replica (pokedumpster-8ch.5, multi-tenant since pd-fof4).
#
# ── RUNBOOK ─────────────────────────────────────────────────────────────────
# WHEN TO USE
#   A tenant's collection DB on the data volume is lost or corrupt and you want
#   to recover it from the off-box S3 replica written by the Litestream sidecar
#   (deploy/pkdump-litestream.container). This is the only backup now; the local
#   snapshot scripts were removed (S3-only). See deploy/RESTORE.md.
#
# WHAT IT DOES
#   1. Stops the app service + the Litestream sidecar (so nothing writes/replicates).
#   2. Runs `litestream restore` from the tenant's S3 replica onto the data
#      volume (temp-then-rename).
#   3. Restarts the app + sidecar.
#   4. Verifies the restored row count.
#   5. Verifies both services actually came back — a restored collection that is
#      no longer replicating is a half-finished recovery, and it fails silently.
#
#   Only the named tenant is touched. Every other tenant keeps replicating from
#   its own prefix and its files are not read or written here.
#
#   The shared catalog (shared.sqlite) is NOT restored — it is reproducible:
#   rebuild it with `deploy/seed.sh <instance>`.
#
# ADDRESSING
#   The sidecar runs in Litestream's directory mode, so deploy/litestream.yml
#   names no database and `restore -config` cannot resolve one. The tenant's
#   replica is addressed by URL, derived by deploy/litestream-lib.sh from the
#   same LITESTREAM_S3_PATH the sidecar replicates under. The derivation is
#   asserted against a real bucket in tests/litestream/run.sh.
#
# CREDENTIALS
#   Assume-role profile in ~/.config/pkdump/<instance>/aws/config + the bootstrap
#   key from podman secret pkdump-<instance>-s3-bootstrap (same as the sidecar) —
#   auto-refreshing temporary credentials via role assumption, never static keys.
#
# USAGE
#   bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] <instance> [tenant]
#     --yes, -y      skip the confirmation prompt (scripted use)
#     --at=<time>    point-in-time restore, e.g. --at=2026-06-01T12:00:00Z
#                    (default: latest; within the 6-month retention window)
#   Examples:
#     bash deploy/restore-litestream.sh prod
#     bash deploy/restore-litestream.sh prod alice
#     bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod alice
# ────────────────────────────────────────────────────────────────────────────
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

YES=false
AT=""
POSITIONAL=()
for arg in "$@"; do
    case $arg in
        --yes|-y) YES=true ;;
        --at=*) AT="${arg#--at=}" ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] <instance> [tenant]"
    exit 1
fi

INSTANCE="${POSITIONAL[0]}"
TENANT="${POSITIONAL[1]:-collection}"
SERVICE_NAME="pkdump-${INSTANCE}"
SIDECAR="pkdump-litestream-${INSTANCE}"
VOLUME="pkdump-${INSTANCE}-data"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LS_IMG="docker.io/litestream/litestream:latest"

# shellcheck source=deploy/litestream-lib.sh
. "${SCRIPT_DIR}/litestream-lib.sh"

echo "==> PokeDumpster Litestream restore"
echo "    Instance: ${INSTANCE}   Tenant: ${TENANT}   Volume: ${VOLUME}"
[ -n "$AT" ] && echo "    Point-in-time: ${AT}"

# Restore into the store the instance actually lives in (pd-fite). Prod's unit
# carries no store flags, so this is a no-op for prod.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Validate --------------------------------------------------------------
command -v sqlite3 >/dev/null 2>&1 || { echo "ERROR: sqlite3 not found on host. Install: sudo apt install sqlite3"; exit 1; }
[ -f "${CONF_DIR}/litestream.env" ] || { echo "ERROR: ${CONF_DIR}/litestream.env not found — is the sidecar configured?"; exit 1; }
[ -f "${CONF_DIR}/aws/config" ] || { echo "ERROR: ${CONF_DIR}/aws/config not found — is the sidecar configured?"; exit 1; }
podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1 || { echo "ERROR: podman secret 'pkdump-${INSTANCE}-s3-bootstrap' not found."; exit 1; }
podman volume exists "$VOLUME" 2>/dev/null || { echo "ERROR: data volume '${VOLUME}' not found."; exit 1; }

# S3 target comes from the instance's env file (bucket / path / region / AWS_PROFILE).
set -a; . "${CONF_DIR}/litestream.env"; set +a

# Rejects a malformed tenant name before it can retarget the S3 prefix, and
# derives the one replica URL this restore will read.
REPLICA_URL="$(tenant_replica_url "$TENANT")"

# --- Confirm ---------------------------------------------------------------
if [ "$YES" = false ]; then
    echo ""
    echo "WARNING: overwrites tenants/${TENANT}.sqlite on '${VOLUME}' from"
    echo "         ${REPLICA_URL%%\?*}"
    echo "         (no other tenant is read or written)"
    read -r -p "Continue? [y/N] " response
    [[ "$response" =~ ^[Yy]$ ]] || { echo "Restore cancelled."; exit 0; }
fi

# --- Stop writers ----------------------------------------------------------
echo "==> Stopping ${SERVICE_NAME} + ${SIDECAR}..."
systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true
systemctl --user stop "${SIDECAR}.service" 2>/dev/null || true
sleep 2

MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"
# Tenant collection DBs live under tenants/ (deploy/TENANTS.md). Restoring onto
# a bare volume — the disaster case — reaches a data dir that has no tenants/
# yet, and litestream will not create the parent of its -o target.
mkdir -p "${MOUNTPOINT}/tenants"
DEST="${MOUNTPOINT}/tenants/${TENANT}.sqlite"
TMP="${MOUNTPOINT}/tenants/${TENANT}.sqlite.restore-tmp"

# --- Restore from S3 -------------------------------------------------------
# litestream restore refuses to overwrite an existing file, so restore to a temp
# path on the volume, then atomically rename into place.
echo "==> Restoring tenants/${TENANT}.sqlite from S3 via litestream..."
rm -f "$TMP"
RESTORE_FLAGS=(restore -integrity-check full)
[ -n "$AT" ] && RESTORE_FLAGS+=(-timestamp "$AT")     # point-in-time restore
RESTORE_FLAGS+=(-o "/data/tenants/${TENANT}.sqlite.restore-tmp" "$REPLICA_URL")
podman run --rm --user 0:0 \
    -v "${VOLUME}:/data" \
    -v "${CONF_DIR}/aws/config:/aws/config:ro" \
    --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
    -e AWS_CONFIG_FILE=/aws/config \
    -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
    -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
    "$LS_IMG" "${RESTORE_FLAGS[@]}"

# --- Atomic swap -----------------------------------------------------------
rm -f "${DEST}-wal" "${DEST}-shm"
mv -f "$TMP" "$DEST"

# --- Restart ---------------------------------------------------------------
# reset-failed before start: the sidecar unit is rate-limited (StartLimitBurst=5
# per 300s) so a crash-loop pages instead of thrashing. Restoring several tenants
# in a row — exactly what a real incident looks like — trips that limit, and then
# `start` fails. Observed in the pd-v8zf DR drill.
echo "==> Starting ${SERVICE_NAME} (+ sidecar)..."
systemctl --user reset-failed "$SERVICE_NAME" "${SIDECAR}.service" 2>/dev/null || true
systemctl --user start "$SERVICE_NAME" 2>/dev/null || true
systemctl --user start "${SIDECAR}.service" 2>/dev/null || true

# --- Verify ----------------------------------------------------------------
ROWS="$(sqlite3 "file:${DEST}?mode=ro" 'SELECT count(*) FROM collection;' 2>&1 || echo '?')"
echo "==> Restore complete — tenants/${TENANT}.sqlite has ${ROWS} collection rows."
echo "    Shared catalog not restored (reproducible): bash deploy/seed.sh ${INSTANCE}"

# A restored collection that is no longer being backed up is a half-finished
# recovery, and the failure is silent: replication stopping produces no error
# anywhere and Layer 1 would not notice for 36 hours. So the services this script
# stopped are checked, not merely started.
DOWN=()
for UNIT in "$SERVICE_NAME" "${SIDECAR}.service"; do
    # Units that are not installed on this box (a bare instance with no app, a
    # box with backups deliberately unconfigured) are not "down".
    systemctl --user cat "$UNIT" >/dev/null 2>&1 || continue
    # A few seconds of grace: the app container pulls itself up behind systemd.
    for _ in $(seq 10); do
        [ "$(systemctl --user is-active "$UNIT")" = active ] && break
        sleep 1
    done
    [ "$(systemctl --user is-active "$UNIT")" = active ] || DOWN+=("$UNIT")
done
if [ ${#DOWN[@]} -gt 0 ]; then
    echo ""
    echo "ERROR: the restore succeeded but these units did NOT come back up:"
    printf '         %s\n' "${DOWN[@]}"
    echo "       Backups are OFF until the sidecar runs. Diagnose and start it:"
    echo "         systemctl --user status ${SIDECAR}.service"
    echo "         systemctl --user reset-failed ${SIDECAR}.service && systemctl --user start ${SIDECAR}.service"
    exit 1
fi
