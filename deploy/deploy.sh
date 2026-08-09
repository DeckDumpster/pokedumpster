#!/usr/bin/env bash
#
# Rebuild the image, reinstall the unit files, and restart a PokeDumpster
# instance. Delegates to setup.sh if the instance does not exist yet.
#
# "Reinstall the unit files" is the whole of pd-2t6u: the units under deploy/ are
# templates copied into ~/.config at install time, and until this script started
# re-rendering them, only setup.sh ever did — so a deploy shipped the new binary
# and left the units at whatever version the instance was created with. Prod ran
# a pre-multi-tenant Litestream unit, with no OnFailure= alerting, for months.
#
# The instance's published port is preserved across the refresh (see
# deploy/units-lib.sh) — shipping a unit-file change must not move the address.
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

# Rebuild into the store the instance already lives in (pd-fite). Prod's unit
# carries no store flags, so prod keeps using Podman's default store.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

if [ ! -f "$QUADLET_FILE" ]; then
    echo "==> No instance '$INSTANCE' yet — running setup..."
    bash "$SCRIPT_DIR/setup.sh" "$INSTANCE"
else
    echo "==> Rebuilding image pkdump:${INSTANCE}..."
    podman build -t pkdump:latest -f "$REPO_DIR/Containerfile" "$REPO_DIR"
    podman tag pkdump:latest "pkdump:${INSTANCE}"

    # Ship the unit files too, not just the image (pd-2t6u). The units under
    # deploy/ are templates that get copied into ~/.config at install time, so an
    # existing instance kept running whatever copy setup.sh made — for prod, the
    # pre-multi-tenant sidecar unit from before the alarming layers existed. A
    # deploy that updates the binary and leaves the units on last year's version
    # is how the repo came to disagree with the box for months.
    #
    # No port argument: the instance keeps the port it already publishes. Passing
    # nothing here would otherwise mean "let Podman pick", which moves prod off
    # 8090 — refreshing the units must not change the address they serve on.
    echo "==> Installing unit files from this checkout..."
    # shellcheck source=deploy/units-lib.sh
    . "$SCRIPT_DIR/units-lib.sh"
    pkdump_units_install "$INSTANCE"
    pkdump_units_report
fi

echo "==> Reloading systemd and restarting ${SERVICE_NAME}..."
systemctl --user daemon-reload
systemctl --user restart "$SERVICE_NAME"

# The sidecar is a separate unit with its own container: daemon-reload makes
# systemd read the new file, but the running container keeps the arguments it
# was started with until it is restarted. Only when it is already running —
# starting a stopped sidecar is an operator decision (it is gated on backups
# being configured at all), not something a deploy gets to make.
if systemctl --user is-active --quiet "pkdump-litestream-${INSTANCE}.service"; then
    echo "==> Restarting the Litestream sidecar..."
    systemctl --user restart "pkdump-litestream-${INSTANCE}.service"
fi

echo "==> ${SERVICE_NAME} restarted. Port: podman port systemd-${SERVICE_NAME}"
