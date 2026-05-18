#!/usr/bin/env bash
#
# Set up a PokeDumpster container instance (rootless Podman). No sudo.
# Run from within the repo clone.
#
# Usage:
#   bash deploy/setup.sh <instance> [port]
#
# Examples:
#   bash deploy/setup.sh prod 8080     # explicit host port
#   bash deploy/setup.sh feature-xyz   # auto-assigned host port
#
# After setup, populate the catalog with deploy/seed.sh and start the
# service with: systemctl --user start pkdump-<instance>
#
set -euo pipefail

# systemctl --user needs XDG_RUNTIME_DIR; non-interactive shells often lack it.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

if [ $# -lt 1 ]; then
    echo "Usage: bash deploy/setup.sh <instance> [port]"
    exit 1
fi

INSTANCE="$1"
PORT="${2:-0}"
SERVICE_NAME="pkdump-${INSTANCE}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
QUADLET_DIR="$HOME/.config/containers/systemd"

if ! command -v podman >/dev/null 2>&1; then
    echo "ERROR: podman not found. Install it: sudo apt install podman"
    exit 1
fi

if ! loginctl show-user "$USER" -p Linger 2>/dev/null | grep -q "Linger=yes"; then
    echo "WARNING: linger not enabled — services stop when you log out."
    echo "  Fix: loginctl enable-linger $USER"
fi

echo "==> Building image pkdump:${INSTANCE}..."
podman build -t pkdump:latest -f "$REPO_DIR/Containerfile" "$REPO_DIR"
podman tag pkdump:latest "pkdump:${INSTANCE}"

echo "==> Installing Quadlet unit..."
mkdir -p "$QUADLET_DIR"
# PORT=0 -> ":8080" lets Podman pick a free host port.
if [ "$PORT" = "0" ]; then
    PORT_MAPPING=":8080"
else
    PORT_MAPPING="${PORT}:8080"
fi
sed \
    -e "s|{{INSTANCE}}|${INSTANCE}|g" \
    -e "s|{{PORT}}:8080|${PORT_MAPPING}|g" \
    "$REPO_DIR/deploy/pkdump.container" \
    > "${QUADLET_DIR}/${SERVICE_NAME}.container"

systemctl --user daemon-reload

echo ""
echo "==> Setup complete."
echo "    Populate catalog:  bash deploy/seed.sh ${INSTANCE}"
echo "    Start:             systemctl --user start ${SERVICE_NAME}"
echo "    Port:              podman port systemd-${SERVICE_NAME}"
echo "    Logs:              journalctl --user -u ${SERVICE_NAME} -f"
echo "    Teardown:          bash deploy/teardown.sh ${INSTANCE}"
