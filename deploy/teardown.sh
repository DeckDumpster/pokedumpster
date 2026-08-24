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

# Stop and disable the per-instance refresh + backup-check timers (ivq), the
# offline catalog derive (pd-1uem), the nightly catalog.prices build (pd-up36),
# the transform tier's nightly run (pd-8m5c) and the ownership shipment
# (pd-dxn3) — an instance that is gone must not leave a timer behind still
# firing at its volume, its bucket and its lake
# network.
systemctl --user disable --now "pkdump-refresh@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-backup-check@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-derive@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-prices@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-value-snapshots@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-ship@${INSTANCE}.timer" 2>/dev/null || true
systemctl --user disable --now "pkdump-heartbeat@${INSTANCE}.timer" 2>/dev/null || true

# Stop the Litestream backup sidecar (pokedumpster-8ch.3).
systemctl --user stop "pkdump-litestream-${INSTANCE}.service" 2>/dev/null || true

rm -f "$QUADLET_FILE" \
      "$HOME/.config/containers/systemd/pkdump-litestream-${INSTANCE}.container"
systemctl --user daemon-reload

# Clear this instance's failed state (pd-n0lf). A unit stopped as part of a
# teardown is not a failure, but systemd keeps it in `--failed` forever, and once
# the unit file is gone it lingers as an unclearable `not-found` ghost. 125 had
# accumulated by 2026-08-13, 61 of them ghosts, which is enough noise to hide a
# real failure in the same listing.
for _u in "$SERVICE_NAME" \
          "pkdump-litestream-${INSTANCE}.service" \
          "pkdump-refresh@${INSTANCE}."{service,timer} \
          "pkdump-backup-check@${INSTANCE}."{service,timer} \
          "pkdump-derive@${INSTANCE}."{service,timer} \
          "pkdump-prices@${INSTANCE}."{service,timer} \
          "pkdump-value-snapshots@${INSTANCE}."{service,timer} \
          "pkdump-ship@${INSTANCE}."{service,timer} \
          "pkdump-heartbeat@${INSTANCE}."{service,timer}; do
    systemctl --user reset-failed "$_u" 2>/dev/null || true
done

podman rmi "pkdump:${INSTANCE}" 2>/dev/null || true

if [ "$PURGE" = true ]; then
    podman volume rm "pkdump-${INSTANCE}-data" 2>/dev/null || true
    # Litestream config + AWS creds for this instance (holds secrets).
    rm -rf "$HOME/.config/pkdump/${INSTANCE}"
    echo "==> ${INSTANCE} removed (data volume + backup config purged)."
else
    echo "==> ${INSTANCE} removed. Data volume kept — add --purge to delete it."
fi
