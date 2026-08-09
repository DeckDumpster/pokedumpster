#!/usr/bin/env bash
#
# Where a PokeDumpster instance's container storage lives (pd-fite).
#
# Rootless Podman keeps images, layers and volumes under $HOME. On the box that
# runs prod, $HOME is on the 98G LVM root that prod itself runs from — so every
# throwaway --test instance, every CI image build and every dangling layer is
# written to the disk prod depends on, while the 938G data disk holding the
# checkouts stays nearly empty. At 100% full a cargo link died with
# `ld terminated with signal 7 [Bus error]`, which reads as a toolchain bug and
# not a disk problem.
#
# The fix is NOT to move Podman's store wholesale — that would relocate prod's
# volumes as a side effect, which is a durability decision nobody asked for.
# Instead this is an OPT-IN alternate store for non-prod instances:
#
#   PKDUMP_STORE_ROOT=<dir>   put images, layers and volumes under <dir>
#   PKDUMP_STORE_ROOT=        (unset/empty) use Podman's default store
#
# Prod never opts in, so prod's behaviour is untouched by construction rather
# than by a conditional that has to get prod right. With the variable empty,
# every function here is a no-op and the generated Quadlet units come out
# byte-identical to what they were before this file existed.
#
# WHERE THE VALUE COMES FROM
#
# Which disk is worth using is a fact about one machine, so it is not in the
# repo: pkdump_store_load_config reads it from ~/.config/pkdump/store.env, the
# same host-config directory alerts.env and litestream.env already live in. It
# is NOT inferred from the box's disk layout — an earlier version derived the
# store from "is the checkout on a different filesystem from $HOME", which
# encoded one machine's topology as a rule and would silently invent a store
# directory at the top of an external drive or a network mount on any other
# box (pd-rf7c).
#
# HOW IT IS APPLIED
#
# Two consumers have to agree, or you get an instance whose image lives in one
# store and whose container looks for it in another:
#
#   1. Shell scripts. pkdump_store_activate installs a `podman` shim on PATH
#      that adds --root/--runroot to every invocation. A shim rather than an
#      argument threaded through each call site because deploy/ci.sh's subtree
#      (setup.sh, teardown.sh, tests/litestream/*.sh, tests/visual/run.sh) has
#      dozens of podman calls and missing ONE fails silently, in the exact way
#      described above. PATH is inherited, so children agree for free.
#   2. Quadlet units. systemd does not inherit our PATH, so the generated unit
#      carries the same flags in a GlobalArgs= key — pkdump_store_stamp_unit
#      writes it. Podman 4.9's Quadlet applies GlobalArgs to ExecStart, ExecStop
#      and ExecStopPost alike, so start and stop use one store.
#
# THE UNIT IS THE RECORD, IN BOTH DIRECTIONS
#
# Because PATH is inherited, a script that operates on an EXISTING instance
# cannot just take the ambient store: it has to ask the instance where it lives.
# pkdump_store_adopt_instance does that, and it has to be able to answer "the
# default store" as positively as it answers "that one over there" — an unstamped
# unit is a statement, not a missing value. It used to be add-only (`return 0`
# when a store was already active), so an instance created BEFORE any of this
# existed, adopted inside a shell that had activated an alternate store, kept the
# parent's store: teardown's `podman rmi` / `podman volume rm` were aimed at a
# store the instance was never in, no-op'd through their `2>/dev/null || true`,
# and then teardown deleted the unit — the only record of where the image and
# volume actually were. deploy.sh had the mirror-image symptom: it built and
# tagged into the alternate store while the unstamped unit kept systemd on the
# default one, so the restart succeeded and went on serving the old image
# (pd-9rxf). Hence pkdump_store_deactivate: adopting an unstamped unit drops the
# shim from PATH and clears the flags, rather than falling through.
#
# TMPDIR moves too: Buildah puts `--mount=type=cache` contents (the Containerfile
# caches the cargo registry and target/ that way) under $TMPDIR/buildah-cache-$UID,
# which is /var/tmp — 6.7G of it on the prod disk when this was written. It is
# pointed inside the store root, which also keeps image-pull staging on the same
# filesystem as the store it is being moved into.
#
# NOT COVERED, deliberately: pkdump-refresh@.service and the backup-check timers
# are a single %i template shared by every instance, so they cannot carry
# per-instance store flags. An instance in an alternate store is a throwaway —
# do not enable those timers for it.
#
# ############################################################################
# # NEVER RUN `podman system reset` — IT IS NOT SCOPED BY --root/--runroot.  #
# ############################################################################
#
# Everything above teaches operators and scripts to pass --root/--runroot at a
# second store. `system reset` is the one subcommand that ignores them, and it
# sits one keystroke away from the store you actually want to delete. It says so
# itself: `podman system reset --help` is "Reset podman storage back to default
# state" — default, not "the state of the store you named".
#
# Measured on podman 4.9.3, 2026-08-08 (pd-rkrf). Cleaning up a THROWAWAY probe
# store with
#
#     podman --root=<probe>/storage --runroot=<probe-runroot> system reset --force
#
# also wiped user-global rootless state that no flag pointed at:
#
#   * /run/user/$UID/libpod and the rootless SHM lock — podman's per-user
#     runtime state, shared by EVERY store on the box, prod's included.
#   * $TMPDIR/buildah-cache-$UID at the AMBIENT TMPDIR (/var/tmp, 6.7G), not the
#     one this file points into the store root.
#
# RESULT: pkdump-prod went down — HTTP 000 on 8090, podman answering "container
# state improper" while conmon and `pkdump serve` were still alive. Other
# instances were left in podman state `Created` with live conmon: still serving,
# but unmanageable until restarted. Data survived (the volumes are not what got
# destroyed) and the Litestream sidecar never stopped replicating.
#
# If someone has already run it: the damage is to runtime state, so restart the
# units — `systemctl --user restart pkdump-<instance>` — one per affected
# instance, and check `podman ps` for anything still stuck in `Created`.
#
# REMOVING A STORE, correctly. Stop and remove what the store owns, from inside
# that store, then delete its directories. Every command below IS scoped by the
# flags, so run them with the shim on PATH (pkdump_store_activate) or pass
# $PKDUMP_STORE_GLOBAL_ARGS explicitly:
#
#     podman stop -a                      # or teardown.sh per instance first
#     podman rm -af
#     podman volume rm -af                # the store's volumes, nobody else's
#     podman rmi -af
#     podman network prune -f
#     rm -rf "$PKDUMP_STORE_ROOT" "${PKDUMP_STORE_GLOBAL_ARGS##*--runroot=}"
#
# The store root is storage/ + tmp/ (the buildah cache) + bin/ (the shim); the
# runroot is read back off the flags rather than globbed, so it is the one
# derived for THIS graph root and not another store's. `rm -rf` on the two
# directories is the part `system reset` was being reached for, and it is scoped
# by construction: a path deletes exactly the path. The shim is inside the store
# root, so a shell that ran this has a PATH entry pointing at nothing — start a
# new one.
#
# Any store-teardown command added to deploy/ must use this recipe. That is not
# left to memory — tests/deploy/run.sh §6 greps deploy/ and tests/ and fails on
# a `podman system reset` anywhere in the repo.
#
# Sourced, not executed.

# pkdump_store_load_config — take PKDUMP_STORE_ROOT from host config when the
# caller has not set it. Called by the scripts that MAY use an alternate store
# (deploy/ci.sh); never by the ones prod runs, so prod cannot pick one up from a
# file it did not ask about.
#
# Precedence, and the distinction is load-bearing:
#
#   PKDUMP_STORE_ROOT set (even to empty)   the caller decided — file ignored
#   PKDUMP_STORE_ROOT unset                 read ~/.config/pkdump/store.env
#   neither                                 Podman's default store
#
# An explicit empty value has to win, because that is how a one-off run opts
# back out on a box whose store.env opts in.
pkdump_store_load_config() {
    [ -z "${PKDUMP_STORE_ROOT+set}" ] || return 0
    local conf="${HOME}/.config/pkdump/store.env"
    [ -f "$conf" ] || return 0
    # Same shape as alerts.env and litestream.env: a dotenv file, sourced.
    set -a
    # shellcheck disable=SC1090
    . "$conf"
    set +a
}

# pkdump_store_activate — point every podman invocation in this process tree at
# $PKDUMP_STORE_ROOT. Idempotent; a no-op when the variable is unset or empty.
#
# Exports PKDUMP_STORE_GLOBAL_ARGS (the flags, for pkdump_store_stamp_unit) and
# PKDUMP_STORE_ROOT itself, so a child script that sources this file again sees
# the same store instead of re-deriving one.
pkdump_store_activate() {
    export PKDUMP_STORE_GLOBAL_ARGS="${PKDUMP_STORE_GLOBAL_ARGS:-}"
    [ -n "${PKDUMP_STORE_ROOT:-}" ] || return 0

    local root graph runroot bin real
    root="$PKDUMP_STORE_ROOT"
    bin="${root}/bin"

    # Already activated in an ancestor: the shim is on PATH and the flags are in
    # the environment. Re-shimming here would resolve `podman` to the shim and
    # build one that exec's itself.
    case ":${PATH}:" in
        *":${bin}:"*) return 0 ;;
    esac

    real="$(command -v podman)" || {
        echo "ERROR: PKDUMP_STORE_ROOT is set but podman is not on PATH" >&2
        return 1
    }

    graph="${root}/storage"
    # Remembered so pkdump_store_deactivate can put it back; see there.
    export PKDUMP_STORE_PREV_TMPDIR="${TMPDIR-}"
    # The runroot holds volatile per-boot state, including the layer store's
    # mountpoints.json — a file each store rewrites wholesale. Two stores sharing
    # one would drop each other's mount records, and prod is one of those stores.
    # It has to be a local filesystem, so it goes in the runtime dir, keyed by the
    # store it belongs to.
    runroot="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pkdump-store-$(printf '%s' "$graph" | sha1sum | cut -c1-8)"
    mkdir -p "$graph" "$runroot" "${root}/tmp" "$bin"

    export PKDUMP_STORE_GLOBAL_ARGS="--root=${graph} --runroot=${runroot}"

    cat > "${bin}/podman" <<EOF
#!/usr/bin/env bash
# Generated by deploy/store-lib.sh — do not edit. Sends every podman call in
# this process tree to the non-prod store under ${root} (pd-fite).
exec ${real} ${PKDUMP_STORE_GLOBAL_ARGS} "\$@"
EOF
    chmod +x "${bin}/podman"

    export PATH="${bin}:${PATH}"
    export TMPDIR="${root}/tmp"
    # stderr: some callers capture a script's stdout and parse it (drill.sh reads
    # backup-check.sh's output), and this is a progress note, not a result.
    echo "==> Container storage: ${graph} (non-prod store; prod's is untouched)" >&2
}

# pkdump_store_is_activated — is the store named by PKDUMP_STORE_ROOT the one
# this process tree's `podman` actually resolves to? The shim on PATH is the
# marker: pkdump_store_activate always puts it there.
#
# This is what separates a store the caller CHOSE for this call from one merely
# inherited from a parent that activated it, and the two are not the same claim.
# `PKDUMP_STORE_ROOT=/x bash deploy/teardown.sh foo` is a decision about foo —
# the escape hatch for a unit that is missing or wrong — and the variable is set
# with no shim on PATH. deploy/ci.sh activating a store and then invoking
# teardown.sh for some OTHER instance says nothing about that instance, and the
# variable arrives WITH the shim.
pkdump_store_is_activated() {
    [ -n "${PKDUMP_STORE_ROOT:-}" ] || return 1
    case ":${PATH}:" in
        *":${PKDUMP_STORE_ROOT}/bin:"*) return 0 ;;
    esac
    return 1
}

# pkdump_store_deactivate — put this process tree back on Podman's default
# store: drop the shim from PATH, restore TMPDIR, clear the flags. The inverse
# of pkdump_store_activate, and the reason adopt can select the default store
# positively instead of by omission.
#
# PATH and TMPDIR are touched only when a store is genuinely active, so on prod
# — which never opts in — this is nothing but two empty exports. Clearing
# PKDUMP_STORE_GLOBAL_ARGS is what stops pkdump_store_stamp_unit from writing a
# store into a unit that must not carry one; exporting PKDUMP_STORE_ROOT empty
# (rather than unsetting it) says "the default store, decided" in the vocabulary
# pkdump_store_load_config already reads.
pkdump_store_deactivate() {
    if pkdump_store_is_activated; then
        local bin="${PKDUMP_STORE_ROOT}/bin"
        PATH="${PATH//":${bin}:"/:}"
        PATH="${PATH#"${bin}:"}"
        PATH="${PATH%":${bin}"}"
        export PATH
        TMPDIR="${PKDUMP_STORE_PREV_TMPDIR-}"
        [ -n "$TMPDIR" ] || unset TMPDIR
    fi
    export PKDUMP_STORE_ROOT=""
    export PKDUMP_STORE_GLOBAL_ARGS=""
}

# pkdump_store_adopt_instance <instance> — put this shell on the store the
# instance actually lives in, read from its installed Quadlet unit, so that
# `deploy/teardown.sh <instance>` removes from the SAME store setup created it
# in and `deploy/deploy.sh <instance>` builds into the one systemd will look in.
#
# The unit is authoritative in both directions (pd-9rxf): GlobalArgs names a
# store, and no GlobalArgs names the default store just as definitely. What it
# does not do is invent a store — with no unit on disk there is no record to
# read (the instance may not exist yet; deploy.sh delegates to setup.sh), so the
# caller's choice stands.
#
# An explicit PKDUMP_STORE_ROOT still beats the unit; an inherited activation
# does not. See pkdump_store_is_activated for why those are different.
pkdump_store_adopt_instance() {
    local unit graph
    unit="${HOME}/.config/containers/systemd/pkdump-${1}.container"
    [ -f "$unit" ] || return 0

    if [ -n "${PKDUMP_STORE_ROOT:-}" ] && ! pkdump_store_is_activated; then
        return 0
    fi

    graph="$(sed -n 's/^GlobalArgs=.*--root=\([^ ]*\).*/\1/p' "$unit" | head -n1)"
    if [ -z "$graph" ]; then
        pkdump_store_deactivate
        return 0
    fi

    graph="${graph%/storage}"
    if [ "${PKDUMP_STORE_ROOT:-}" != "$graph" ]; then
        # Leave the store we are on before naming another, so PATH carries one
        # shim and TMPDIR belongs to the store it points at.
        pkdump_store_deactivate
        PKDUMP_STORE_ROOT="$graph"
    fi
}

# pkdump_store_stamp_unit <quadlet file> — teach a generated Quadlet unit to use
# the active store. A no-op when no store is active, which is what keeps prod's
# generated unit byte-identical to the pre-pd-fite one.
pkdump_store_stamp_unit() {
    [ -n "${PKDUMP_STORE_GLOBAL_ARGS:-}" ] || return 0
    sed -i "/^\[Container\]\$/a GlobalArgs=${PKDUMP_STORE_GLOBAL_ARGS}" "$1"
}
