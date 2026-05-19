#!/usr/bin/env bash
#
# Rebuild and restart a single PokeDumpster instance on macOS (no systemd).
# Run from within the repo clone. The data volume is preserved.
#
# Usage:
#   bash deploy/mac-deploy.sh <instance>
#
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "Usage: bash deploy/mac-deploy.sh <instance>"
    exit 1
fi

INSTANCE="$1"
CONTAINER_NAME="pkdump-${INSTANCE}"
VOLUME="${CONTAINER_NAME}-data"
IMAGE="localhost/pkdump:${INSTANCE}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

if ! podman machine inspect --format '{{.State}}' 2>/dev/null | grep -q "running"; then
    echo "ERROR: Podman machine is not running. Start it: podman machine start"
    exit 1
fi

echo "==> Rebuilding image pkdump:${INSTANCE}..."
podman build -t "pkdump:${INSTANCE}" -f "$REPO_DIR/Containerfile" "$REPO_DIR"

echo "==> Restarting container ($CONTAINER_NAME)..."
podman stop "$CONTAINER_NAME" 2>/dev/null || true
podman rm "$CONTAINER_NAME" 2>/dev/null || true
podman run -d \
    --name "$CONTAINER_NAME" \
    -e PKDUMP_HOME=/data \
    -p ":8080" \
    -v "${VOLUME}:/data" \
    "$IMAGE"

# Discover the assigned port and wait for the server to answer.
sleep 2
PORT=$(podman port "$CONTAINER_NAME" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)
if [ -z "$PORT" ]; then
    echo "ERROR: could not determine port. Check: podman port $CONTAINER_NAME"
    exit 1
fi
echo "==> Listening on port $PORT"

echo "==> Health check..."
for i in $(seq 1 15); do
    if curl -sf --connect-timeout 3 -o /dev/null "http://localhost:${PORT}/"; then
        echo "==> Health check passed (attempt $i)."
        echo "    URL: http://localhost:${PORT}"
        exit 0
    fi
    echo "    Attempt $i/15 failed, waiting 2s..."
    sleep 2
done

echo "==> Health check FAILED. Check: podman logs $CONTAINER_NAME"
exit 1
