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

echo "==> Stopping ${SERVICE_NAME}..."
systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true

rm -f "$QUADLET_FILE"
systemctl --user daemon-reload

podman rmi "pkdump:${INSTANCE}" 2>/dev/null || true

if [ "$PURGE" = true ]; then
    podman volume rm "pkdump-${INSTANCE}-data" 2>/dev/null || true
    echo "==> ${INSTANCE} removed (data volume purged)."
else
    echo "==> ${INSTANCE} removed. Data volume kept — add --purge to delete it."
fi
