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
# do not enable those timers for it. Being one file per box is also why a
# throwaway's setup.sh does not get to REWRITE them: see the "HOST-WIDE UNITS"
# header in deploy/units-lib.sh (pd-onyd).
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
# a user-defined network until its netns file is dropped. The scaffolding is now
# SPLIT so that cannot happen (pkdump_store_containers_conf, pd-3zjt), and
# pkdump_store_netns_repair remains as the recovery for a store that predates the
# split; the mechanism is written up at both (pd-yfev, pd-3zjt).
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
    pkdump_store_containers_conf "$root" "$runroot"

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

# pkdump_store_libpod_tmp_dir <runroot> — where THIS store keeps its libpod tmp
# state, and therefore its rootless-netns scaffolding. Inside the store's own
# runroot, which is already keyed by the graph path, so no two stores can name
# the same one.
pkdump_store_libpod_tmp_dir() {
    printf '%s/libpod-tmp' "$1"
}

# pkdump_store_containers_conf — give this store its OWN rootless-netns
# scaffolding directory, so its cleanup cannot take prod's (pd-3zjt).
#
# THE BUG THIS CLOSES is pd-yfev's, pointed the other way. That one was written
# up as a cost of running a second store ("whichever store cleans up last takes
# the scaffolding from the other") and repaired only on the side that opts in —
# pkdump_store_netns_repair returns immediately when PKDUMP_STORE_ROOT is empty,
# which prod always is. So the damage in the direction that matters most was
# never addressed at all: a CI gate's last bridge-network container exits,
# podman RemoveAll's the SHARED directory, and PROD is left holding a netns file
# that still looks valid. Every prod container on a user-defined network then
# dies at start with
#
#   Error: failed to mount runtime directory for rootless netns: no such file or directory
#
# which is what killed pkdump-value-snapshots@prod every night from 2026-08-12.
# Only jobs on a user-defined network are affected, so pkdump-refresh@ stayed
# green throughout and nothing else on the box reported anything.
#
# THE SPLIT. The shared path is $Engine.TmpDir/rootless-netns. --root, --runroot
# and --tmpdir all leave Engine.TmpDir alone (--tmpdir re-measured on 4.9.3 for
# this bead: no effect), but containers.conf's `[engine] tmp_dir` moves it, and
# CONTAINERS_CONF_OVERRIDE is the merge-on-top spelling that does not discard
# whatever containers.conf the box already has. With it set, the alternate
# store's scaffolding, slirp pid and resolv.conf live under its own runroot and
# its cleanup can only ever remove its own.
#
# THE ONE CAVEAT, measured: podman records tmp_dir in the store's libpod
# database when the store is CREATED and then pins it —
#
#   level=debug msg="Overriding tmp dir \"…\" with \"/run/user/1000/libpod/tmp\" from database"
#
# — so a store that already existed before this landed keeps sharing prod's
# scaffolding no matter what this writes. That is not silently tolerated: it is
# what deploy/store-teardown.sh is for, and the README says to run it once.
#
# THE SAME PIN IS WHAT MAKES THE SPLIT HOLD FOR CALLERS THAT NEVER SEE THIS
# VARIABLE, which is most of them. CONTAINERS_CONF_OVERRIDE is environment, and a
# Quadlet unit inherits none of this shell's: systemd starts `podman run` with
# the unit's own environment and the `GlobalArgs=` line pkdump_store_stamp_unit
# wrote, nothing else. deploy/pkdump-nessie.container is on a user-defined
# network, so a non-prod instance in an alternate store runs a long-lived bridge
# container this variable can never reach — and if that container's cleanup used
# the shared directory, the split would be decorative for the case it was bought
# for. It does not: podman reads the pinned tmp dir back out of the store's
# database, so the split is a property of the STORE, not of the caller.
# Measured, and asserted by tests/store/netns_split.sh §4.
#
# Prod never opts into a store, so prod is never given a containers.conf and its
# tmp dir stays exactly where podman puts it. The isolation is entirely on the
# non-prod side, which is the side that was doing the damage.
pkdump_store_containers_conf() {
    local root="$1" runroot="$2" conf tmp_dir
    conf="${root}/containers.conf"
    tmp_dir="$(pkdump_store_libpod_tmp_dir "$runroot")"
    mkdir -p "$tmp_dir"

    cat > "$conf" <<EOF
# Generated by deploy/store-lib.sh — do not edit. Keeps this store's
# rootless-netns scaffolding out of the one every other store shares, so its
# cleanup cannot leave prod unable to start a container on a user-defined
# network (pd-3zjt).
[engine]
tmp_dir = "${tmp_dir}"
EOF

    # Remembered so pkdump_store_deactivate can put it back, on the same footing
    # as TMPDIR above.
    export PKDUMP_STORE_PREV_CONTAINERS_CONF_OVERRIDE="${CONTAINERS_CONF_OVERRIDE-}"
    export CONTAINERS_CONF_OVERRIDE="$conf"
}

# pkdump_store_split_check — did the store ACTUALLY take the split, or is this a
# store created before it whose libpod database still pins the shared tmp dir?
#
# The caveat above is a one-time operator action, and an operator action that
# nothing checks is one nobody knows is outstanding: a box pulls this change, the
# generated containers.conf says exactly the right thing, every gate passes, and
# the store goes on sharing prod's scaffolding exactly as before. There is no
# symptom until the night a cleanup lands between prod and its network namespace.
#
# So it is asked rather than assumed, and podman is the only one who can answer —
# the pin lives in the store's database, not in anything on disk this could stat.
# `--log-level=debug` is the only place podman says which tmp dir it settled on.
# One `podman info` per CI run, which is nothing beside what follows it.
#
# It WARNS, and does not fail the run. The store works; the gates pass; what is
# at risk is another store's namespace, and since pd-3zjt that risk is bounded at
# the other end too — pkdump_store_netns_ensure detects and repairs a wedged
# namespace at the start of the two jobs that need one. Failing here would block
# a box's whole suite on a cleanup, and the suite is how the fix gets exercised.
# What the warning must do instead is be actionable, so it names the command and
# what running it costs.
pkdump_store_split_check() {
    [ -n "${PKDUMP_STORE_ROOT:-}" ] || return 0

    local want actual
    want="$(pkdump_store_libpod_tmp_dir "${PKDUMP_STORE_GLOBAL_ARGS##*--runroot=}")"
    actual="$(podman --log-level=debug info 2>&1 |
        sed -n 's/.*Using tmp dir \([^"]*\)".*/\1/p' | tail -1)"

    # Podman could not be asked — no debug line, a version that words it
    # differently. Silence is right: a warning about a store that may be
    # perfectly fine is how a real one gets ignored.
    [ -n "$actual" ] || return 0
    [ "$actual" = "$want" ] && return 0

    echo "" >&2
    echo "!! This container store PREDATES the rootless-netns split (pd-3zjt)." >&2
    echo "   Podman pins tmp_dir in a store's database when the store is created," >&2
    echo "   so this one still shares prod's scaffolding:" >&2
    echo "     using:  ${actual}" >&2
    echo "     wanted: ${want}" >&2
    echo "   Its cleanup can still leave prod unable to start a container on a" >&2
    echo "   user-defined network. Tear the store down ONCE to take the split:" >&2
    echo "     bash deploy/store-teardown.sh" >&2
    echo "   Costs one cold image rebuild on the next run. Nothing else." >&2
    echo "" >&2
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
#     (neither does --tmpdir — measured on 4.9.3). It is what
#     pkdump_store_containers_conf now moves, for every store created after
#     pd-3zjt; this function is the recovery for the ones that predate it, and
#     for anything else that leaves a netns file behind without its scaffolding.
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
# The guard is what keeps this from being a live-namespace killer: if the
# scaffolding is present, some store is using it, and nothing is removed.
#
# WHICH scaffolding it looks at is the one thing pd-3zjt changed. A store created
# under pkdump_store_containers_conf has its own, and that one is authoritative
# for it; a store that predates the split has none of its own and is still on the
# shared directory, which is then the only thing worth asking about. Checking the
# store's own directory ONLY would read a pre-split store as permanently stale
# and drop a netns that might be in use by a gate running right now; checking the
# shared one only would refuse to repair a post-split store for as long as prod
# happened to be running. So the question asked is "does this store have a tmp
# dir of its own yet", and the answer picks which directory is the evidence.
pkdump_store_netns_repair() {
    [ -n "${PKDUMP_STORE_ROOT:-}" ] || return 0

    local rundir netns_file store_tmp scaffold
    rundir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    netns_file="${rundir}/netns/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"

    # Nothing of ours to reap.
    [ -e "$netns_file" ] || return 0

    store_tmp="$(pkdump_store_libpod_tmp_dir "${PKDUMP_STORE_GLOBAL_ARGS##*--runroot=}")"
    # `alive` is podman's own marker that it has used this tmp dir; the directory
    # alone proves nothing, since pkdump_store_containers_conf mkdir -p's it.
    if [ -e "${store_tmp}/alive" ]; then
        scaffold="${store_tmp}/rootless-netns"
    else
        scaffold="${rundir}/libpod/tmp/rootless-netns"
    fi

    # Scaffolding intact — the netns is usable, and may be in use right now.
    [ -d "${scaffold}/run/user/$(id -u)" ] && return 0

    echo "==> Rootless netns for this store is stale (its scaffolding under" >&2
    echo "    ${scaffold} is gone). Dropping ${netns_file##*/} so podman rebuilds it (pd-yfev)." >&2
    rm -f "$netns_file"
}

# How long a repair waits for what it restarted to answer, and how often it asks.
# 120s because pkdump-nessie is a JVM measured at 30-40s on this box and its own
# unit allows TimeoutStartSec=120, while the units that call this allow 1800 — so
# the wait cannot be what times a nightly job out. Overridable as a test seam;
# nothing in production sets either.
PKDUMP_NETNS_READY_TIMEOUT="${PKDUMP_NETNS_READY_TIMEOUT:-120}"
PKDUMP_NETNS_READY_INTERVAL="${PKDUMP_NETNS_READY_INTERVAL:-3}"

# pkdump_store_netns_ensure <network> [<ready-command>...] — make sure a
# container can actually be started on a user-defined network in the ACTIVE
# store, and say something useful when it cannot. Called by the wrappers that run
# a job on the lake network (pd-3zjt); nothing else needs it, and nothing else
# should pay for it.
#
# WHY THIS EXISTS SEPARATELY FROM pkdump_store_netns_repair. That one is a
# file-stat, runs at every activation, and answers only for a store this shell
# opted into — PROD never opts into one, so on the box where this bug actually
# bit, it was a no-op by construction. This one asks podman, about whatever store
# is active, at the moment a job is about to need the answer:
#
#   podman unshare --rootless-netns true
#
# is the whole probe. It runs the same setup a container start runs and fails
# with the same message, needs no image, no network and no container, and takes
# about a tenth of a second. Nothing about podman's internals is reproduced to
# get the answer — podman is asked.
#
# WHAT IT WILL AND WILL NOT REPAIR. Dropping the netns file puts podman back on
# the create branch, and the containers already running on the OLD namespace are
# then unreachable from anything started afterwards — measured for this bead: a
# fresh container comes up fine and cannot resolve the running one at all
# ("wget: bad address"). Restarting them completes the repair. So:
#
#   * nothing running on a user-defined network  -> drop the netns, done
#   * only containers on <network> running        -> drop it and restart THEM
#   * anything else on a user-defined network     -> REFUSE, and name it
#
# The last case is not caution for its own sake. The default store is prod's, and
# on a box where prod shares it with unrelated projects, their containers are on
# it too — a nightly job may not restart another project's service to get its own
# work done. That case is an operator's, and the refusal prints the two commands.
#
# Rebuilding the scaffolding in place instead was tried and does not work: podman
# wants live files there (resolv.conf, the slirp4netns pid), not just directories,
# and hand-made empty ones only move the failure to the next mount.
#
# <ready-command> — WHY THE REPAIR IS NOT DONE WHEN THE RESTART RETURNS (pd-p39v).
# `systemctl --user restart` returns when the CONTAINER is running, which for
# pkdump-nessie means a JVM that will not answer for another 30-40 seconds. The
# repair therefore raced its own remedy: it printed "Rootless networking
# repaired", the caller started its job immediately, and the job died on a
# connection error to a service that was coming up fine. The condition self-heals
# by the next run, so the unit paged for something already fixed — the exact
# false page this repo has paid for twice before.
#
# So a repair that RESTARTED something is only complete once that something
# ANSWERS again, and what "answers" means is the caller's to say: this file knows
# about container stores, not about Nessie. The caller passes a command that
# exits 0 when its network is usable again — for both lake jobs, an HTTP GET of
# the catalog's config endpoint from a throwaway container ON the network, which
# is the same path the job itself is about to take. It is polled, bounded, and
# a deadline that passes FAILS the repair rather than proceeding: proceeding is
# what produced the false page.
#
# The wait is paid ONLY when something was restarted. Nothing running means
# nothing is mid-start — the next container rebuilds the namespace itself — and
# the healthy path never reaches any of this at all.
#
# A caller that restarts something and offers no way to confirm it came back is
# refused, loudly, rather than allowed to return the old success: an unverifiable
# repair IS this bug, and a silent second copy of it is what a default would buy.
pkdump_store_netns_ensure() {
    local network="$1"
    shift
    local ready=("$@")
    local probe rundir graph netns_file stranded foreign restarted name deadline

    probe="$(podman unshare --rootless-netns true 2>&1)" && return 0

    # Some other failure — no rootless podman at all, a broken pause process.
    # Not ours to interpret; let the run itself report it in its own words.
    case "$probe" in
    *"rootless netns"*) ;;
    *) return 0 ;;
    esac

    echo "==> Rootless networking is wedged in this store (pd-3zjt):" >&2
    echo "    ${probe}" >&2
    echo "    Another rootless store's cleanup removed the scaffolding this store's" >&2
    echo "    network namespace was mounted into. Nothing can start on a user-defined" >&2
    echo "    network until the namespace is rebuilt." >&2

    # Everything with a network name is on a bridge and therefore on the rootless
    # netns; rootless podman's default is slirp4netns, which is not and shows
    # blank here.
    stranded="$(podman ps --format '{{.Names}} {{.Networks}}' 2>/dev/null | awk 'NF > 1')"
    foreign="$(printf '%s\n' "$stranded" | awk -v n="$network" 'NF > 1 && $2 != n {print $1}')"

    if [ -n "$foreign" ]; then
        echo "  REFUSING to rebuild the namespace — containers that are not ours are running" >&2
        echo "  on it and would be cut off from everything started afterwards:" >&2
        printf '%s\n' "$foreign" | while read -r name; do echo "    ${name}" >&2; done
        echo "  This one is an operator's call. To repair by hand:" >&2
        echo "    rm -f \$XDG_RUNTIME_DIR/netns/rootless-netns-*   # the one for this store" >&2
        echo "    systemctl --user restart <each container above>" >&2
        return 1
    fi

    rundir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    graph="$(podman info --format '{{.Store.GraphRoot}}' 2>/dev/null)"
    if [ -z "$graph" ]; then
        echo "  Could not ask podman where this store's graph root is; not guessing." >&2
        return 1
    fi
    netns_file="${rundir}/netns/$(pkdump_store_netns_name "$graph")"

    echo "    Dropping ${netns_file##*/} so podman rebuilds it." >&2
    rm -f "$netns_file"

    if ! probe="$(podman unshare --rootless-netns true 2>&1)"; then
        echo "  The namespace did not come back: ${probe}" >&2
        return 1
    fi

    # Nothing was on the old namespace, so nothing is mid-start: the caller's own
    # container rebuilds the namespace when it starts, and there is nothing to
    # wait for. This is the quiet-box case and it stays as cheap as it was.
    restarted="$(printf '%s\n' "$stranded" | awk 'NF > 1 {print $1}')"
    if [ -z "$restarted" ]; then
        echo "    Rootless networking repaired; nothing was on the old namespace." >&2
        return 0
    fi

    # Only ours were on it, so restarting them is in scope and finishes the job.
    # A Quadlet container is named by its unit (ContainerName=), so the unit is
    # tried first — restarting the container behind systemd's back leaves the
    # unit's idea of it wrong.
    printf '%s\n' "$restarted" | while read -r name; do
        echo "    Restarting ${name}, which was left on the old namespace." >&2
        systemctl --user restart "${name}.service" 2>/dev/null ||
            podman restart "$name" >/dev/null 2>&1 ||
            echo "    WARNING: could not restart ${name}; this job may not reach it." >&2
    done

    # The restart has RETURNED, which is not the same as the service ANSWERING —
    # see the header. Without a way to tell the two apart this function would go
    # back to reporting a repair it has not finished, so it refuses instead.
    if [ "${#ready[@]}" -eq 0 ]; then
        echo "  Restarted the containers above, but the caller gave no way to confirm" >&2
        echo "  they answer again, so this repair cannot be reported COMPLETE (pd-p39v)." >&2
        echo "  Pass a readiness command: pkdump_store_netns_ensure <network> <cmd...>" >&2
        return 1
    fi

    echo "    Waiting up to ${PKDUMP_NETNS_READY_TIMEOUT}s for them to answer again — a restart" >&2
    echo "    returns when the container is RUNNING, and a JVM is not answering yet." >&2
    deadline=$((SECONDS + PKDUMP_NETNS_READY_TIMEOUT))
    while :; do
        if "${ready[@]}"; then
            echo "    Rootless networking repaired, and what was restarted answers again." >&2
            return 0
        fi
        [ "$SECONDS" -lt "$deadline" ] || break
        sleep "$PKDUMP_NETNS_READY_INTERVAL"
    done

    echo "  The namespace was rebuilt, but what was restarted never answered within" >&2
    echo "  ${PKDUMP_NETNS_READY_TIMEOUT}s. FAILING rather than starting a job that would die on it:" >&2
    printf '%s\n' "$restarted" | while read -r name; do echo "    ${name}" >&2; done
    echo "  Check it: systemctl --user status <name>; podman logs <name>" >&2
    return 1
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

    # The containers.conf went with the store root, and podman REFUSES TO RUN AT
    # ALL while CONTAINERS_CONF_OVERRIDE names a file that is not there —
    #
    #   level=error msg="CONTAINERS_CONF_OVERRIDE file: stat …: no such file or directory"
    #
    # exit 1, measured on 4.9.3, for `podman info` against the DEFAULT store. So
    # this shell would come out of a teardown unable to use podman for anything,
    # and the message names a path inside a store that no longer exists. The shim
    # and TMPDIR are deliberately left as they are (the README says to start a new
    # shell, and a missing shim only means the default store); this one is
    # restored because it breaks every store, including the one that is still
    # there (pd-3zjt).
    CONTAINERS_CONF_OVERRIDE="${PKDUMP_STORE_PREV_CONTAINERS_CONF_OVERRIDE-}"
    [ -n "$CONTAINERS_CONF_OVERRIDE" ] || unset CONTAINERS_CONF_OVERRIDE

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
        # The store's containers.conf goes with it, or the default store would
        # keep being told to put its libpod tmp state in a store that this shell
        # has just stopped using (pd-3zjt).
        CONTAINERS_CONF_OVERRIDE="${PKDUMP_STORE_PREV_CONTAINERS_CONF_OVERRIDE-}"
        [ -n "$CONTAINERS_CONF_OVERRIDE" ] || unset CONTAINERS_CONF_OVERRIDE
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
