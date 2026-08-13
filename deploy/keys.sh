#!/usr/bin/env bash
#
# `pkdump keys`, against a real instance (pd-ulds). The wrapper that knows
# which per-instance master key file an instance uses.
#
# Usage:
#   bash deploy/keys.sh <instance> <subcommand> [args...]
#
#   bash deploy/keys.sh prod init
#   bash deploy/keys.sh prod status
#   bash deploy/keys.sh prod backup --yes
#   bash deploy/keys.sh prod register 01K2C7HQ8N...
#   bash deploy/keys.sh prod tombstone 01K2C7HQ8N... --yes --reason "account deleted"
#
# Runbook: deploy/KEYS.md. Read §1 before §5.
#
# ── WHY A WRAPPER AT ALL ────────────────────────────────────────────────────
# The master key is HOST config, per instance, and lives beside that instance's
# litestream.env — ~/.config/pkdump/<instance>/tenant-master.key, mode 600 in a
# directory at 700. The binary finds it through $PKDUMP_MASTER_KEY_FILE, and
# exporting that by hand is exactly the step somebody skips before running
# `keys init` and minting a second key in the default location. So the instance
# is named once, here, and every invocation is pointed at the right file.
#
# ── WHY THE KEY IS NOT ON THE DATA VOLUME ───────────────────────────────────
# The data volume is what Litestream replicates. Putting the key that protects
# the tenant zone into the same replication stream as the data it protects is
# how a key ends up stored beside its own ciphertext. It stays on the host, in
# the host-config directory, and is mounted read-only into the container for
# the commands that need it.
#
# ── THE TWO PATHS ARE STILL TWO PATHS HERE ──────────────────────────────────
# `backup` mounts the key and NOT the data volume; `tombstone` mounts the data
# volume and NOT the key. That is not tidiness — it is the same rule the Rust
# side is held to (crates/pkdump-keys/tests/separation.rs), carried through the
# one layer that could quietly undo it by mounting everything for everything.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: keys.sh <instance> <subcommand> [args...]}"
shift
SUBCOMMAND="${1:?usage: keys.sh <instance> <subcommand> [args...]}"
shift
ARGS=("$@")

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# The host-config directory for this instance — where litestream.env already
# lives. PKDUMP_KEYS_CONF_DIR is a test seam (the PKDUMP_ALERTS_ENV /
# PKDUMP_LAKE_ENV precedent), and it exists because the obvious alternative —
# a harness redirecting $HOME — ALSO redirects the rootless Podman store, so
# every image on the box disappears and this script reports "not built" about
# an image that is right there. Production sets none of these three.
CONF_DIR="${PKDUMP_KEYS_CONF_DIR:-${HOME}/.config/pkdump/${INSTANCE}}"
KEY_FILE="${CONF_DIR}/tenant-master.key"

IMAGE="${PKDUMP_KEYS_IMAGE:-localhost/pkdump:${INSTANCE}}"
DATA="${PKDUMP_KEYS_DATA:-pkdump-${INSTANCE}-data}"

# Look in the store the instance actually lives in (pd-fite). No-op for prod.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

if ! podman image exists "$IMAGE" 2>/dev/null; then
    echo "keys: no image ${IMAGE}." >&2
    echo "  '${INSTANCE}' has not been built on this box: bash deploy/setup.sh ${INSTANCE}" >&2
    exit 1
fi

# The host-config directory, at the mode the Litestream credentials beside it
# already carry. Created here so `init` on a fresh box does not need setup.sh to
# have run first — and chmodded here so a directory that predates this script
# is tightened rather than trusted.
mkdir -p "$CONF_DIR"
chmod 700 "$CONF_DIR"

# ── What each subcommand is allowed to see ──────────────────────────────────
# The key file is mounted read-write ONLY for the two commands that create one;
# everything else that needs it gets it read-only, and the commands that have no
# business with it do not get it at all.
MOUNTS=()
case "$SUBCOMMAND" in
init | restore)
    # These WRITE the key file, so the directory is mounted rather than the
    # file — the file does not exist yet.
    MOUNTS+=(-v "${CONF_DIR}:/keys:rw")
    ;;
backup | status | derive)
    if [ -e "$KEY_FILE" ]; then
        MOUNTS+=(-v "${KEY_FILE}:/keys/tenant-master.key:ro")
    fi
    ;;
tombstone | register | list)
    # THE DESTRUCTION PATH gets no key file, deliberately. Revoking a tenant
    # is a row in the registry; a command that cannot reach the master key
    # cannot be the command that deletes it by mistake.
    ;;
*)
    echo "keys: unknown subcommand '${SUBCOMMAND}'." >&2
    echo "  init | status | backup | restore | register | list | derive | tombstone" >&2
    echo "  See deploy/KEYS.md." >&2
    exit 1
    ;;
esac

# The data volume carries registry.sqlite — where key STATE lives. `backup`
# does not get it, for the same reason `tombstone` does not get the key.
case "$SUBCOMMAND" in
backup) ;;
*) MOUNTS+=(-v "${DATA}:/data:Z") ;;
esac

# ── Host paths in the arguments (`backup -o`, `restore -i`) ─────────────────
# An operator types a path on the HOST — that is the whole point of staging a
# copy on the way to a password manager, and of pasting one back. Passed
# through unchanged it would name a path inside the container, where the
# directory does not exist, and `backup -o ~/key` would fail with a bewildering
# "No such file or directory" about a path that plainly exists.
#
# So the file's DIRECTORY is bind-mounted at /io and the argument is rewritten
# to point there. Only the directory the operator named; nothing else on the
# host becomes visible, and only for the two subcommands that take such a flag.
rewrite_path_arg() { # rewrite_path_arg <short> <long>
    local short="$1" long="$2" i j host dir
    for i in "${!ARGS[@]}"; do
        host=""
        case "${ARGS[$i]}" in
        "$short" | "$long")
            j=$((i + 1))
            [ "$j" -lt "${#ARGS[@]}" ] || continue
            host="${ARGS[$j]}"
            ;;
        "$long"=*)
            j="$i"
            host="${ARGS[$i]#*=}"
            ;;
        *) continue ;;
        esac
        [ -n "$host" ] || continue
        # Not `mkdir -p`: a typo'd directory is a typo, and silently creating it
        # is how a backup lands somewhere nobody looks again.
        dir="$(cd "$(dirname "$host")" 2>/dev/null && pwd)" || {
            echo "keys: ${host} — its directory does not exist on this host." >&2
            exit 1
        }
        MOUNTS+=(-v "${dir}:/io:rw")
        if [ "$j" = "$i" ]; then
            ARGS[$j]="${long}=/io/$(basename "$host")"
        else
            ARGS[$j]="/io/$(basename "$host")"
        fi
        return 0
    done
}
case "$SUBCOMMAND" in
backup) rewrite_path_arg -o --out ;;
restore) rewrite_path_arg -i --input ;;
esac

# `restore` reads the key from stdin when it was handed no `-i`, so the
# container needs stdin passed through.
STDIN_ARGS=()
case "$SUBCOMMAND" in
restore) STDIN_ARGS+=(-i) ;;
esac

RUN=(
    podman run --rm --pull=never "${STDIN_ARGS[@]}"
    "${MOUNTS[@]}"
    -e PKDUMP_HOME=/data
    -e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key
    --entrypoint pkdump
    "$IMAGE" keys "$SUBCOMMAND" ${ARGS[@]+"${ARGS[@]}"}
)

# A test seam, and it is here for a specific reason. The claim this script makes
# is about its MOUNTS — that the destruction path is never handed the master key
# and the backup path is never handed the registry — and a gate can only check
# that by seeing the argv. tests/keys/run.sh §7 originally grepped this file for
# the branch LABELS instead, which stayed green when the mount was moved into
# the branch: a guard that could not see the thing it was guarding. So the argv
# is printable, one item per line, and the assertion is made against what would
# actually run.
if [ -n "${PKDUMP_KEYS_DRY_RUN:-}" ]; then
    printf '%s\n' "${RUN[@]}"
    exit 0
fi

# `|| RC=$?` rather than a bare call plus `RC=$?`: under `set -e` the bare form
# takes the whole script with it on a non-zero status, so the mode check below
# would never run — and this script's exit code has to be the job's either way.
RC=0
"${RUN[@]}" || RC=$?

# `init` and `restore` write the key inside the container, where the process is
# container-root and therefore (under rootless podman) this user. The mode is
# set by the writer; this is the assertion that it LANDED, on the host, where
# it actually matters. "the code sets 600" and "the file is 600" are different
# claims and only the second one protects anything.
case "$SUBCOMMAND" in
init | restore)
    if [ "$RC" -eq 0 ] && [ -e "$KEY_FILE" ]; then
        MODE="$(stat -c '%a' "$KEY_FILE")"
        if [ "$MODE" != "600" ]; then
            echo "keys: FAILED — ${KEY_FILE} landed at mode ${MODE}, not 600." >&2
            echo "  Fix it now: chmod 600 ${KEY_FILE}" >&2
            exit 1
        fi
        echo "keys: verified ${KEY_FILE} is mode 600 on the host"
    fi
    ;;
esac

exit "$RC"
