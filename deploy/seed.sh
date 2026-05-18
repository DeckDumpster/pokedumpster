#!/usr/bin/env bash
#
# Populate a PokeDumpster instance's catalog by running `pkdump setup`
# in a one-off container against the instance's data volume.
#
# Usage:
#   bash deploy/seed.sh <instance> [extra pkdump setup args...]
#
# Examples:
#   bash deploy/seed.sh prod                       # full catalog build
#   bash deploy/seed.sh prod --skip-tail           # skip the API tail fetch
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

if [ $# -lt 1 ]; then
    echo "Usage: bash deploy/seed.sh <instance> [extra pkdump setup args...]"
    exit 1
fi

INSTANCE="$1"
shift
VOLUME="pkdump-${INSTANCE}-data"
IMAGE="localhost/pkdump:${INSTANCE}"

if ! podman image exists "$IMAGE" 2>/dev/null; then
    echo "ERROR: image $IMAGE not found — run deploy/setup.sh ${INSTANCE} first."
    exit 1
fi

podman volume exists "$VOLUME" 2>/dev/null || podman volume create "$VOLUME" >/dev/null

echo "==> Running 'pkdump setup' against ${VOLUME} (downloads the catalog)..."
podman run --rm \
    -v "${VOLUME}:/data:Z" \
    -e PKDUMP_HOME=/data \
    --entrypoint pkdump \
    "$IMAGE" \
    setup "$@"

echo "==> Catalog populated. Restart: systemctl --user restart pkdump-${INSTANCE}"
