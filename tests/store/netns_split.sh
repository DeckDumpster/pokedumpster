#!/usr/bin/env bash
# The rootless-netns split, measured against real podman (pd-3zjt).
#
# Two rootless stores on one login share ONE scaffolding directory,
# $Engine.TmpDir/rootless-netns, and whichever cleans up last removes it from
# under the other — leaving that store holding a netns file that still looks
# valid and mounts into nothing. Every container it then starts on a
# user-defined network dies with
#
#   Error: failed to mount runtime directory for rootless netns: no such file or directory
#
# which is what killed pkdump-value-snapshots@prod every night from 2026-08-12.
# The store doing the removing is a CI gate's; the store left wedged is PROD's.
#
# deploy/store-lib.sh closes it by writing each non-prod store a containers.conf
# whose `[engine] tmp_dir` points inside that store's own runroot, so its
# scaffolding — and therefore its cleanup — can only ever be its own.
#
# tests/deploy/run.sh §8b asserts everything about that which is shell: the file
# is written, it names the right directory, it is handed back on deactivate,
# teardown does not leave it dangling. What it CANNOT assert is the only thing
# that actually matters — that podman honours it. `[engine] tmp_dir` moving the
# scaffolding is a fact about podman 4.9.3, not about this repo, and a fact about
# someone else's program is exactly the kind that stops being true quietly. So
# this file asks podman:
#
#   §1 an activated store's rootless netns is built under ITS OWN runroot
#   §2 the shared directory is not created by that, and not touched if it exists
#   §3 pkdump_store_netns_name derives the name podman really used
#
# NOT hermetic — it runs real podman. It needs no image, no container, no
# network and no registry: `podman unshare --rootless-netns true` runs the same
# setup a container start runs, in about a tenth of a second. It is also safe on
# the box that runs prod BY CONSTRUCTION — it only ever CREATES scaffolding, in
# a throwaway store of its own, and removes nothing outside it.
#
#   bash tests/store/netns_split.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
UID_N="$(id -u)"
RUNDIR="${XDG_RUNTIME_DIR:-/run/user/${UID_N}}"
SHARED="${RUNDIR}/libpod/tmp/rootless-netns"

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

command -v podman >/dev/null || {
	echo "ERROR: podman is not on PATH — this gate is the one store test that needs it." >&2
	exit 1
}

# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"

# deploy/ci.sh activates the box's own alternate store before it runs any gate,
# and this one is about a store it creates itself. Shimming a second store on top
# of an inherited one would leave `podman` exec'ing through two shims with two
# sets of --root flags, which is not a configuration anything else on this box
# ever runs. Start from the default store, deliberately.
pkdump_store_deactivate

# A store of this run's own. Keyed by the checkout path like every other gate in
# this repo, so concurrent polecats on one box do not collide — and under
# $TMPDIR, since it never holds an image or a layer, only the store skeleton.
WORK="$(mktemp -d)"
STORE_ROOT="${WORK}/pkdump-netns-split-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)"

cleanup() {
	# The store goes whatever happened, including its runroot, its scaffolding
	# and its netns name — a leaked store is 20 directories under /run/user that
	# nothing ever collects (pd-yfev), and a leaked netns name is worse.
	export PKDUMP_STORE_ROOT="$STORE_ROOT"
	pkdump_store_teardown >/dev/null 2>&1
	rm -rf "$WORK"
}
trap cleanup EXIT

# What the shared directory looks like BEFORE anything here runs. On a busy box
# it may well exist and be in use by prod right now; on a quiet one it will not
# exist at all. Either is fine — what must not change is which.
SHARED_BEFORE="$([ -d "$SHARED" ] && echo present || echo absent)"

# ---------------------------------------------------------------------------
log "1. An activated store builds its rootless netns under its own runroot"
# ---------------------------------------------------------------------------

export PKDUMP_STORE_ROOT="$STORE_ROOT"
pkdump_store_activate >/dev/null 2>&1 || {
	echo "ERROR: could not activate a store at ${STORE_ROOT}" >&2
	exit 1
}
RUNROOT="${PKDUMP_STORE_GLOBAL_ARGS##*--runroot=}"
GRAPH="${STORE_ROOT}/storage"

check "podman resolves to the throwaway store" "$GRAPH" \
	"$(podman info --format '{{.Store.GraphRoot}}' 2>/dev/null)"

# The probe. Same code path a `podman run --network <user-defined>` takes to set
# the namespace up, and the same error when it cannot.
PROBE_OUT="$(podman unshare --rootless-netns true 2>&1)"
PROBE_RC=$?
check "the rootless netns comes up" "0" "$PROBE_RC"
[[ $PROBE_RC -eq 0 ]] || echo "        ${PROBE_OUT}"

# THE MEASUREMENT. Not "a directory exists" — the scaffolding is the bind mount
# tree podman mounts the namespace's /run into, so the store's own runroot has to
# be where that tree ends up.
check "its scaffolding is inside this store's runroot" "present" \
	"$([ -d "${RUNROOT}/libpod-tmp/rootless-netns/run/user/${UID_N}" ] && echo present || echo absent)"
# podman's own marker that it used this tmp dir, and what
# pkdump_store_netns_repair keys on to know a store is post-split. If podman ever
# stops writing it, the repair silently starts reading the wrong directory.
check "and podman marked the tmp dir as its own" "present" \
	"$([ -e "${RUNROOT}/libpod-tmp/alive" ] && echo present || echo absent)"

# ---------------------------------------------------------------------------
log "2. The shared directory is left exactly as it was found"
# ---------------------------------------------------------------------------
#
# This is the whole bug, in one assertion. Before the split, the probe above
# would have created (and this store's cleanup would later have removed) the
# directory prod's network namespace is mounted into.

check "unchanged by a non-prod store coming up" "$SHARED_BEFORE" \
	"$([ -d "$SHARED" ] && echo present || echo absent)"
check "and it is not where this store was sent" "different" \
	"$([ "${RUNROOT}/libpod-tmp/rootless-netns" = "$SHARED" ] && echo same || echo different)"

# ---------------------------------------------------------------------------
log "3. The name podman gives it is the name the repair derives"
# ---------------------------------------------------------------------------
#
# pkdump_store_netns_repair and pkdump_store_teardown both address a netns file
# by a name this repo COMPUTES — sha256 of <graph root>/libpod, first ten bytes.
# tests/deploy/run.sh pins that to a vector measured once; here the running
# podman is asked, so a derivation that drifts with a podman upgrade fails
# loudly instead of becoming a repair that never fires and a teardown that
# leaves its namespace behind.

DERIVED="$(pkdump_store_netns_name "$GRAPH")"
check "the derived name is the file podman created" "present" \
	"$([ -e "${RUNDIR}/netns/${DERIVED}" ] && echo present || echo absent)"

# ---------------------------------------------------------------------------
log "4. A caller that never sets the override gets the split anyway"
# ---------------------------------------------------------------------------
#
# THE GAP THIS CLOSES, and it is not obvious. CONTAINERS_CONF_OVERRIDE is an
# environment variable, and the environment is exactly what a Quadlet unit does
# NOT inherit: systemd starts `podman run` with the unit's own environment, and
# the only thing the unit carries about the store is the `GlobalArgs=` line
# pkdump_store_stamp_unit writes. deploy/pkdump-nessie.container is on
# `Network=pkdump-lake-<instance>` — a user-defined network — so a non-prod
# instance in an alternate store runs a long-lived bridge container that this
# shell's variable can never reach. If that container's cleanup used the shared
# directory, the split would be decorative for the exact case it was bought for.
#
# It does not, and the reason is the caveat, pointed the useful way: podman
# records tmp_dir in the store's libpod database at creation and PINS it —
#
#   msg="Overriding tmp dir \"/run/user/…/libpod/tmp\" with \"…/libpod-tmp\" from database"
#
# so the split is a property of the STORE, not of the caller's environment. Every
# later caller inherits it whether it knows about any of this or not. (The same
# pin is why a store created BEFORE the split keeps sharing prod's scaffolding
# until deploy/store-teardown.sh is run once against it.)
#
# Measured here rather than reasoned about, because the whole fix rests on it.

QUADLET_TMP="$(env -u CONTAINERS_CONF_OVERRIDE podman --log-level=debug info 2>&1 |
	sed -n 's/.*Using tmp dir \([^"]*\)".*/\1/p' | tail -1)"
check "podman reads the store's tmp dir out of its own database" "${RUNROOT}/libpod-tmp" \
	"${QUADLET_TMP:-<none>}"

env -u CONTAINERS_CONF_OVERRIDE podman unshare --rootless-netns true >/dev/null 2>&1
QUADLET_RC=$?
check "and a bridge container it starts comes up" "0" "$QUADLET_RC"
check "still under this store's runroot" "present" \
	"$([ -d "${RUNROOT}/libpod-tmp/rootless-netns/run/user/${UID_N}" ] && echo present || echo absent)"
check "with the shared directory still as it was found" "$SHARED_BEFORE" \
	"$([ -d "$SHARED" ] && echo present || echo absent)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — a non-prod store's rootless-netns scaffolding is its own, and"
echo "         ${SHARED} is untouched."
