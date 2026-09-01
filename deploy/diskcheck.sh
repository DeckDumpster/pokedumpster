#!/usr/bin/env bash
#
# Low-disk checks. Two modes, one threshold source, one place to reason about
# how full is too full.
#
#   diskcheck.sh                     ALERT mode (Layer 4, pokedumpster-ivq.4).
#                                    Pushes a Pushover alert while there is still
#                                    room to act. Exits 0 having paged — it is a
#                                    timer, not a gate. The host hit 94% during
#                                    the Jun 2026 backup work; a full disk
#                                    silently breaks backups, image builds, and
#                                    the DB.
#
#                                    TWO ARMS, and the free-space one is the
#                                    point (pd-smcp). A percentage cannot say
#                                    whether work is about to be blocked: the
#                                    gate below is denominated in GIGABYTES, and
#                                    on this box's 98G root its 10G floor sits at
#                                    89.8% used — BELOW the 90% the alert fired
#                                    at. So the page could not arrive before the
#                                    thing it warns about. It arrived after, if
#                                    at all, and the actual failure mode stayed
#                                    'the next agent to need CI discovers it' —
#                                    three polecats in a row, 91% -> 95% -> 96%.
#                                    The free-space arm is stated in the floor's
#                                    own currency and PKDUMP_DISK_WARN_GB must
#                                    exceed the floor, so the page necessarily
#                                    precedes the block whatever the disk's size.
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
#                                    paths name it, and a path that does not exist
#                                    yet is measured on its nearest existing
#                                    ancestor — so a caller may hand it every
#                                    directory it writes to without knowing which
#                                    of them share a filesystem or which are
#                                    there yet. ci.sh does exactly that — see
#                                    PKDUMP_CI_DISK_PATHS, and note TMPDIR and
#                                    CARGO_TARGET_DIR among them: on this box
#                                    /tmp is its own mount and the target dir is
#                                    relocated to a third volume, so neither
#                                    $HOME nor the store root can see either fill
#                                    (pd-20ia, pd-6jyd).
#
# Env-driven (host-wide ~/.config/pkdump/alerts.env):
#   PKDUMP_DISK_THRESHOLD   percent-used at or over which alert mode pages
#                           (default 90). The size-independent arm: "this disk
#                           is nearly full" is worth saying on any disk.
#   PKDUMP_DISK_WARN_GB     gigabytes free below which alert mode pages
#                           (default 2x PKDUMP_DISK_FLOOR_GB). The arm that
#                           buys lead time, because it is denominated in the
#                           same unit the gate refuses in. Must be GREATER than
#                           the floor — a warning that fires no earlier than the
#                           failure is not a warning, so a value at or under it
#                           is REFUSED rather than clamped.
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
# Twice the floor: the page arrives once the headroom ABOVE the floor has shrunk
# to less than the floor itself. Proportional to whatever the operator declared
# work needs, so it scales with the floor instead of with a guess about disk size.
WARN_GB="${PKDUMP_DISK_WARN_GB:-$((FLOOR_GB * 2))}"
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

# A warn line at or under the floor cannot fire before the gate refuses, which
# makes this whole layer decorative — the exact state pd-smcp was filed over.
# Refuse it rather than clamping: the unit's OnFailure= then pages, and a guard
# that has been configured out of usefulness says so instead of running quietly.
if [ "$WARN_GB" -le "$FLOOR_GB" ]; then
    echo "diskcheck: REFUSING to run — PKDUMP_DISK_WARN_GB=${WARN_GB} is not above" >&2
    echo "  PKDUMP_DISK_FLOOR_GB=${FLOOR_GB}, so this alert could not fire before the" >&2
    echo "  gate that blocks builds. Raise the warn line or lower the floor." >&2
    exit 1
fi

# Use% and free space of the filesystem backing DISK_PATH, digits only.
USE="$(df --output=pcent "$DISK_PATH" | tail -n1 | tr -dc '0-9')"
FREE_GB="$(df -BG --output=avail "$DISK_PATH" | tail -n1 | tr -dc '0-9')"
MOUNT="$(df --output=target "$DISK_PATH" | tail -n1)"
echo "diskcheck: ${DISK_PATH} (${MOUNT}) at ${USE}% used, ${FREE_GB}G free" \
    "(warn under ${WARN_GB}G, floor ${FLOOR_GB}G, threshold ${THRESHOLD}%)"

# Most severe arm wins, and each arm has its OWN title. That is deliberate, and
# it is what keeps this honest under pd-hqdt's repeat suppression: the signature
# is the exact title plus the message with digit runs collapsed, so a title
# carrying today's percentage pages EVERY DAY on a box parked just over the line
# — different number, different signature, no suppression — which is the noise
# that trains the channel to be ignored. These titles carry the CONFIGURED limit
# instead, so a disk sitting still pages once and then goes quiet, while a disk
# that keeps falling crosses into the next title and pages again immediately.
# Two escalations, both meaning something: "act soon", then "work is blocked".
if [ "$FREE_GB" -lt "$FLOOR_GB" ]; then
    TITLE="PokeDumpster DISK BELOW FLOOR — under ${FLOOR_GB}G free on ${MOUNT}"
    WHY="free space is UNDER the ${FLOOR_GB}G floor: deploy/ci.sh will not build here, and a build that did would fail as a bus error rather than as a disk error."
elif [ "$FREE_GB" -lt "$WARN_GB" ]; then
    TITLE="PokeDumpster LOW DISK — under ${WARN_GB}G free on ${MOUNT}"
    WHY="free space is heading for the ${FLOOR_GB}G floor deploy/ci.sh refuses to build under. Acting now is cheaper than being blocked."
elif [ "$USE" -ge "$THRESHOLD" ]; then
    TITLE="PokeDumpster LOW DISK — over ${THRESHOLD}% used on ${MOUNT}"
    WHY="the filesystem is over the ${THRESHOLD}% threshold. A full disk silently breaks backups, image builds and the DB."
else
    exit 0
fi

# The remedies travel WITH the page. This disk has been reclaimed by hand three
# times; a page that arrives in time and still costs a fresh diagnosis has only
# moved the work earlier, not removed it.
"${SCRIPT_DIR}/alert.sh" "$TITLE" "$(cat <<EOF
$(df -h "$DISK_PATH" | tail -n1) on $(hostname)
${USE}% used, ${FREE_GB}G free — ${WHY}

Reclaim, safest first:
  bash deploy/teardown.sh <instance> --purge   # retire a finished non-prod instance
  bash deploy/store-teardown.sh <store-root>   # remove a whole non-prod store
Prod's store is ~/.local/share/containers and is SHARED with another project:
never prune it by hand. Builds there already collect the previous build's
orphans (deploy/image-lib.sh), so growth in it is worth investigating, not pruning.
EOF
)"
