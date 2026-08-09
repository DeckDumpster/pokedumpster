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

reset_store

# ---------------------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
