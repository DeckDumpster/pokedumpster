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
# This checkout ships TWO images, and until pd-rn4c only one of them had a
# deploy path. See the lake block at the bottom of this file.
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

# --- The other image this checkout ships (pd-rn4c) --------------------------
#
# `lake/` builds a SECOND image — the PyIceberg job runtime the nightly price
# build and the value-snapshots transform run in — and until now nothing but the
# one-time installer, deploy/setup-lake.sh, ever built it. So a change under
# lake/ reached a box only if an operator remembered a second command, and the
# stale half is invisible: the jobs keep exiting 0 over yesterday's code.
#
# What it cost: on 2026-08-26 prod's job image predated pd-bbv7 by six hours
# while its checkout did not. `catalog.sealed_prices` was never written and the
# transform recorded no dimension='sealed' row, so the chart reported $10,636.81
# of cards with $10,351.47 of sealed product beside it and no line for it —
# three real runs, each "1 tenant(s) snapshotted, 0 skipped". Exactly pd-2t6u's
# shape: a deploy that ships part of what the checkout says it ships.
#
# AFTER the app has been restarted, deliberately. The app is what serves
# requests; a lake build that fails must not be able to leave the new binary
# built and not running.
#
# Only for an instance that already HAS a lakehouse. Installing one is
# setup-lake.sh's job — it needs a bucket name (deploy/LAKE.md) that this script
# has no business inventing, and a box with no lakehouse is the ordinary case.
# shellcheck source=deploy/lake-lib.sh
. "$SCRIPT_DIR/lake-lib.sh"
if pkdump_lake_installed "$INSTANCE"; then
    echo "==> Rebuilding the lake job image $(pkdump_lake_job_image "$INSTANCE")..."
    pkdump_lake_job_image_build "$INSTANCE" "$REPO_DIR"
    # Nothing to restart: the lake jobs are one-off containers their timers
    # start, so the next scheduled run is the one that picks this up.
    echo "==> Lake job image rebuilt. Its timers run the new image from their next run."
else
    echo "==> No lakehouse installed for '${INSTANCE}' — lake job image skipped."
    echo "    (bash deploy/setup-lake.sh ${INSTANCE} installs one.)"
fi
