#!/usr/bin/env bash
#
# Restore a PokeDumpster instance's per-user collection database from a
# backup snapshot produced by deploy/backup.sh.
#
# Stops the instance, replaces <user>.sqlite, restarts, verifies integrity.
# The shared catalog (shared.sqlite) is left untouched — it is reproducible
# and is not part of a backup.
#
# Usage:
#   bash deploy/restore.sh [--yes] <backup-file.sqlite> [instance] [user]
#
# Options:
#   --yes, -y   Skip the confirmation prompt (for scripted use).
#
# Examples:
#   bash deploy/restore.sh ~/pkdump-backups/prod/daily/pkdump-prod-20260519-020000.sqlite prod
#   bash deploy/restore.sh --yes <file> prod collection
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

# --- Parse arguments --------------------------------------------------------

YES=false
POSITIONAL=()
for arg in "$@"; do
    case $arg in
        --yes|-y) YES=true ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/restore.sh [--yes] <backup-file.sqlite> [instance] [user]"
    exit 1
fi

BACKUP_FILE="${POSITIONAL[0]}"
INSTANCE="${POSITIONAL[1]:-prod}"
USER_DB="${POSITIONAL[2]:-collection}"
SERVICE_NAME="pkdump-${INSTANCE}"
CONTAINER="systemd-${SERVICE_NAME}"
VOLUME="pkdump-${INSTANCE}-data"

echo "==> PokeDumpster restore"
echo "    Backup:    $BACKUP_FILE"
echo "    Instance:  $INSTANCE"
echo "    User DB:   ${USER_DB}.sqlite"
echo "    Volume:    $VOLUME"

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "ERROR: sqlite3 not found on host. Install it: sudo apt install sqlite3"
    exit 1
fi

# --- Validate the backup file ----------------------------------------------

if [ ! -f "$BACKUP_FILE" ]; then
    echo "ERROR: backup file not found: $BACKUP_FILE"
    exit 1
fi

INTEGRITY="$(sqlite3 "file:${BACKUP_FILE}?mode=ro" 'PRAGMA integrity_check;' 2>/dev/null || echo "FAILED")"
if [ "$INTEGRITY" != "ok" ]; then
    echo "ERROR: backup file failed integrity check (${INTEGRITY})."
    echo "    This does not look like a valid PokeDumpster backup."
    exit 1
fi
echo "    Backup file validated."

if ! podman volume exists "$VOLUME" 2>/dev/null; then
    echo "ERROR: data volume '$VOLUME' not found — has '$INSTANCE' been set up?"
    exit 1
fi

# --- Confirm ----------------------------------------------------------------

if [ "$YES" = false ]; then
    echo ""
    echo "WARNING: this overwrites ${USER_DB}.sqlite for instance '$INSTANCE'."
    read -r -p "Continue? [y/N] " response
    if [[ ! "$response" =~ ^[Yy]$ ]]; then
        echo "Restore cancelled."
        exit 0
    fi
fi

# --- Stop the instance ------------------------------------------------------

echo "==> Stopping $SERVICE_NAME..."
systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true
sleep 2

# --- Replace the user database ---------------------------------------------

MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"
DEST_DB="${MOUNTPOINT}/${USER_DB}.sqlite"

echo "==> Restoring ${USER_DB}.sqlite onto ${VOLUME}..."
# Remove any stale WAL/SHM sidecar files from the previous database.
rm -f "${DEST_DB}-wal" "${DEST_DB}-shm"
# Copy atomically: write a temp file alongside, then rename into place.
cp "$BACKUP_FILE" "${DEST_DB}.restore-tmp"
mv -f "${DEST_DB}.restore-tmp" "$DEST_DB"

# --- Restart ----------------------------------------------------------------

echo "==> Starting $SERVICE_NAME..."
systemctl --user start "$SERVICE_NAME"
sleep 3

# --- Verify -----------------------------------------------------------------

echo "==> Verifying restored database..."
VERIFY="$(sqlite3 "file:${DEST_DB}?mode=ro" \
    "SELECT 'OK — ' || COUNT(*) || ' collection rows' FROM collection;" 2>&1 || echo "verify failed")"
echo "    $VERIFY"

echo "==> Restore complete. Check: systemctl --user status $SERVICE_NAME"
echo "    Container: podman port $CONTAINER 8080/tcp"
