#!/usr/bin/env bash
#
# Tear down a PokeDumpster container instance on macOS (no systemd).
# Stops and removes the container and image. The data volume is preserved
# unless --purge is passed.
#
# Usage:
#   bash deploy/mac-teardown.sh <instance>          # keep data
#   bash deploy/mac-teardown.sh <instance> --purge  # remove everything
#
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "Usage: bash deploy/mac-teardown.sh <instance> [--purge]"
    exit 1
fi

INSTANCE="$1"
PURGE="${2:-}"
CONTAINER_NAME="pkdump-${INSTANCE}"
VOLUME="${CONTAINER_NAME}-data"

echo "==> Tearing down $CONTAINER_NAME..."
podman stop "$CONTAINER_NAME" 2>/dev/null || true
podman rm "$CONTAINER_NAME" 2>/dev/null || true
podman rmi "pkdump:${INSTANCE}" 2>/dev/null || true

if [ "$PURGE" = "--purge" ]; then
    podman volume rm "$VOLUME" 2>/dev/null || true
    echo "==> ${INSTANCE} removed (data volume purged)."
else
    echo "==> ${INSTANCE} removed. Data volume kept — add --purge to delete it."
fi
