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
# That command now exists: pkdump_store_teardown runs exactly the recipe above,
# and deploy/store-teardown.sh is the CLI over it. Two details it had to add,
# both measured rather than reasoned — see the function (pd-yfev).
#
# WHAT A SECOND STORE COSTS
#
# Podman 4.9 does not fully support two rootless stores on one login: they share
# one rootless-netns scaffolding directory, and whichever store cleans up last
# takes it from the other — leaving that store unable to start ANY container on
# a user-defined network until its netns file is dropped. pkdump_store_netns_repair
# handles it at activation; the mechanism is written up there (pd-yfev).
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

    pkdump_store_netns_repair
}

# pkdump_store_netns_name — the name Podman gives THIS store's rootless network
# namespace. Podman derives it from the libpod static dir, which is <graph>/libpod:
#
#   libpod/networking_linux.go
#     hash := sha256.Sum256([]byte(r.config.Engine.StaticDir))
#     netnsName := fmt.Sprintf("%s-%x", rootlessNetNsName, hash[:10])
#
# Reproducing a hash from another project's internals is not something to do
# lightly. It is done here because it is what makes the repair below SAFE: with
# the name computed from this store's own graph root, prod's name is never even
# derived, so no code path can remove it. The alternative — reaping every
# `rootless-netns-*` in the runtime dir — would have to reason about prod's.
#
# Verified byte-exact against two live stores on podman 4.9.3. If a future
# podman changes the scheme this stops matching, the repair silently finds
# nothing, and the failure mode is the status quo ante (pd-yfev).
pkdump_store_netns_name() {
    printf 'rootless-netns-%s' \
        "$(printf '%s' "${1}/libpod" | sha256sum | cut -c1-20)"
}

# pkdump_store_netns_repair — un-wedge this store's rootless networking.
#
# THE BUG (pd-yfev). Two rootless stores cannot both keep a network namespace,
# and podman 4.9 does not notice:
#
#   * Each store gets its OWN netns file, named from the hash above, under
#     $XDG_RUNTIME_DIR/netns/.
#   * They SHARE one scaffolding directory, $XDG_RUNTIME_DIR/libpod/tmp/rootless-netns.
#     That path comes from Engine.TmpDir, which --root and --runroot do not move
#     (neither does --tmpdir — measured on 4.9.3).
#   * The scaffolding is created only on the branch that CREATES the netns.
#   * RootlessNetNS.Cleanup() does os.RemoveAll on the SHARED directory when the
#     last bridge-network container *in its own store* exits — it counts
#     containers out of its own store's database and cannot see the other one's.
#
# So the moment one store's last container on a user-defined network goes away,
# every OTHER store is left holding a netns file that still looks valid. Podman
# takes it, skips the create branch, and then fails to mount into scaffolding
# that is no longer there:
#
#   Error: failed to mount runtime directory for rootless netns: no such file or directory
#
# That store can never start a container on a user-defined network again, and
# tests/litestream/{run,drill}.sh both create one — so deploy/ci.sh cannot pass.
# It wedges silently, mid-session, and nothing about the message says "store".
#
# THE REPAIR is to delete this store's stale netns file, which puts podman back
# on the create branch. It is not a mountpoint in our mount namespace (the mount
# lives in the pause process's), so removing the name is all it takes.
#
# It is deliberately NOT `podman system migrate`, which is the repair that was
# found by hand first: migrate kills the pause process, and that process is
# per-USER, not per-store — shared with the default store prod runs in. A
# non-prod gate must not reach into prod's runtime state to fix itself.
#
# The guard is what keeps this from being a live-namespace killer: if the shared
# scaffolding is present, some store is using it, and nothing is removed.
pkdump_store_netns_repair() {
    [ -n "${PKDUMP_STORE_ROOT:-}" ] || return 0

    local rundir netns_file
    rundir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    netns_file="${rundir}/netns/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"

    # Nothing of ours to reap.
    [ -e "$netns_file" ] || return 0
    # Scaffolding intact — the netns is usable, and may be in use right now.
    [ -d "${rundir}/libpod/tmp/rootless-netns/run/user/$(id -u)" ] && return 0

    echo "==> Rootless netns for this store is stale (another store's cleanup removed" >&2
    echo "    the shared scaffolding). Dropping ${netns_file##*/} so podman rebuilds it (pd-yfev)." >&2
    rm -f "$netns_file"
}

# pkdump_store_teardown — remove the active store: every container and image in
# it, the store root (graph, Buildah TMPDIR and the shim), its runroot, and its
# netns name. The store-level lifecycle command that did not exist —
# deploy/teardown.sh removes an INSTANCE and deliberately leaves the store it
# lived in alone, so a box accumulated stores nothing ever collected (pd-yfev).
#
# This is the "REMOVING A STORE, correctly" recipe in this file's header, made
# executable: stop and remove what the store owns from INSIDE that store, then
# delete its directories. Never `podman system reset`, which ignores
# --root/--runroot and took prod down when it was aimed at a throwaway store
# (pd-rkrf) — a path deletes exactly the path, however podman resolves things.
#
# It must not run without a store either. With PKDUMP_STORE_ROOT empty the
# target WOULD be Podman's default store, which is prod's, so that case refuses
# instead of defaulting.
#
# `podman unshare rm -rf` rather than plain rm: a rootless store's layer
# directories are owned by subuids, and rm fails on every one of them with
# EPERM. unshare enters the user namespace where those uids are ours.
pkdump_store_teardown() {
    if [ -z "${PKDUMP_STORE_ROOT:-}" ]; then
        echo "ERROR: pkdump_store_teardown needs PKDUMP_STORE_ROOT; refusing to" >&2
        echo "       act on Podman's default store (prod's)." >&2
        return 1
    fi

    # Not optional. Every podman call below is a bare `podman`, which without the
    # shim means Podman's DEFAULT store — so an un-activated teardown would
    # `rm -f -a` prod's containers. Activation is idempotent.
    pkdump_store_activate || return 1

    local root graph rundir netns_file
    root="$PKDUMP_STORE_ROOT"
    graph="${root}/storage"
    rundir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    netns_file="${rundir}/netns/$(pkdump_store_netns_name "$graph")"

    echo "==> Removing container store ${root}" >&2

    # The recipe, in order, every command scoped by the shim's flags. Best-effort
    # throughout: a store whose database is already gone has nothing to stop, and
    # the directory removal below is what actually frees the disk.
    podman stop -a >/dev/null 2>&1 || true
    podman rm -af >/dev/null 2>&1 || true
    podman volume rm -af >/dev/null 2>&1 || true
    podman rmi -af >/dev/null 2>&1 || true
    podman network prune -f >/dev/null 2>&1 || true
    # The header's recipe ends in `rm -rf` on the store root and its runroot.
    # Two details it does not mention, both measured rather than reasoned:
    #
    #   * `podman rm -af` returns before the container's rootfs is unmounted, and
    #     one leftover mount fails the whole removal with EBUSY on
    #     storage/overlay. It survived three retries a second apart. So the
    #     unmount and the removal happen inside ONE `podman unshare` — the mount
    #     is in that namespace, and a second invocation is a second namespace
    #     that cannot see it.
    #   * It cannot be the last word. This `podman` IS the doomed store's shim,
    #     so podman re-creates that store's skeleton as it shuts down — after
    #     the command inside it has exited. storage.lock, overlay/ and
    #     overlay-layers/ come back every time. What comes back is empty and
    #     ours (not subuid-owned), so a plain rm finishes the job.
    #
    # bin/ (the shim this call is running through) goes with it: podman has
    # already exec'd, so the script being deleted underneath it is harmless.
    podman unshare sh -c '
        root=$1
        while read -r _ _ _ _ mp _; do
            case "$mp" in "$root"|"$root"/*) printf "%s\n" "$mp" ;; esac
        done < /proc/self/mountinfo | sort -r | while read -r mp; do
            umount -l "$mp" 2>/dev/null || true
        done
        rm -rf "$root"
    ' sh "$root" >/dev/null 2>&1 || true
    rm -rf "$root" 2>/dev/null || true

    # The volatile state goes either way — it is this store's alone (the runroot
    # is keyed by the graph path) and is worthless without the store.
    rm -rf "${rundir}/pkdump-store-$(printf '%s' "$graph" | sha1sum | cut -c1-8)"
    rm -f "$netns_file"

    # Say what is actually true. A teardown that reports success over a store it
    # could not remove is worse than one that fails: the disk it was supposed to
    # free stays full and nothing says so.
    if [ -e "$root" ]; then
        echo "ERROR: ${root} is still on disk — something in it is still mounted." >&2
        echo "       Stop anything using this store and run this again." >&2
        return 1
    fi
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
