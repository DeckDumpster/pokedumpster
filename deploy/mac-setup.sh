#!/usr/bin/env bash
#
# Set up a PokeDumpster container instance on macOS (rootless Podman, no
# systemd). Run from within the repo clone.
#
# Prerequisites (one-time):
#   brew install podman
#   podman machine init --memory 4096 --cpus 4
#   podman machine start          # also after each reboot
#
# Usage:
#   bash deploy/mac-setup.sh <instance> [--init] [--test]
#
# Examples:
#   bash deploy/mac-setup.sh feature-xyz
#   bash deploy/mac-setup.sh ci --test     # seed data volume from fixture
#   bash deploy/mac-setup.sh demo --init   # clone the pre-built seed volume
#
# Container naming:
#   Image:     pkdump:<instance>
#   Container: pkdump-<instance>
#   Volume:    pkdump-<instance>-data
#
set -euo pipefail

# --- Parse arguments --------------------------------------------------------

INIT=false
TEST=false
POSITIONAL=()
for arg in "$@"; do
    case $arg in
        --init) INIT=true ;;
        --test) TEST=true ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/mac-setup.sh <instance> [--init] [--test]"
    exit 1
fi

INSTANCE="${POSITIONAL[0]}"
CONTAINER_NAME="pkdump-${INSTANCE}"
VOLUME="${CONTAINER_NAME}-data"
IMAGE="localhost/pkdump:${INSTANCE}"
SEED_VOLUME="pkdump-seed-data"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "==> PokeDumpster macOS setup"
echo "    Instance:  $INSTANCE"
echo "    Container: $CONTAINER_NAME"
echo "    Repo:      $REPO_DIR"

# --- Prerequisites ----------------------------------------------------------

if ! command -v podman >/dev/null 2>&1; then
    echo "ERROR: podman not found. Install it: brew install podman"
    exit 1
fi

if ! podman machine inspect --format '{{.State}}' 2>/dev/null | grep -q "running"; then
    echo "ERROR: Podman machine is not running."
    echo "  Start it:    podman machine start"
    echo "  First time:  podman machine init --memory 4096 --cpus 4 && podman machine start"
    exit 1
fi

# --- Build the image --------------------------------------------------------

echo "==> Building image pkdump:${INSTANCE}..."
podman build -t "pkdump:${INSTANCE}" -f "$REPO_DIR/Containerfile" "$REPO_DIR"

# --- Optional data volume seeding ------------------------------------------

if [ "$TEST" = true ]; then
    FIXTURE_DIR="$REPO_DIR/tests/ui/fixtures"
    if [ ! -f "$FIXTURE_DIR/shared.sqlite" ] || [ ! -f "$FIXTURE_DIR/collection.sqlite" ]; then
        echo "ERROR: fixture files not found under ${FIXTURE_DIR}"
        exit 1
    fi
    echo "==> Seeding ${VOLUME} from committed fixture (offline)..."
    podman volume exists "$VOLUME" 2>/dev/null || podman volume create "$VOLUME" >/dev/null
    TEMP="pkdump-fixture-$$"
    podman run -d --name "$TEMP" -v "${VOLUME}:/data" --entrypoint sleep \
        "$IMAGE" infinity >/dev/null
    # Tenant collection DBs live under /data/tenants (deploy/TENANTS.md).
    podman exec "$TEMP" mkdir -p /data/tenants
    podman cp "$FIXTURE_DIR/shared.sqlite"     "${TEMP}:/data/shared.sqlite"
    podman cp "$FIXTURE_DIR/collection.sqlite" "${TEMP}:/data/tenants/collection.sqlite"
    podman rm -f "$TEMP" >/dev/null
elif [ "$INIT" = true ]; then
    if podman volume exists "$SEED_VOLUME" 2>/dev/null; then
        echo "==> Cloning seed volume into ${VOLUME}..."
        podman volume exists "$VOLUME" 2>/dev/null || podman volume create "$VOLUME" >/dev/null
        podman volume export "$SEED_VOLUME" | podman volume import "$VOLUME" -
    else
        echo "==> No seed volume found — running full 'pkdump setup' (slow)..."
        podman volume exists "$VOLUME" 2>/dev/null || podman volume create "$VOLUME" >/dev/null
        podman run --rm -v "${VOLUME}:/data" -e PKDUMP_HOME=/data \
            --entrypoint pkdump "$IMAGE" setup
    fi
fi

# --- Start the container ----------------------------------------------------

echo "==> Starting container ($CONTAINER_NAME)..."
podman stop "$CONTAINER_NAME" 2>/dev/null || true
podman rm "$CONTAINER_NAME" 2>/dev/null || true
podman run -d \
    --name "$CONTAINER_NAME" \
    -e PKDUMP_HOME=/data \
    -p ":8080" \
    -v "${VOLUME}:/data" \
    "$IMAGE"

# Discover the assigned port.
sleep 2
PORT=$(podman port "$CONTAINER_NAME" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)

echo ""
echo "==> Setup complete."
if [ -n "$PORT" ]; then
    echo "    URL:       http://localhost:${PORT}"
fi
echo "    Port:      podman port $CONTAINER_NAME 8080/tcp"
echo "    Logs:      podman logs -f $CONTAINER_NAME"
echo "    Stop:      podman stop $CONTAINER_NAME"
echo "    Teardown:  bash deploy/mac-teardown.sh $INSTANCE"
