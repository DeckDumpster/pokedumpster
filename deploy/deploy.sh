#!/usr/bin/env bash
#
# Rebuild the image and restart a PokeDumpster instance.
# Delegates to setup.sh if the instance does not exist yet.
#
# Usage:
#   bash deploy/deploy.sh <instance>
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

if [ $# -lt 1 ]; then
    echo "Usage: bash deploy/deploy.sh <instance>"
    exit 1
fi

INSTANCE="$1"
SERVICE_NAME="pkdump-${INSTANCE}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
QUADLET_FILE="$HOME/.config/containers/systemd/${SERVICE_NAME}.container"

if [ ! -f "$QUADLET_FILE" ]; then
    echo "==> No instance '$INSTANCE' yet — running setup..."
    bash "$SCRIPT_DIR/setup.sh" "$INSTANCE"
else
    echo "==> Rebuilding image pkdump:${INSTANCE}..."
    podman build -t pkdump:latest -f "$REPO_DIR/Containerfile" "$REPO_DIR"
    podman tag pkdump:latest "pkdump:${INSTANCE}"
fi

echo "==> Reloading systemd and restarting ${SERVICE_NAME}..."
systemctl --user daemon-reload
systemctl --user restart "$SERVICE_NAME"

echo "==> ${SERVICE_NAME} restarted. Port: podman port systemd-${SERVICE_NAME}"
