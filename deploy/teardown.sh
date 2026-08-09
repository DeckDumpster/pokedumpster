#!/usr/bin/env bash
#
# Stop and remove a PokeDumpster instance.
# With --purge, also deletes the data volume (the catalog + collection).
#
# Usage:
#   bash deploy/teardown.sh <instance> [--purge]
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

PURGE=false
POSITIONAL=()
for arg in "$@"; do
    case $arg in
        --purge) PURGE=true ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/teardown.sh <instance> [--purge]"
    exit 1
fi

INSTANCE="${POSITIONAL[0]}"
SERVICE_NAME="pkdump-${INSTANCE}"
QUADLET_FILE="$HOME/.config/containers/systemd/${SERVICE_NAME}.container"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Remove from the store the instance was CREATED in, not whichever one this
# shell happens to point at — otherwise a throwaway instance's image and volume
# leak into a store nothing cleans. The unit file records it (pd-fite).
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

echo "==> Stopping ${SERVICE_NAME}..."
systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true

# Stop and disable the per-instance refresh + backup-check timers (ivq).
systemctl --user disable --now "pkdump-refresh@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-backup-check@${INSTANCE}.timer" 2>/dev/null || true

# Stop the Litestream backup sidecar (pokedumpster-8ch.3).
systemctl --user stop "pkdump-litestream-${INSTANCE}.service" 2>/dev/null || true

rm -f "$QUADLET_FILE" \
      "$HOME/.config/containers/systemd/pkdump-litestream-${INSTANCE}.container"
systemctl --user daemon-reload

podman rmi "pkdump:${INSTANCE}" 2>/dev/null || true

if [ "$PURGE" = true ]; then
    podman volume rm "pkdump-${INSTANCE}-data" 2>/dev/null || true
    # Litestream config + AWS creds for this instance (holds secrets).
    rm -rf "$HOME/.config/pkdump/${INSTANCE}"
    echo "==> ${INSTANCE} removed (data volume + backup config purged)."
else
    echo "==> ${INSTANCE} removed. Data volume kept — add --purge to delete it."
fi
