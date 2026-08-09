#!/usr/bin/env bash
# Deploy-script gate (pd-fite): the container-store resolution and the low-disk
# guard are shell, so they get a shell test.
#
# Two properties are worth more than the rest and are asserted first:
#
#   PROD IS UNAFFECTED — with PKDUMP_STORE_ROOT unset, every function here is a
#   no-op and a generated Quadlet unit comes out byte-identical to the one the
#   templates produced before any of this existed. Prod never opts in, so that
#   is the whole of prod's exposure.
#
#   THE GUARD ACTUALLY FIRES — a floor check that has never been seen to fail is
#   not a guard. §2 runs it against a floor it cannot satisfy and asserts both
#   the non-zero exit and the message that explains the bus error.
#
# Hermetic: no podman, no containers, no disk written outside a temp dir.
#
#   bash tests/deploy/run.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
DISKCHECK="${REPO_DIR}/deploy/diskcheck.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

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

# The library under test. Sourced into this shell, so every case has to put the
# environment back the way it found it — see reset_store below.
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"

ORIG_PATH="$PATH"
ORIG_TMPDIR="${TMPDIR:-}"
reset_store() {
	unset PKDUMP_STORE_ROOT PKDUMP_STORE_GLOBAL_ARGS
	PATH="$ORIG_PATH"
	if [ -n "$ORIG_TMPDIR" ]; then TMPDIR="$ORIG_TMPDIR"; else unset TMPDIR; fi
}

# ---------------------------------------------------------------------------
log "1. No store opted in — prod's path"
# ---------------------------------------------------------------------------

reset_store
pkdump_store_activate
check "PATH untouched" "$ORIG_PATH" "$PATH"
check "no store flags" "" "${PKDUMP_STORE_GLOBAL_ARGS:-}"

# The unit a no-store setup.sh would generate, byte for byte.
sed -e 's|{{INSTANCE}}|prod|g' -e 's|{{PORT}}:8080|8090:8080|' \
	"${REPO_DIR}/deploy/pkdump.container" > "${WORK}/prod.container"
cp "${WORK}/prod.container" "${WORK}/prod.expected"
pkdump_store_stamp_unit "${WORK}/prod.container"
check "prod's unit is byte-identical" "same" \
	"$(cmp -s "${WORK}/prod.container" "${WORK}/prod.expected" && echo same || echo differs)"
check "prod's unit has no GlobalArgs" "0" \
	"$(grep -c '^GlobalArgs=' "${WORK}/prod.container" || true)"

# ---------------------------------------------------------------------------
log "2. The low-disk guard fires"
# ---------------------------------------------------------------------------

# A floor no filesystem on any box can satisfy. If this ever passes, the guard
# is not looking at anything.
set +e
GUARD_OUT="$(PKDUMP_DISK_FLOOR_GB=999999999 bash "$DISKCHECK" --floor "$WORK" 2>&1)"
GUARD_RC=$?
set -e
check "exits non-zero under the floor" "1" "$GUARD_RC"
check "names the free space and the floor" "1" \
	"$(printf '%s' "$GUARD_OUT" | grep -c 'floor 999999999G' || true)"
check "explains the bus error" "1" \
	"$(printf '%s' "$GUARD_OUT" | grep -c 'Bus error' || true)"

set +e
OK_OUT="$(PKDUMP_DISK_FLOOR_GB=0 bash "$DISKCHECK" --floor "$WORK" 2>&1)"
OK_RC=$?
set -e
check "exits zero above the floor" "0" "$OK_RC"
check "reports the mount it checked" "1" \
	"$(printf '%s' "$OK_OUT" | grep -c 'free (floor 0G) — ok' || true)"

# ci.sh passes two paths that are usually one filesystem; a repeated device must
# not turn into a repeated line (or a doubled failure).
check "same filesystem checked once" "1" \
	"$(PKDUMP_DISK_FLOOR_GB=0 bash "$DISKCHECK" --floor "$WORK" "$WORK" | grep -c 'ok' || true)"

# A store root that has not been created yet still sits on a real filesystem.
check "unborn path resolves to its filesystem" "0" \
	"$(PKDUMP_DISK_FLOOR_GB=0 bash "$DISKCHECK" --floor "${WORK}/not/created/yet" >/dev/null 2>&1; echo $?)"

# Alert mode must stay a timer, not a gate: it reports and exits 0 even when the
# threshold is crossed (the push is alert.sh's job, and it no-ops unconfigured).
check "alert mode still exits zero" "0" \
	"$(PKDUMP_DISK_THRESHOLD=0 PKDUMP_DISK_PATH="$WORK" bash "$DISKCHECK" >/dev/null 2>&1; echo $?)"

# ---------------------------------------------------------------------------
log "3. Where the store root comes from: host config, never disk topology"
# ---------------------------------------------------------------------------

reset_store

# Nothing may derive a store path from the box's disk layout. The function this
# replaced did exactly that — "checkout on a different filesystem from $HOME" —
# and on a machine whose checkout sat on an external drive or a network mount it
# would have invented a store directory at the top of it (pd-rf7c).
check "the inferring function is gone" "" "$(type -t pkdump_store_default_root || true)"
check "store-lib.sh consults no filesystem to place a store" "0" \
	"$(grep -c 'df \|stat -c' "${REPO_DIR}/deploy/store-lib.sh" || true)"

# The host config file, in the directory alerts.env and litestream.env use.
mkdir -p "${WORK}/home/.config/pkdump"
CONF="${WORK}/home/.config/pkdump/store.env"
# load_config in a clean subshell, printing what it decided. Unset vs empty is
# the whole contract, so `${VAR-<unset>}` (no colon) is deliberate.
load() { # load <env assignment or nothing>
	env -u PKDUMP_STORE_ROOT HOME="${WORK}/home" \
		bash -c "${1:+export ${1};} . '${REPO_DIR}/deploy/store-lib.sh'; pkdump_store_load_config; printf '%s' \"\${PKDUMP_STORE_ROOT-<unset>}\""
}

check "no store.env -> unconfigured" "<unset>" "$(load)"

printf 'PKDUMP_STORE_ROOT=/big/disk/store\n' > "$CONF"
check "store.env supplies the root" "/big/disk/store" "$(load)"
check "an explicit root beats the file" "/elsewhere" "$(load 'PKDUMP_STORE_ROOT=/elsewhere')"
# The opt-out: one run wants Podman's default store even though this box's
# store.env opts in. An empty value is SET, so the file must not overrule it.
check "an explicit empty root beats the file" "" "$(load 'PKDUMP_STORE_ROOT=')"

# The shape setup.sh scaffolds: present, but deciding nothing.
printf '# PokeDumpster store config\n#PKDUMP_STORE_ROOT=/big/disk/store\n' > "$CONF"
check "commented-out store.env -> unconfigured" "<unset>" "$(load)"
rm -f "$CONF"

# Who is allowed to ask the host. ci.sh may; setup.sh must not, because setup.sh
# is also how prod is installed and prod's store is not a host-configurable
# thing. Prod's opt-out survives a box whose store.env opts in.
check "ci.sh reads host store config" "1" \
	"$(grep -c '^pkdump_store_load_config$' "${REPO_DIR}/deploy/ci.sh" || true)"
check "setup.sh does not" "0" \
	"$(grep -c 'pkdump_store_load_config' "${REPO_DIR}/deploy/setup.sh" || true)"
check "setup.sh scaffolds the knob commented out" "1" \
	"$(grep -c '^#PKDUMP_STORE_ROOT=' "${REPO_DIR}/deploy/setup.sh" || true)"

reset_store

# ---------------------------------------------------------------------------
log "4. Activation reaches every podman call"
# ---------------------------------------------------------------------------

reset_store
# A stand-in podman that just prints its arguments, so the shim can be driven
# end to end without touching a real store.
mkdir -p "${WORK}/fakebin"
cat > "${WORK}/fakebin/podman" <<'EOF'
#!/usr/bin/env bash
echo "PODMAN-ARGS: $*"
EOF
chmod +x "${WORK}/fakebin/podman"
PATH="${WORK}/fakebin:${PATH}"

export PKDUMP_STORE_ROOT="${WORK}/store"
pkdump_store_activate >/dev/null 2>&1

check "graph root is under the store" "1" \
	"$(printf '%s' "$PKDUMP_STORE_GLOBAL_ARGS" | grep -c -- "--root=${WORK}/store/storage" || true)"
check "runroot is set and separate" "1" \
	"$(printf '%s' "$PKDUMP_STORE_GLOBAL_ARGS" | grep -c -- '--runroot=' || true)"
check "runroot is not the default one" "0" \
	"$(printf '%s' "$PKDUMP_STORE_GLOBAL_ARGS" | grep -c -- '--runroot=[^ ]*/containers$' || true)"
check "buildah's cache mounts move too" "${WORK}/store/tmp" "${TMPDIR:-}"

# The property that matters: a bare `podman` — the form every deploy script and
# every test script uses — now carries the flags.
check "bare podman carries the flags" "PODMAN-ARGS: --root=${WORK}/store/storage $(printf '%s' "$PKDUMP_STORE_GLOBAL_ARGS" | grep -o -- '--runroot=[^ ]*') images" \
	"$(podman images)"

# Sourcing the library again in a child must not build a shim that exec's itself.
check "activation is idempotent" "PODMAN-ARGS: --root=${WORK}/store/storage $(printf '%s' "$PKDUMP_STORE_GLOBAL_ARGS" | grep -o -- '--runroot=[^ ]*') images" \
	"$(bash -c ". '${REPO_DIR}/deploy/store-lib.sh'; pkdump_store_activate >/dev/null; podman images")"

# ---------------------------------------------------------------------------
log "5. The unit records the store, and teardown reads it back"
# ---------------------------------------------------------------------------

sed -e 's|{{INSTANCE}}|ci-test|g' -e 's|{{PORT}}:8080|:8080|' \
	"${REPO_DIR}/deploy/pkdump.container" > "${WORK}/ci.container"
pkdump_store_stamp_unit "${WORK}/ci.container"
check "GlobalArgs written once" "1" "$(grep -c '^GlobalArgs=' "${WORK}/ci.container")"
check "GlobalArgs is inside [Container]" "1" \
	"$(awk '/^\[Container\]/{c=1;next} /^\[/{c=0} c && /^GlobalArgs=/{n++} END{print n+0}' "${WORK}/ci.container")"

# teardown.sh's recovery path: no PKDUMP_STORE_ROOT in the environment, the unit
# on disk is the only record of where the image and volume went.
mkdir -p "${WORK}/home/.config/containers/systemd"
cp "${WORK}/ci.container" "${WORK}/home/.config/containers/systemd/pkdump-ci-test.container"
check "store recovered from the unit" "${WORK}/store" \
	"$(HOME="${WORK}/home" bash -c ". '${REPO_DIR}/deploy/store-lib.sh'; unset PKDUMP_STORE_ROOT; pkdump_store_adopt_instance ci-test; printf '%s' \"\$PKDUMP_STORE_ROOT\"")"
check "an explicit root still wins" "/elsewhere" \
	"$(HOME="${WORK}/home" bash -c ". '${REPO_DIR}/deploy/store-lib.sh'; PKDUMP_STORE_ROOT=/elsewhere; pkdump_store_adopt_instance ci-test; printf '%s' \"\$PKDUMP_STORE_ROOT\"")"
check "no unit -> no store (prod's default)" "" \
	"$(HOME="${WORK}/home" bash -c ". '${REPO_DIR}/deploy/store-lib.sh'; unset PKDUMP_STORE_ROOT; pkdump_store_adopt_instance nosuchinstance; printf '%s' \"\${PKDUMP_STORE_ROOT:-}\"")"

# ---------------------------------------------------------------------------
log "6. Adopt is authoritative in BOTH directions (pd-9rxf)"
# ---------------------------------------------------------------------------

# The failing shape: deploy/ci.sh activates the alternate store, then invokes
# teardown.sh for a stale instance created BEFORE any of this existed — whose
# unit therefore carries no GlobalArgs. Adopt used to return early because a
# store was already set, so teardown's rmi/volume rm were aimed at a store that
# instance was never in, no-op'd, and then teardown deleted the unit: the image
# and the data volume stay in the default store with nothing able to find them.
# Same root cause, mirrored: deploy.sh builds into the alternate store while the
# unstamped unit keeps systemd on the default one, and the restart silently goes
# on serving the old image.
#
# So: an unstamped unit adopted inside an ACTIVATED shell must land back on the
# default store — asserted where it counts, on what a bare `podman` does.
sed -e 's|{{INSTANCE}}|ci-plain|g' -e 's|{{PORT}}:8080|:8080|' \
	"${REPO_DIR}/deploy/pkdump.container" > "${WORK}/home/.config/containers/systemd/pkdump-ci-plain.container"

# A unit stamped for a DIFFERENT store than the shell is on, to prove adopt
# switches stores as well as leaving them. Stamped through the real writer, in a
# subshell so the flags do not leak into this one.
sed -e 's|{{INSTANCE}}|ci-other|g' -e 's|{{PORT}}:8080|:8080|' \
	"${REPO_DIR}/deploy/pkdump.container" > "${WORK}/home/.config/containers/systemd/pkdump-ci-other.container"
(
	PKDUMP_STORE_GLOBAL_ARGS="--root=${WORK}/other/storage --runroot=/run/nowhere"
	pkdump_store_stamp_unit "${WORK}/home/.config/containers/systemd/pkdump-ci-other.container"
)

# Adopt inside a shell that has already activated ${WORK}/store, then activate
# again the way every deploy script does, and report what podman resolves to.
# TMPDIR is unset going in so its restoration is visible.
adopt_activated() { # adopt_activated <instance>
	HOME="${WORK}/home" PATH="${WORK}/fakebin:${ORIG_PATH}" \
		env -u TMPDIR -u PKDUMP_STORE_ROOT -u PKDUMP_STORE_GLOBAL_ARGS -u PKDUMP_STORE_PREV_TMPDIR \
		bash -c "
			. '${REPO_DIR}/deploy/store-lib.sh'
			export PKDUMP_STORE_ROOT='${WORK}/store'
			pkdump_store_activate >/dev/null 2>&1
			pkdump_store_adopt_instance '$1'
			pkdump_store_activate >/dev/null 2>&1
			podman images
			printf 'root=[%s] tmpdir=[%s]' \"\${PKDUMP_STORE_ROOT:-}\" \"\${TMPDIR:-}\"
		"
}

check "unstamped unit -> podman drops the store flags" "PODMAN-ARGS: images" \
	"$(adopt_activated ci-plain | head -n1)"
check "unstamped unit -> root cleared and TMPDIR restored" "root=[] tmpdir=[]" \
	"$(adopt_activated ci-plain | tail -n1)"

# A stamped unit is still obeyed, including when it names a store other than the
# one the calling shell activated. The runroot is not read back from the unit —
# activate derives it from the graph root — so only the root is asserted.
check "stamped unit wins over the activated store" "1" \
	"$(adopt_activated ci-other | head -n1 | grep -c -- "--root=${WORK}/other/storage" || true)"
check "the abandoned store's shim leaves PATH" "0" \
	"$(adopt_activated ci-other | head -n1 | grep -c -- "--root=${WORK}/store/storage" || true)"
check "stamped unit -> root and TMPDIR follow it" \
	"root=[${WORK}/other] tmpdir=[${WORK}/other/tmp]" \
	"$(adopt_activated ci-other | tail -n1)"

# A unit stamped for the store we are already on must change nothing.
check "same store adopted twice is a no-op" \
	"PODMAN-ARGS: --root=${WORK}/store/storage $(printf '%s' "$PKDUMP_STORE_GLOBAL_ARGS" | grep -o -- '--runroot=[^ ]*') images" \
	"$(adopt_activated ci-test | head -n1)"

# Prod's path through the new code: nothing activated, an unstamped unit. PATH
# and TMPDIR must come out exactly as they went in — deactivate may only touch
# them when there is a shim to remove.
check "prod: adopting an unstamped unit leaves PATH and TMPDIR alone" "same" \
	"$(HOME="${WORK}/home" TMPDIR=/var/tmp PATH="$ORIG_PATH" \
		env -u PKDUMP_STORE_ROOT bash -c ". '${REPO_DIR}/deploy/store-lib.sh'; pkdump_store_adopt_instance ci-plain; pkdump_store_activate; [ \"\$PATH\" = '${ORIG_PATH}' ] && [ \"\${TMPDIR:-}\" = /var/tmp ] && echo same || echo differs")"
check "prod: no store flags to stamp a unit with" "" \
	"$(HOME="${WORK}/home" env -u PKDUMP_STORE_ROOT bash -c ". '${REPO_DIR}/deploy/store-lib.sh'; pkdump_store_adopt_instance ci-plain; printf '%s' \"\${PKDUMP_STORE_GLOBAL_ARGS:-}\"")"

# The scripts that operate on an EXISTING instance must all ask it where it
# lives; one that only activates puts the image in one store and the unit's
# systemd lookup in another. setup.sh included: re-running it is how unit-file
# changes reach an instance, and that must not move its store.
for s in teardown deploy setup seed restore-litestream backup-check; do
	check "deploy/${s}.sh adopts the instance's store" "1" \
		"$(grep -c 'pkdump_store_adopt_instance' "${REPO_DIR}/deploy/${s}.sh" || true)"
done

reset_store

# ---------------------------------------------------------------------------
log "6. nothing in the repo invokes podman's store-wide reset (pd-rkrf)"
# ---------------------------------------------------------------------------

# `podman system reset` is NOT scoped by --root/--runroot. Aimed at a throwaway
# store it still wiped /run/user/$UID/libpod, the rootless SHM lock and the
# buildah cache at the ambient TMPDIR — and took prod down with it, podman
# answering "container state improper" while `pkdump serve` was still alive.
# store-lib.sh's header carries the correct removal recipe (stop/rm what the
# store owns, then rm -rf the store root and its runroot); this is what keeps the
# next store-teardown command from reaching for the foot-gun anyway.
#
# Comment lines are excluded: the recipe has to be allowed to name what it
# forbids. The needle is spelled in two pieces so this file does not match
# itself, which is also why no line of code below writes the command out.
NEEDLE='system'"[[:space:]]+"'reset'
OFFENDERS="$(
	grep -rnE --include='*.sh' --include='*.container' --include='*.service' \
		--include='*.timer' --include='Containerfile' -e "$NEEDLE" \
		"${REPO_DIR}/deploy" "${REPO_DIR}/tests" /dev/null |
		grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' || true
)"
check "no script resets podman storage" "" "$OFFENDERS"

# ---------------------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
