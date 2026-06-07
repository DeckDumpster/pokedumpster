#!/usr/bin/env bash
#
# Layer 4 — low-disk alert (pokedumpster-ivq.4). The host hit 94% during the
# Jun 2026 backup work; a full disk silently breaks backups, image builds, and
# the DB. This pushes a Pushover alert when the watched filesystem crosses a
# threshold. Host-wide (disk is not per-instance) — install/enable once.
#
# Env-driven (host-wide ~/.config/pkdump/alerts.env):
#   PKDUMP_DISK_THRESHOLD   percent-used that triggers an alert (default 90)
#   PKDUMP_DISK_PATH        filesystem to watch (default $HOME — where the
#                           podman volumes + image storage live under rootless)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
[ -f "${HOME}/.config/pkdump/alerts.env" ] && { set -a; . "${HOME}/.config/pkdump/alerts.env"; set +a; }

THRESHOLD="${PKDUMP_DISK_THRESHOLD:-90}"
DISK_PATH="${PKDUMP_DISK_PATH:-$HOME}"

# Use% of the filesystem backing DISK_PATH, digits only.
USE="$(df --output=pcent "$DISK_PATH" | tail -n1 | tr -dc '0-9')"
echo "diskcheck: ${DISK_PATH} at ${USE}% (threshold ${THRESHOLD}%)"

if [ "$USE" -ge "$THRESHOLD" ]; then
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster LOW DISK (${USE}%)" \
        "$(df -h "$DISK_PATH" | tail -n1) on $(hostname) — over ${THRESHOLD}% threshold"
fi
