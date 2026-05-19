#!/usr/bin/env bash
#
# Back up a PokeDumpster instance's per-user collection database.
# Runs on the host, outside the container. No sudo required.
#
# Usage:
#   bash deploy/backup.sh [instance] [user]
#
#   instance  defaults to "prod"
#   user      defaults to "collection" (the default PKDUMP_USER)
#
# What gets backed up:
#   Only <user>.sqlite — the per-user collection. The shared catalog
#   (shared.sqlite) is fully reproducible from upstream via `pkdump setup`,
#   so it is deliberately NOT backed up.
#
# How:
#   A proper online snapshot via `sqlite3 .backup` against the live DB on
#   the instance's data volume. `.backup` is WAL-safe and consistent even
#   while the server is writing — no need to stop the instance.
#
# Backup directory (default ~/pkdump-backups):
#   Override with PKDUMP_BACKUP_DIR.
#
# Retention:
#   daily/   — last 7
#   weekly/  — last 8  (~2 months)
#   monthly/ — last 12 (~1 year)
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:-prod}"
USER_DB="${2:-collection}"
VOLUME="pkdump-${INSTANCE}-data"
BACKUP_DIR="${PKDUMP_BACKUP_DIR:-$HOME/pkdump-backups}"
INSTANCE_DIR="${BACKUP_DIR}/${INSTANCE}"
DAILY_DIR="${INSTANCE_DIR}/daily"
WEEKLY_DIR="${INSTANCE_DIR}/weekly"
MONTHLY_DIR="${INSTANCE_DIR}/monthly"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
BACKUP_NAME="pkdump-${INSTANCE}-${TIMESTAMP}.sqlite"

echo "==> PokeDumpster backup"
echo "    Instance:  $INSTANCE"
echo "    User DB:   ${USER_DB}.sqlite"
echo "    Volume:    $VOLUME"
echo "    Backup to: $INSTANCE_DIR"

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "ERROR: sqlite3 not found on host. Install it: sudo apt install sqlite3"
    exit 1
fi

if ! podman volume exists "$VOLUME" 2>/dev/null; then
    echo "ERROR: data volume '$VOLUME' not found."
    echo "    Has instance '$INSTANCE' been set up?"
    exit 1
fi

# --- Locate the live database on the volume --------------------------------
# Rootless Podman volumes are plain directories owned by the current user.

MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"
SRC_DB="${MOUNTPOINT}/${USER_DB}.sqlite"

if [ ! -f "$SRC_DB" ]; then
    echo "ERROR: ${USER_DB}.sqlite not found on the data volume."
    echo "    Looked at: $SRC_DB"
    echo "    Has the collection been initialized for instance '$INSTANCE'?"
    exit 1
fi

mkdir -p "$DAILY_DIR" "$WEEKLY_DIR" "$MONTHLY_DIR"

# --- Online snapshot via sqlite3 .backup -----------------------------------
# `.backup` takes a consistent snapshot of a live, possibly-being-written DB.

echo "==> Creating online snapshot via 'sqlite3 .backup'..."
sqlite3 "file:${SRC_DB}?mode=ro" ".backup '${DAILY_DIR}/${BACKUP_NAME}'"

# Verify the snapshot's integrity before we trust it.
INTEGRITY="$(sqlite3 "${DAILY_DIR}/${BACKUP_NAME}" 'PRAGMA integrity_check;')"
if [ "$INTEGRITY" != "ok" ]; then
    echo "ERROR: integrity check failed on the snapshot: $INTEGRITY"
    rm -f "${DAILY_DIR}/${BACKUP_NAME}"
    exit 1
fi

BACKUP_SIZE="$(du -h "${DAILY_DIR}/${BACKUP_NAME}" | cut -f1)"
echo "    Snapshot OK (${BACKUP_SIZE}): ${DAILY_DIR}/${BACKUP_NAME}"

# --- Retention -------------------------------------------------------------

prune_dir() {
    local dir="$1" keep="$2" count
    count=$(find "$dir" -maxdepth 1 -name 'pkdump-*.sqlite' | wc -l)
    if [ "$count" -gt "$keep" ]; then
        local to_remove=$((count - keep))
        echo "    Pruning ${to_remove} old backup(s) from $(basename "$dir")/ (keep ${keep})"
        find "$dir" -maxdepth 1 -name 'pkdump-*.sqlite' -print0 \
            | sort -z | head -z -n "$to_remove" | xargs -0 rm -f
    fi
}

promote_oldest() {
    local src_dir="$1" dst_dir="$2" src_keep="$3" src_count oldest
    src_count=$(find "$src_dir" -maxdepth 1 -name 'pkdump-*.sqlite' | wc -l)
    if [ "$src_count" -gt "$src_keep" ]; then
        oldest=$(find "$src_dir" -maxdepth 1 -name 'pkdump-*.sqlite' -print0 \
            | sort -z | head -z -n 1 | tr -d '\0')
        if [ -n "$oldest" ]; then
            echo "    Promoting $(basename "$oldest") to $(basename "$dst_dir")/"
            mv "$oldest" "$dst_dir/"
        fi
    fi
}

echo "==> Running retention pruning..."
# Promote before pruning so the oldest is preserved one tier up.
promote_oldest "$WEEKLY_DIR" "$MONTHLY_DIR" 8
promote_oldest "$DAILY_DIR"  "$WEEKLY_DIR"  7
prune_dir "$DAILY_DIR"   7
prune_dir "$WEEKLY_DIR"  8
prune_dir "$MONTHLY_DIR" 12

echo "==> Backup complete: ${DAILY_DIR}/${BACKUP_NAME}"
