#!/usr/bin/env bash
#
# Disaster recovery: restore a PokeDumpster instance's collection database from
# its Litestream S3 replica (pokedumpster-8ch.5).
#
# ── RUNBOOK ─────────────────────────────────────────────────────────────────
# WHEN TO USE
#   The per-user collection DB on the data volume is lost or corrupt and you
#   want to recover from the off-box S3 replica written by the Litestream
#   sidecar (deploy/pkdump-litestream.container). This is the only backup now;
#   the local snapshot scripts were removed (S3-only). See deploy/RESTORE.md.
#
# WHAT IT DOES
#   1. Stops the app service + the Litestream sidecar (so nothing writes/replicates).
#   2. Runs `litestream restore` from S3 onto the data volume (temp-then-rename).
#   3. Restarts the app + sidecar.
#   4. Verifies the restored row count.
#
#   The shared catalog (shared.sqlite) is NOT restored — it is reproducible:
#   rebuild it with `deploy/seed.sh <instance>`.
#
# CREDENTIALS
#   Assume-role profile in ~/.config/pkdump/<instance>/aws/config + the bootstrap
#   key from podman secret pkdump-<instance>-s3-bootstrap (same as the sidecar) —
#   auto-refreshing temporary credentials via role assumption, never static keys.
#
# USAGE
#   bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] <instance> [user]
#     --yes, -y      skip the confirmation prompt (scripted use)
#     --at=<time>    point-in-time restore, e.g. --at=2026-06-01T12:00:00Z
#                    (default: latest; within the 6-month retention window)
#   Examples:
#     bash deploy/restore-litestream.sh prod
#     bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod
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
    echo "Usage: bash deploy/restore-litestream.sh [--yes] <instance> [user]"
    exit 1
fi

INSTANCE="${POSITIONAL[0]}"
USER_DB="${POSITIONAL[1]:-collection}"
SERVICE_NAME="pkdump-${INSTANCE}"
SIDECAR="pkdump-litestream-${INSTANCE}"
VOLUME="pkdump-${INSTANCE}-data"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LS_YML="${REPO_DIR}/deploy/litestream.yml"
LS_IMG="docker.io/litestream/litestream:latest"

echo "==> PokeDumpster Litestream restore"
echo "    Instance: ${INSTANCE}   User DB: ${USER_DB}.sqlite   Volume: ${VOLUME}"
[ -n "$AT" ] && echo "    Point-in-time: ${AT}"

# --- Validate --------------------------------------------------------------
command -v sqlite3 >/dev/null 2>&1 || { echo "ERROR: sqlite3 not found on host. Install: sudo apt install sqlite3"; exit 1; }
[ -f "${CONF_DIR}/litestream.env" ] || { echo "ERROR: ${CONF_DIR}/litestream.env not found — is the sidecar configured?"; exit 1; }
[ -f "${CONF_DIR}/aws/config" ] || { echo "ERROR: ${CONF_DIR}/aws/config not found — is the sidecar configured?"; exit 1; }
podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1 || { echo "ERROR: podman secret 'pkdump-${INSTANCE}-s3-bootstrap' not found."; exit 1; }
podman volume exists "$VOLUME" 2>/dev/null || { echo "ERROR: data volume '${VOLUME}' not found."; exit 1; }

# S3 target comes from the instance's env file (bucket / path / region / AWS_PROFILE).
set -a; . "${CONF_DIR}/litestream.env"; set +a
: "${LITESTREAM_S3_BUCKET:?missing in litestream.env}"
: "${LITESTREAM_S3_PATH:?missing in litestream.env}"
: "${LITESTREAM_S3_REGION:?missing in litestream.env}"

# --- Confirm ---------------------------------------------------------------
if [ "$YES" = false ]; then
    echo ""
    echo "WARNING: overwrites ${USER_DB}.sqlite on '${VOLUME}' from"
    echo "         s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_PATH}"
    read -r -p "Continue? [y/N] " response
    [[ "$response" =~ ^[Yy]$ ]] || { echo "Restore cancelled."; exit 0; }
fi

# --- Stop writers ----------------------------------------------------------
echo "==> Stopping ${SERVICE_NAME} + ${SIDECAR}..."
systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true
systemctl --user stop "${SIDECAR}.service" 2>/dev/null || true
sleep 2

MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"
DEST="${MOUNTPOINT}/${USER_DB}.sqlite"
TMP="${MOUNTPOINT}/${USER_DB}.sqlite.restore-tmp"

# --- Restore from S3 -------------------------------------------------------
# litestream restore refuses to overwrite an existing file, so restore to a temp
# path on the volume, then atomically rename into place.
echo "==> Restoring ${USER_DB}.sqlite from S3 via litestream..."
rm -f "$TMP"
RESTORE_FLAGS=(restore -config /etc/litestream.yml)
[ -n "$AT" ] && RESTORE_FLAGS+=(-timestamp "$AT")     # point-in-time restore
RESTORE_FLAGS+=(-o "/data/${USER_DB}.sqlite.restore-tmp" "/data/${USER_DB}.sqlite")
podman run --rm --user 0:0 \
    -v "${VOLUME}:/data" \
    -v "${LS_YML}:/etc/litestream.yml:ro" \
    -v "${CONF_DIR}/aws/config:/aws/config:ro" \
    --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
    -e AWS_CONFIG_FILE=/aws/config \
    -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
    -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
    -e LITESTREAM_S3_BUCKET="$LITESTREAM_S3_BUCKET" \
    -e LITESTREAM_S3_REGION="$LITESTREAM_S3_REGION" \
    -e LITESTREAM_S3_PATH="$LITESTREAM_S3_PATH" \
    -e LITESTREAM_DB_PATH="/data/${USER_DB}.sqlite" \
    "$LS_IMG" "${RESTORE_FLAGS[@]}"

# --- Atomic swap -----------------------------------------------------------
rm -f "${DEST}-wal" "${DEST}-shm"
mv -f "$TMP" "$DEST"

# --- Restart ---------------------------------------------------------------
echo "==> Starting ${SERVICE_NAME} (+ sidecar)..."
systemctl --user start "$SERVICE_NAME" 2>/dev/null || true
systemctl --user start "${SIDECAR}.service" 2>/dev/null || true

# --- Verify ----------------------------------------------------------------
ROWS="$(sqlite3 "file:${DEST}?mode=ro" 'SELECT count(*) FROM collection;' 2>&1 || echo '?')"
echo "==> Restore complete — ${USER_DB}.sqlite has ${ROWS} collection rows."
echo "    Shared catalog not restored (reproducible): bash deploy/seed.sh ${INSTANCE}"
