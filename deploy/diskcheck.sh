#!/usr/bin/env bash
#
# Low-disk checks. Two modes, one threshold source, one place to reason about
# how full is too full.
#
#   diskcheck.sh                     ALERT mode (Layer 4, pokedumpster-ivq.4).
#                                    Pushes a Pushover alert when the watched
#                                    filesystem is at or over PKDUMP_DISK_THRESHOLD
#                                    percent. Always exits 0 — it is a timer, not
#                                    a gate. The host hit 94% during the Jun 2026
#                                    backup work; a full disk silently breaks
#                                    backups, image builds, and the DB.
#
#   diskcheck.sh --floor [path...]   GATE mode (pd-fite). Exits NON-ZERO when any
#                                    named path's filesystem has less than
#                                    PKDUMP_DISK_FLOOR_GB free. Run before work
#                                    that needs room — deploy/ci.sh calls it —
#                                    because running out mid-build does not
#                                    announce itself as a disk problem: at 697M
#                                    free a cargo link died with
#                                    `ld terminated with signal 7 [Bus error]`
#                                    and exit 101, which reads as a broken
#                                    toolchain and cost real time to diagnose.
#                                    With no paths given it checks PKDUMP_DISK_PATH.
#                                    Each DEVICE is reported once however many
#                                    paths name it, so a caller may hand it every
#                                    directory it writes to without knowing which
#                                    of them share a filesystem. ci.sh does
#                                    exactly that — see PKDUMP_CI_DISK_PATHS, and
#                                    note that TMPDIR is one of them: on a box
#                                    where /tmp is its own mount, neither $HOME
#                                    nor the store root can see it fill (pd-20ia).
#
# Env-driven (host-wide ~/.config/pkdump/alerts.env):
#   PKDUMP_DISK_THRESHOLD   percent-used that triggers an alert (default 90)
#   PKDUMP_DISK_FLOOR_GB    gigabytes free below which --floor fails (default 10)
#   PKDUMP_DISK_PATH        filesystem to watch (default $HOME — where the
#                           podman volumes + image storage live under rootless)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# PKDUMP_ALERTS_ENV names the host-wide file; production never sets it — only
# tests point it elsewhere so they never write the operator's real credentials.
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"
[ -f "$ALERTS_ENV" ] && { set -a; . "$ALERTS_ENV"; set +a; }

THRESHOLD="${PKDUMP_DISK_THRESHOLD:-90}"
FLOOR_GB="${PKDUMP_DISK_FLOOR_GB:-10}"
DISK_PATH="${PKDUMP_DISK_PATH:-$HOME}"

# --- Gate mode --------------------------------------------------------------

if [ "${1:-}" = "--floor" ]; then
    shift
    PATHS=("$@")
    [ ${#PATHS[@]} -gt 0 ] || PATHS=("$DISK_PATH")

    # Paths on the same filesystem are the same check; report each device once so
    # the output names disks rather than repeating one under several aliases.
    declare -A SEEN=()
    FAILED=0
    for p in "${PATHS[@]}"; do
        # A store root that does not exist yet still sits on some mounted
        # filesystem — walk up until df has something to measure.
        while [ ! -e "$p" ] && [ "$p" != "/" ]; do p="$(dirname "$p")"; done
        DEV="$(df --output=source "$p" | tail -n1)"
        [ -z "${SEEN[$DEV]:-}" ] || continue
        SEEN[$DEV]=1

        # df -BG gives whole gigabytes; digits only.
        FREE_GB="$(df -BG --output=avail "$p" | tail -n1 | tr -dc '0-9')"
        MOUNT="$(df --output=target "$p" | tail -n1)"
        if [ "$FREE_GB" -lt "$FLOOR_GB" ]; then
            echo "ERROR: only ${FREE_GB}G free on ${MOUNT} (floor ${FLOOR_GB}G)." >&2
            echo "  Builds that run out of room here do NOT fail as disk errors — the" >&2
            echo "  last time this happened a cargo link reported 'ld terminated with" >&2
            echo "  signal 7 [Bus error]'. Free space before re-running." >&2
            echo "  $(df -h "$p" | tail -n1)" >&2
            FAILED=1
        else
            echo "diskcheck: ${MOUNT} has ${FREE_GB}G free (floor ${FLOOR_GB}G) — ok"
        fi
    done
    exit "$FAILED"
fi

# --- Alert mode -------------------------------------------------------------

# Use% of the filesystem backing DISK_PATH, digits only.
USE="$(df --output=pcent "$DISK_PATH" | tail -n1 | tr -dc '0-9')"
echo "diskcheck: ${DISK_PATH} at ${USE}% (threshold ${THRESHOLD}%)"

if [ "$USE" -ge "$THRESHOLD" ]; then
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster LOW DISK (${USE}%)" \
        "$(df -h "$DISK_PATH" | tail -n1) on $(hostname) — over ${THRESHOLD}% threshold"
fi
