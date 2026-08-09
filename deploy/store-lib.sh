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

# pkdump_store_adopt_instance <instance> — recover the store an instance was
# created in from its installed Quadlet unit, so a bare
# `deploy/teardown.sh <instance>` removes from the SAME store setup created in
# instead of looking in the default one and leaving the image and volume behind.
#
# An explicit PKDUMP_STORE_ROOT wins; the unit is only consulted when the caller
# said nothing.
pkdump_store_adopt_instance() {
    [ -n "${PKDUMP_STORE_ROOT:-}" ] && return 0
    local unit graph
    unit="${HOME}/.config/containers/systemd/pkdump-${1}.container"
    [ -f "$unit" ] || return 0
    graph="$(sed -n 's/^GlobalArgs=.*--root=\([^ ]*\).*/\1/p' "$unit" | head -n1)"
    [ -n "$graph" ] || return 0
    PKDUMP_STORE_ROOT="${graph%/storage}"
}

# pkdump_store_stamp_unit <quadlet file> — teach a generated Quadlet unit to use
# the active store. A no-op when no store is active, which is what keeps prod's
# generated unit byte-identical to the pre-pd-fite one.
pkdump_store_stamp_unit() {
    [ -n "${PKDUMP_STORE_GLOBAL_ARGS:-}" ] || return 0
    sed -i "/^\[Container\]\$/a GlobalArgs=${PKDUMP_STORE_GLOBAL_ARGS}" "$1"
}
