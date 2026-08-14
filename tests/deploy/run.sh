#!/usr/bin/env bash
# Deploy-script gate (pd-fite, pd-2t6u): the container-store resolution, the
# low-disk guard and the unit-file install are shell, so they get a shell test.
#
# Three properties are worth more than the rest:
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
#   A DEPLOY SHIPS THE UNIT FILES — the units are templates copied into
#   ~/.config at install time, and for months only setup.sh wrote those copies,
#   so prod ran a Litestream unit with no failure alerting while the repo said
#   it had some. §7 drives deploy.sh over a fake HOME holding stale units and
#   asserts every one comes back matching the template, port intact.
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

# Hermetic includes the runtime dir. Activation creates a runroot under
# $XDG_RUNTIME_DIR keyed by the store's graph path, and every run of this file
# used a fresh mktemp store — so every run left one more directory under the
# real /run/user/$UID that nothing collected. There were 20 of them on the box
# this was noticed on (pd-yfev). Point the whole file at a throwaway one; §8
# also needs to plant netns state under it, which must never be the real one.
export XDG_RUNTIME_DIR="${WORK}/run"
mkdir -p "$XDG_RUNTIME_DIR"

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
# end to end without touching a real store. `unshare` is the one verb it really
# runs: §7's teardown removes a rootless store through it, and a fake that only
# echoed would let a teardown that deletes nothing pass.
mkdir -p "${WORK}/fakebin"
cat > "${WORK}/fakebin/podman" <<'EOF'
#!/usr/bin/env bash
echo "PODMAN-ARGS: $*"
args=("$@")
while [[ ${#args[@]} -gt 0 && ${args[0]} == --* ]]; do args=("${args[@]:1}"); done
if [[ ${#args[@]} -gt 0 && ${args[0]} == unshare ]]; then
	exec "${args[@]:1}"
fi
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

reset_store

# ---------------------------------------------------------------------------
log "7. deploy.sh ships the unit files, not just the image (pd-2t6u)"
# ---------------------------------------------------------------------------
#
# The units under deploy/ are TEMPLATES; what an instance runs is a copy made at
# install time. Only setup.sh ever wrote those copies, so `deploy/deploy.sh prod`
# — the command an operator runs to ship a change — updated the binary and left
# the units at whatever version the instance was created with. Prod's Litestream
# sidecar was still the pre-multi-tenant template months later, missing
# `OnFailure=pkdump-alert@%n.service`: the backup that silently stopped
# replicating on 2026-08-08 had no failure alerting wired, while the repo said it
# did.
#
# Driven end to end against a fake HOME with stale units already installed.
# Hermetic: podman and systemctl are stubs, nothing is built and no unit is
# loaded.

reset_store

FAKE_HOME="${WORK}/deployhome"
QUADLET="${FAKE_HOME}/.config/containers/systemd"
UNITS="${FAKE_HOME}/.config/systemd/user"
mkdir -p "$QUADLET" "$UNITS" "${WORK}/deploybin"

# Stubs for the two commands deploy.sh drives. `systemctl is-active --quiet`
# must report NOT active, so the sidecar-restart branch stays out of the way.
cat > "${WORK}/deploybin/podman" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "${WORK}/deploybin/systemctl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do [ "$a" = is-active ] && exit 3; done
echo "SYSTEMCTL: $*" >> "$PKDUMP_TEST_SYSTEMCTL_LOG"
exit 0
EOF
chmod +x "${WORK}/deploybin/podman" "${WORK}/deploybin/systemctl"

# The instance as it exists on the box: a unit from an older checkout, still
# publishing the port everything reaches it on.
printf '[Unit]\nDescription=stale\n\n[Container]\nImage=localhost/pkdump:prod\nPublishPort=8090:8080\n' \
	> "${QUADLET}/pkdump-prod.container"
printf '[Unit]\nDescription=stale sidecar\n' \
	> "${QUADLET}/pkdump-litestream-prod.container"

SYSCTL_LOG="${WORK}/systemctl.log"
: > "$SYSCTL_LOG"
DEPLOY_OUT="$(
	PATH="${WORK}/deploybin:${ORIG_PATH}" \
		HOME="$FAKE_HOME" \
		PKDUMP_TEST_SYSTEMCTL_LOG="$SYSCTL_LOG" \
		bash "${REPO_DIR}/deploy/deploy.sh" prod 2>&1
)"

# The unit that had no alerting. This is the actual regression: prod's copy was
# missing keys the template had carried since Jun 2026.
check "sidecar unit gets OnFailure=" "1" \
	"$(grep -c '^OnFailure=pkdump-alert@%n.service$' "${QUADLET}/pkdump-litestream-prod.container" || true)"
check "sidecar unit gets its restart bounds" "2" \
	"$(grep -c '^StartLimit\(IntervalSec\|Burst\)=' "${QUADLET}/pkdump-litestream-prod.container" || true)"
check "sidecar unit names this checkout" "1" \
	"$(grep -c "^Volume=${REPO_DIR}/deploy/litestream.yml:/etc/litestream.yml:ro$" "${QUADLET}/pkdump-litestream-prod.container" || true)"

# Every unit the shipped templates define, byte for byte — not just the one that
# was noticed. Drift is per-file, so an assertion on one file finds one bug.
sed -e 's|{{INSTANCE}}|prod|g' -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
	"${REPO_DIR}/deploy/pkdump-litestream.container" > "${WORK}/ls.expected"
check "sidecar unit matches the template" "same" \
	"$(cmp -s "${QUADLET}/pkdump-litestream-prod.container" "${WORK}/ls.expected" && echo same || echo differs)"

sed -e 's|{{INSTANCE}}|prod|g' -e 's|{{PORT}}:8080|8090:8080|' \
	"${REPO_DIR}/deploy/pkdump.container" > "${WORK}/app.expected"
check "app unit matches the template" "same" \
	"$(cmp -s "${QUADLET}/pkdump-prod.container" "${WORK}/app.expected" && echo same || echo differs)"

# An outage caused by fixing the units is not a fix. Refreshing must never move
# an instance off the address everything reaches it on.
check "the published port survives the refresh" "1" \
	"$(grep -c '^PublishPort=8090:8080$' "${QUADLET}/pkdump-prod.container" || true)"

# The alarming and refresh templates travel with a deploy too — alarm-status.sh
# tells an operator to run setup.sh when one is missing, and a deploy that
# rewrites only the Quadlets would leave that advice permanently true.
for U in pkdump-alert@.service pkdump-backup-check@.service pkdump-backup-check@.timer \
	pkdump-refresh@.service pkdump-refresh@.timer pkdump-diskcheck.service pkdump-diskcheck.timer \
	pkdump-value-snapshots@.service pkdump-value-snapshots@.timer \
	pkdump-derive@.service pkdump-derive@.timer; do
	check "installs ${U}" "yes" "$([ -f "${UNITS}/${U}" ] && echo yes || echo no)"
done
check "no {{REPO_DIR}} left unsubstituted" "0" \
	"$(grep -rl '{{REPO_DIR}}' "$UNITS" "$QUADLET" 2>/dev/null | wc -l)"

# Drift is invisible by construction — installed copy here, template there — so
# the deploy that finally corrects it has to say so, or it reads identically to
# the deploy that corrected nothing.
check "names what it changed" "1" \
	"$(printf '%s' "$DEPLOY_OUT" | grep -c 'pkdump-litestream-prod.container' || true)"
check "reloads systemd after writing" "1" \
	"$(grep -c '^SYSTEMCTL: --user daemon-reload$' "$SYSCTL_LOG" || true)"

# Second run, nothing changed: converging must be idempotent, and must not claim
# work it did not do.
: > "$SYSCTL_LOG"
DEPLOY_OUT2="$(
	PATH="${WORK}/deploybin:${ORIG_PATH}" \
		HOME="$FAKE_HOME" \
		PKDUMP_TEST_SYSTEMCTL_LOG="$SYSCTL_LOG" \
		bash "${REPO_DIR}/deploy/deploy.sh" prod 2>&1
)"
check "a second deploy rewrites nothing" "1" \
	"$(printf '%s' "$DEPLOY_OUT2" | grep -c 'already match this checkout' || true)"
check "and leaves no temp files behind" "0" \
	"$(find "$QUADLET" "$UNITS" -name '.*.new.*' | wc -l)"

reset_store

# ---------------------------------------------------------------------------
log "8. A second store cannot take this store's rootless netns"
# ---------------------------------------------------------------------------
#
# pd-yfev. Podman 4.9 gives each store its own netns file but ONE shared
# scaffolding directory, and the first store to clean up removes it — leaving
# every other store holding a netns file that still looks valid and mounts into
# nothing. That store can then never start a container on a user-defined
# network, which is every container tests/litestream/{run,drill}.sh starts.
#
# Reproduced end to end against real podman before this was written; here it is
# the state machine that gets asserted, from planted directories.

reset_store
PATH="${WORK}/fakebin:${PATH}"
UID_N="$(id -u)"
NETNS_DIR="${XDG_RUNTIME_DIR}/netns"
SCAFFOLD="${XDG_RUNTIME_DIR}/libpod/tmp/rootless-netns/run/user/${UID_N}"
mkdir -p "$NETNS_DIR"

# The name podman derives, byte for byte: sha256 of <graph root>/libpod, first
# ten bytes. Pinned to a vector measured against podman 4.9.3 — a store at
# /workspaces/pd-netns-probe/storage really was given this name. Getting it
# wrong is not a loud failure, it is a repair that silently never fires, so the
# derivation is asserted rather than trusted.
check "netns name matches podman's derivation" \
	"rootless-netns-c94900efa81f2edcf008" \
	"$(pkdump_store_netns_name /workspaces/pd-netns-probe/storage)"

wedge_store() { # wedge_store <store root> — a netns file, no scaffolding
	mkdir -p "${1}/storage"
	rm -rf "${XDG_RUNTIME_DIR}/libpod"
	: > "${NETNS_DIR}/$(pkdump_store_netns_name "${1}/storage")"
}

# The wedge: activation drops the stale name so podman rebuilds it.
export PKDUMP_STORE_ROOT="${WORK}/wedged"
wedge_store "$PKDUMP_STORE_ROOT"
WEDGED_NS="${NETNS_DIR}/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"
pkdump_store_activate >/dev/null 2>&1
check "a stale netns name is dropped" "gone" \
	"$([ -e "$WEDGED_NS" ] && echo present || echo gone)"

# The guard, and it is the one that matters: scaffolding present means some
# store is USING that namespace right now. Removing the name there would break a
# live container instead of a wedged store.
reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/live"
wedge_store "$PKDUMP_STORE_ROOT"
mkdir -p "$SCAFFOLD"
LIVE_NS="${NETNS_DIR}/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"
pkdump_store_activate >/dev/null 2>&1
check "a live netns is left alone" "present" \
	"$([ -e "$LIVE_NS" ] && echo present || echo gone)"
rm -rf "${XDG_RUNTIME_DIR}/libpod"

# Prod's exposure is nil by construction: with no store opted in, the repair
# returns before it derives a name at all, so there is no code path that can
# compute — let alone remove — the default store's netns.
reset_store
PROD_NS="${NETNS_DIR}/$(pkdump_store_netns_name "${HOME}/.local/share/containers/storage")"
: > "$PROD_NS"
pkdump_store_netns_repair >/dev/null 2>&1
check "no store opted in -> prod's netns untouched" "present" \
	"$([ -e "$PROD_NS" ] && echo present || echo gone)"
rm -f "$PROD_NS"

# The repair that was found by hand first was `podman system migrate`, and it is
# the wrong one: it kills the pause process, which is per-USER and shared with
# the store prod runs in. A non-prod gate must not reach into prod's runtime
# state to fix itself. (The other foot-gun, the store-wide reset, is §6's job
# and it covers the whole repo.)
check "the repair does not migrate the user's podman" "0" \
	"$(grep -v '^ *#' "${REPO_DIR}/deploy/store-lib.sh" | grep -c 'system migrate' || true)"

reset_store

# ---------------------------------------------------------------------------
log "9. A store has a teardown of its own"
# ---------------------------------------------------------------------------
#
# deploy/teardown.sh removes an INSTANCE and leaves the store standing, which is
# correct — the store is shared. The consequence was that nothing removed a
# store, ever: 3.9G of images and a runroot per store on the box this was found
# on (pd-yfev).

reset_store
PATH="${WORK}/fakebin:${PATH}"

# No store configured means the target would be Podman's default store, which is
# prod's. That must refuse, not default.
set +e
TD_OUT="$(unset PKDUMP_STORE_ROOT; pkdump_store_teardown 2>&1)"
TD_RC=$?
set -e
check "refuses without a store" "1" "$TD_RC"
check "and says whose store that would be" "1" \
	"$(printf '%s' "$TD_OUT" | grep -c "default store" || true)"

export PKDUMP_STORE_ROOT="${WORK}/doomed"
pkdump_store_activate >/dev/null 2>&1
DOOMED_RUNROOT="${XDG_RUNTIME_DIR}/pkdump-store-$(printf '%s' "${PKDUMP_STORE_ROOT}/storage" | sha1sum | cut -c1-8)"
DOOMED_NS="${NETNS_DIR}/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"
: > "$DOOMED_NS"
mkdir -p "${PKDUMP_STORE_ROOT}/storage/overlay" "${DOOMED_RUNROOT}/overlay-layers"
# A second store's runroot, to prove the removal is keyed to the store and does
# not sweep the runtime dir.
BYSTANDER="${XDG_RUNTIME_DIR}/pkdump-store-decoy00"
mkdir -p "$BYSTANDER"

pkdump_store_teardown >/dev/null 2>&1

check "graph root removed" "gone" \
	"$([ -e "${PKDUMP_STORE_ROOT}/storage" ] && echo present || echo gone)"
check "buildah TMPDIR removed" "gone" \
	"$([ -e "${PKDUMP_STORE_ROOT}/tmp" ] && echo present || echo gone)"
check "the shim goes too" "gone" \
	"$([ -e "${PKDUMP_STORE_ROOT}/bin" ] && echo present || echo gone)"
check "its runroot removed" "gone" \
	"$([ -e "$DOOMED_RUNROOT" ] && echo present || echo gone)"
check "its netns name removed" "gone" \
	"$([ -e "$DOOMED_NS" ] && echo present || echo gone)"
check "another store's runroot survives" "present" \
	"$([ -e "$BYSTANDER" ] && echo present || echo gone)"

# A teardown that could not remove the store must SAY so. Reporting success over
# a store still on disk is the worse failure: the disk it was meant to free
# stays full and nothing anywhere says why. Simulated by making the store's
# parent unwritable, so the removal cannot unlink it.
reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/ro/store"
mkdir -p "${PKDUMP_STORE_ROOT}/storage"
pkdump_store_activate >/dev/null 2>&1
chmod 500 "${WORK}/ro"
set +e
STUCK_OUT="$(pkdump_store_teardown 2>&1)"
STUCK_RC=$?
set -e
chmod 700 "${WORK}/ro"
check "a store it could not remove is an error" "1" "$STUCK_RC"
check "and it names the store" "1" \
	"$(printf '%s' "$STUCK_OUT" | grep -c "${PKDUMP_STORE_ROOT} is still on disk" || true)"

# The CLI over it resolves the store the same way ci.sh does, and refuses the
# same way when the answer is "Podman's default".
reset_store
PATH="${WORK}/fakebin:${PATH}"
set +e
CLI_OUT="$(env -u PKDUMP_STORE_ROOT HOME="${WORK}/home" \
	bash "${REPO_DIR}/deploy/store-teardown.sh" 2>&1)"
CLI_RC=$?
set -e
check "store-teardown.sh exits non-zero unconfigured" "1" "$CLI_RC"
check "and removes nothing" "1" \
	"$(printf '%s' "$CLI_OUT" | grep -c 'nothing to remove' || true)"

reset_store

# ---------------------------------------------------------------------------
log "10. The transform tier is SCHEDULED, and 2 is not a failure (pd-8m5c)"
# ---------------------------------------------------------------------------
#
# `pkdump data refresh` step 7 is deleted (pd-hkbc), so
# pkdump-lake-value-snapshots is the only thing that records today's value for
# anybody — and it shipped with no unit, no timer and nothing under deploy/
# referencing it. deploy/LAKE.md said it "belongs on a timer" and that was the
# whole of the scheduling. This section exists because that gap existed
# precisely where nothing tested it.

reset_store

VS_SVC="${REPO_DIR}/deploy/pkdump-value-snapshots.service"
VS_TMR="${REPO_DIR}/deploy/pkdump-value-snapshots.timer"

# The ordering, which is the guarantee: the transform values a collection from
# catalog.prices, built from what the refresh lands. They may never run beside
# each other.
check "ordered after the refresh" "1" \
	"$(grep -c '^After=pkdump-refresh@%i.service$' "$VS_SVC" || true)"
# And NOT Wants=: the refresh is a oneshot without RemainAfterExit, so pulling it
# in would re-run the whole catalog fetch a second time every night.
check "does not pull the refresh in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-refresh@%i.service$' "$VS_SVC" || true)"

# 0 / 2 / 1 are three answers. 2 means "completed, some tenants skipped" — a
# tenant mid-import or a restore in flight — and a unit that called that a
# failure would page on a normal partial run and leave the timer's last run red.
check "exit 2 is a success for the unit" "1" \
	"$(grep -c '^SuccessExitStatus=2$' "$VS_SVC" || true)"
check "a real failure still pages" "1" \
	"$(grep -c '^OnFailure=pkdump-alert@%n.service$' "$VS_SVC" || true)"
# A box that has never had setup.sh run on it has no lake.env at all, and the unit
# skips rather than failing. It is only half a guard — setup.sh scaffolds that file
# commented out, so on every box that HAS been set up it exists and gates nothing;
# the other half is the wrapper's own refusal below.
check "skips a box with no lake config at all" "1" \
	"$(grep -c '^ConditionPathExists=%h/.config/pkdump/lake.env$' "$VS_SVC" || true)"

# The calendar entry is DERIVED from the refresh unit's own declared bounds
# rather than guessed at: last possible start + the time it is allowed to take.
# Move either number in pkdump-refresh.* and this fails rather than silently
# leaving the two jobs overlapping.
hhmm_secs() { # hhmm_secs <file> — OnCalendar's time of day, in seconds
	sed -n 's/^OnCalendar=\*-\*-\* \([0-9]\{2\}\):\([0-9]\{2\}\):.*/\1 \2/p' "$1" |
		awk '{print $1 * 3600 + $2 * 60; exit}'
}
key_secs() { # key_secs <file> <key> — a systemd seconds value, 0 if absent
	sed -n "s/^$2=\([0-9]\{1,\}\)$/\1/p" "$1" | awk 'NR==1{print; found=1} END{if(!found) print 0}'
}
REFRESH_LATEST=$((
	$(hhmm_secs "${REPO_DIR}/deploy/pkdump-refresh.timer") +
		$(key_secs "${REPO_DIR}/deploy/pkdump-refresh.timer" RandomizedDelaySec) +
		$(key_secs "${REPO_DIR}/deploy/pkdump-refresh.service" TimeoutStartSec)
))
VS_START=$(($(hhmm_secs "$VS_TMR") + $(key_secs "$VS_TMR" RandomizedDelaySec)))
check "fires no earlier than the refresh can finish" "ok" \
	"$([ "$VS_START" -ge "$REFRESH_LATEST" ] && echo ok || echo "starts ${VS_START}s < refresh ${REFRESH_LATEST}s")"

# A missed day is a permanent hole: each run writes exactly the date it is asked
# for, so nothing later fills it in.
check "catches up a missed run" "1" "$(grep -c '^Persistent=true$' "$VS_TMR" || true)"
check "timer is enablable" "1" "$(grep -c '^WantedBy=timers.target$' "$VS_TMR" || true)"

# An instance that is gone must not leave a timer behind firing at its volume.
check "teardown disables the timer" "1" \
	"$(grep -c 'pkdump-value-snapshots@\${INSTANCE}.timer' "${REPO_DIR}/deploy/teardown.sh" || true)"

# --- What the wrapper does with each exit status ----------------------------
# Driven end to end with a fake podman standing in for the job, because "2 is
# passed through, named and warned about; 1 is a failure; 0 is quiet" is
# behaviour, not a grep.

VS_HOME="${WORK}/vshome"
mkdir -p "${VS_HOME}/.config/pkdump" "${WORK}/vsbin"
printf 'PKDUMP_LAKE_S3_BUCKET=pdtest\nPKDUMP_LAKE_S3_REGION=us-west-2\n' \
	> "${VS_HOME}/.config/pkdump/lake.env"

# `secret inspect` fails (no bootstrap secret on a test instance) and `run`
# replays a canned job transcript with a canned status.
cat > "${WORK}/vsbin/podman" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"secret inspect") exit 1 ;;
"image exists" | "network exists") exit "${PKDUMP_TEST_NO_LAKEHOUSE:-0}" ;;
esac
if [ "$1" = run ]; then
	printf '==> 2026-08-11: 1 tenant(s) snapshotted, 1 skipped\n    skipped ghost: no database at /data/tenants/01J.sqlite\n'
	exit "${PKDUMP_TEST_JOB_RC:-0}"
fi
exit 0
EOF
chmod +x "${WORK}/vsbin/podman"

run_vs() { # run_vs <job exit status>
	set +e
	PATH="${WORK}/vsbin:${ORIG_PATH}" HOME="$VS_HOME" \
		PKDUMP_LAKE_ENV="${VS_HOME}/.config/pkdump/lake.env" \
		PKDUMP_ALERTS_ENV="${VS_HOME}/.config/pkdump/nonexistent.env" \
		PKDUMP_TEST_JOB_RC="$1" \
		bash "${REPO_DIR}/deploy/value-snapshots.sh" vstest 2>&1
	printf 'RC=%s' "$?"
	set -e
}

VS_OK="$(run_vs 0)"
check "a clean run exits 0" "1" "$(printf '%s' "$VS_OK" | grep -c 'RC=0$' || true)"
check "and says every tenant was done" "1" \
	"$(printf '%s' "$VS_OK" | grep -c 'value-snapshots: OK' || true)"

VS_PARTIAL="$(run_vs 2)"
check "a partial run exits 2, not 0" "1" "$(printf '%s' "$VS_PARTIAL" | grep -c 'RC=2$' || true)"
# Not silent: the journal line names WHO was skipped, not "some".
check "and names the tenant it skipped" "1" \
	"$(printf '%s' "$VS_PARTIAL" | grep -c 'PARTIAL — tenants skipped: ghost' || true)"
# And an undeliverable warning is reported rather than promoted to a failure —
# no Pushover channel is configured here, which is every test instance.
check "an undeliverable warning is not a failure" "1" \
	"$(printf '%s' "$VS_PARTIAL" | grep -c 'the PARTIAL warning reached nobody' || true)"

VS_FAILED="$(run_vs 1)"
check "a run that never started exits 1" "1" "$(printf '%s' "$VS_FAILED" | grep -c 'RC=1$' || true)"
check "and says so" "1" \
	"$(printf '%s' "$VS_FAILED" | grep -c 'value-snapshots: FAILED' || true)"

# The date the job refuses to default from the clock is supplied by the
# scheduler, which is the one component allowed to know what day it is.
check "the wrapper names today's date" "1" \
	"$(printf '%s' "$VS_OK" | grep -c -- "--date $(date +%F)" || true)"
# An explicit date wins — this is how a backfill runs through the same path.
check "an explicit --date is not overridden" "1" \
	"$(PATH="${WORK}/vsbin:${ORIG_PATH}" HOME="$VS_HOME" \
		PKDUMP_LAKE_ENV="${VS_HOME}/.config/pkdump/lake.env" \
		bash "${REPO_DIR}/deploy/value-snapshots.sh" vstest --date 2026-08-09 2>&1 |
		grep -c -- '--date 2026-08-09' || true)"

# No lake configured is a refusal that names the file to write, never a silent
# skip — the same rule --land-raw follows.
set +e
VS_NOLAKE="$(PATH="${WORK}/vsbin:${ORIG_PATH}" HOME="$VS_HOME" \
	PKDUMP_LAKE_ENV="${WORK}/nosuch.env" \
	bash "${REPO_DIR}/deploy/value-snapshots.sh" vstest 2>&1)"
VS_NOLAKE_RC=$?
set -e
check "no lake.env -> refuses" "1" "$VS_NOLAKE_RC"
check "and names the file" "1" \
	"$(printf '%s' "$VS_NOLAKE" | grep -c 'nosuch.env does not exist' || true)"

# An instance whose lakehouse was never installed. lake.env cannot answer this —
# deploy/setup.sh scaffolds it (commented out) on every box, so it exists
# everywhere and gates nothing. Observed before this check existed: the run died
# inside podman, retrying `localhost` as a container REGISTRY for a local-only
# image and exiting 125, so the operator read a network error instead of "you
# never ran setup-lake.sh".
set +e
VS_NOLAKEHOUSE="$(PATH="${WORK}/vsbin:${ORIG_PATH}" HOME="$VS_HOME" \
	PKDUMP_LAKE_ENV="${VS_HOME}/.config/pkdump/lake.env" \
	PKDUMP_TEST_NO_LAKEHOUSE=1 \
	bash "${REPO_DIR}/deploy/value-snapshots.sh" vstest 2>&1)"
VS_NOLAKEHOUSE_RC=$?
set -e
check "no job image -> fails (a timer armed at nothing must page)" "1" "$VS_NOLAKEHOUSE_RC"
check "and names the command that installs it" "1" \
	"$(printf '%s' "$VS_NOLAKEHOUSE" | grep -c 'deploy/setup-lake.sh vstest' || true)"
# And the job image is local-only, so a pull can only ever be podman mistaking
# `localhost` for a registry.
check "never pulls the job image" "1" \
	"$(grep -c '^podman run --rm --pull=never' "${REPO_DIR}/deploy/value-snapshots.sh" || true)"

reset_store

# ---------------------------------------------------------------------------
log "11. The image is built once per run and tagged, not rebuilt per gate (pd-5l2e)"
# ---------------------------------------------------------------------------
#
# Five gates in deploy/ci.sh need the shipped image and each wants its own tag,
# so ci.sh builds it once and exports PKDUMP_PREBUILT_IMAGE. Three properties
# are worth asserting, and the third is the one that decays quietly:
#
#   PROD STILL BUILDS — with the variable unset, pkdump_image_ensure is the
#   `podman build` every caller ran before. Prod never sets it, and neither does
#   a polecat running one gate by hand.
#
#   SET MEANS TAG — with it set to an image that exists, no builder runs at all.
#
#   SET-BUT-MISSING FAILS — a silent rebuild would turn "the build-once wiring
#   broke" into "CI got slower again", which is a regression nobody files.
#
# Hermetic: podman is a stub that records its arguments.

. "${REPO_DIR}/deploy/image-lib.sh"

IMGBIN="${WORK}/imgbin"
mkdir -p "$IMGBIN"
# `image exists` answers yes only for the name in $FAKE_EXISTING_IMAGE, which is
# how the missing-image case is driven without a store.
cat > "${IMGBIN}/podman" <<'EOF'
#!/usr/bin/env bash
echo "PODMAN: $*" >> "$PKDUMP_TEST_PODMAN_LOG"
if [ "$1" = image ] && [ "$2" = exists ]; then
	[ "$3" = "${FAKE_EXISTING_IMAGE:-}" ] && exit 0
	exit 1
fi
exit 0
EOF
chmod +x "${IMGBIN}/podman"
export PKDUMP_TEST_PODMAN_LOG="${WORK}/podman-image.log"

img_run() { # img_run <tag> -> podman calls, one per line
	: > "$PKDUMP_TEST_PODMAN_LOG"
	PATH="${IMGBIN}:${ORIG_PATH}" pkdump_image_ensure "$1" "$REPO_DIR" >/dev/null 2>&1
	printf '%s' "$(cat "$PKDUMP_TEST_PODMAN_LOG")"
}

unset PKDUMP_PREBUILT_IMAGE
check "unset -> builds from this checkout's Containerfile" \
	"PODMAN: build -t pkdump:x -f ${REPO_DIR}/Containerfile ${REPO_DIR}" \
	"$(img_run pkdump:x)"

export FAKE_EXISTING_IMAGE="localhost/pkdump:build-ci"
export PKDUMP_PREBUILT_IMAGE="$FAKE_EXISTING_IMAGE"
IMG_TAGGED="$(img_run pkdump:upgrade-abc)"
check "set -> tags it" \
	"PODMAN: tag localhost/pkdump:build-ci pkdump:upgrade-abc" \
	"$(printf '%s\n' "$IMG_TAGGED" | grep '^PODMAN: tag')"
check "...and no builder runs" "0" \
	"$(printf '%s\n' "$IMG_TAGGED" | grep -c '^PODMAN: build' || true)"

# The failure that must not be silent.
export PKDUMP_PREBUILT_IMAGE="localhost/pkdump:never-built"
set +e
IMG_MISSING="$(PATH="${IMGBIN}:${ORIG_PATH}" pkdump_image_ensure pkdump:y "$REPO_DIR" 2>&1)"
IMG_MISSING_RC=$?
set -e
check "set but missing -> refuses" "1" "$IMG_MISSING_RC"
check "and names the image it could not find" "1" \
	"$(printf '%s' "$IMG_MISSING" | grep -c 'localhost/pkdump:never-built' || true)"
check "and says how to build instead" "1" \
	"$(printf '%s' "$IMG_MISSING" | grep -c 'unset it' || true)"
unset PKDUMP_PREBUILT_IMAGE FAKE_EXISTING_IMAGE PKDUMP_TEST_PODMAN_LOG

# And the wiring itself, because a sixth gate added next month will copy a
# neighbour: nothing under tests/ and no CI-path deploy script may run the
# builder over the shipped Containerfile directly. deploy/{ci,deploy,seed,mac-*}.sh
# are the builders — ci.sh once per run, the others outside CI entirely.
BUILDERS="$(
	grep -rn --include='*.sh' -e 'podman build' "${REPO_DIR}/tests" "${REPO_DIR}/deploy" /dev/null |
		grep -F 'Containerfile' |
		grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' |
		grep -vE '/(image-lib\.sh|deploy\.sh|seed\.sh|mac-deploy\.sh|mac-setup\.sh|setup-lake\.sh):' |
		grep -vE 'lake/Containerfile' || true
)"
check "only image-lib.sh builds the shipped image on the CI path" "" "$BUILDERS"
# ...and the gates that need it really do go through the helper.
for gate in tests/tenants/upgrade.sh tests/tenants/handles.sh \
	tests/refresh/tenant_bytes.sh deploy/setup.sh; do
	check "${gate} tags rather than rebuilds" "1" \
		"$(grep -c '^[[:space:]]*pkdump_image_ensure ' "${REPO_DIR}/${gate}" || true)"
done

log "12. Landing and deriving are TWO units, and the derive is scheduled (pd-1uem)"
# ---------------------------------------------------------------------------
#
# Item 5 of the lake-as-source epic. A derive that shared a unit with the
# landing could not run on a night the fetch failed, which is the whole reason
# the two are split — and a split that nothing schedules is a catalog nobody
# rebuilds. Same failure shape as pd-8m5c one section up, so the same kind of
# assertions.

reset_store

DV_SVC="${REPO_DIR}/deploy/pkdump-derive.service"
DV_TMR="${REPO_DIR}/deploy/pkdump-derive.timer"

# They are separate units. Stated as an assertion because "two units" is the
# requirement, not an implementation detail: the landing unit must not have
# grown a derive step.
check "the landing unit does not derive from raw" "0" \
	"$(grep -c 'pkdump-lake-derive' "${REPO_DIR}/deploy/pkdump-refresh.service" || true)"
check "the derive unit does not fetch upstream" "0" \
	"$(grep -c 'data refresh' "$DV_SVC" || true)"

# The ordering, which is the guarantee: both write shared.sqlite today, so they
# may never run beside each other.
check "ordered after the landing" "1" \
	"$(grep -c '^After=pkdump-refresh@%i.service$' "$DV_SVC" || true)"
# And NOT Wants=: the landing is a oneshot without RemainAfterExit, so pulling it
# in would re-run the whole catalog fetch a second time every night.
check "does not pull the landing in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-refresh@%i.service$' "$DV_SVC" || true)"
# …and the transform runs after the derive, so the nightly chain is total.
check "the transform is ordered after the derive" "1" \
	"$(grep -c '^After=pkdump-derive@%i.service$' "${REPO_DIR}/deploy/pkdump-value-snapshots.service" || true)"

# There is no partial success for a catalog. Unlike the transform tier, which
# writes N tenant databases and legitimately exits 2, this job writes ONE
# catalog: it either holds that date's data or it does not.
check "no exit status is silently a success" "0" \
	"$(grep -c '^SuccessExitStatus=' "$DV_SVC" || true)"
check "a failure pages" "1" \
	"$(grep -c '^OnFailure=pkdump-alert@%n.service$' "$DV_SVC" || true)"
check "skips a box with no lake config at all" "1" \
	"$(grep -c '^ConditionPathExists=%h/.config/pkdump/lake.env$' "$DV_SVC" || true)"

# The calendar entry is DERIVED from the landing unit's own declared bounds
# rather than guessed at — the same computation §10 makes for the transform.
DV_START=$(($(hhmm_secs "$DV_TMR") + $(key_secs "$DV_TMR" RandomizedDelaySec)))
check "fires no earlier than the landing can finish" "ok" \
	"$([ "$DV_START" -ge "$REFRESH_LATEST" ] && echo ok || echo "starts ${DV_START}s < landing ${REFRESH_LATEST}s")"
check "catches up a missed run" "1" "$(grep -c '^Persistent=true$' "$DV_TMR" || true)"
check "timer is enablable" "1" "$(grep -c '^WantedBy=timers.target$' "$DV_TMR" || true)"
check "teardown disables the timer" "1" \
	"$(grep -c 'pkdump-derive@\${INSTANCE}.timer' "${REPO_DIR}/deploy/teardown.sh" || true)"

# --- What the wrapper actually does -----------------------------------------
# Driven end to end with a fake podman standing in for the job, because "the
# scheduler names the date", "a fallback is warned about but not a failure" and
# "no lake.env refuses by name" are behaviour, not greps.

DV_HOME="${WORK}/dvhome"
mkdir -p "${DV_HOME}/.config/pkdump" "${WORK}/dvbin"
printf 'PKDUMP_LAKE_S3_BUCKET=pdtest\nPKDUMP_LAKE_S3_REGION=us-west-2\n' \
	> "${DV_HOME}/.config/pkdump/lake.env"

cat > "${WORK}/dvbin/podman" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"secret inspect") exit 1 ;;
"image exists") exit "${PKDUMP_TEST_NO_IMAGE:-0}" ;;
esac
if [ "$1" = run ]; then
	printf 'Deriving ingest_date=... from dir /raw\n'
	[ -n "${PKDUMP_TEST_FELL_BACK:-}" ] &&
		printf '!! raw coverage has REGRESSED: https://tcgcsv.com/tcgplayer/3/9/prices is not in raw/\n'
	printf 'Derive complete: /data/shared.sqlite\n'
	exit "${PKDUMP_TEST_JOB_RC:-0}"
fi
exit 0
EOF
chmod +x "${WORK}/dvbin/podman"

run_derive() { # run_derive [extra args...]
	set +e
	PATH="${WORK}/dvbin:${ORIG_PATH}" HOME="$DV_HOME" \
		PKDUMP_LAKE_ENV="${DV_HOME}/.config/pkdump/lake.env" \
		PKDUMP_ALERTS_ENV="${DV_HOME}/.config/pkdump/nonexistent.env" \
		bash "${REPO_DIR}/deploy/derive.sh" dvtest "$@" 2>&1
	printf 'RC=%s' "$?"
	set -e
}

DV_OK="$(run_derive)"
check "a clean run exits 0" "1" "$(printf '%s' "$DV_OK" | grep -c 'RC=0$' || true)"
check "and says the catalog was rebuilt" "1" \
	"$(printf '%s' "$DV_OK" | grep -c 'derive: OK' || true)"
# The date the job refuses to default from the clock is supplied by the
# scheduler — the one component allowed to know what day it is. UTC, because
# that is what the ingest_date partition is named in.
check "the wrapper names today's UTC date" "1" \
	"$(printf '%s' "$DV_OK" | grep -c -- "--ingest-date $(date -u +%F)" || true)"
check "an explicit --ingest-date is not overridden" "1" \
	"$(run_derive --ingest-date 2026-08-09 | grep -c -- '--ingest-date 2026-08-09' || true)"

# A run that needed the temporary upstream fallback is CORRECT but not
# reproducible from the lake. That is a warning, not a failure, and it must not
# be swallowed just because the job exited 0.
DV_FELLBACK="$(PKDUMP_TEST_FELL_BACK=1 run_derive)"
check "a fallback still exits 0" "1" "$(printf '%s' "$DV_FELLBACK" | grep -c 'RC=0$' || true)"
check "…and is warned about, loudly" "1" \
	"$(printf '%s' "$DV_FELLBACK" | grep -c 'WARNING — raw coverage has regressed' || true)"
check "an undeliverable warning is not a failure" "1" \
	"$(printf '%s' "$DV_FELLBACK" | grep -c 'the coverage warning reached nobody' || true)"

DV_FAILED="$(PKDUMP_TEST_JOB_RC=1 run_derive)"
check "a failed derive exits 1" "1" "$(printf '%s' "$DV_FAILED" | grep -c 'RC=1$' || true)"
check "and says the catalog was NOT rebuilt" "1" \
	"$(printf '%s' "$DV_FAILED" | grep -c 'derive: FAILED' || true)"

# No lake configured is a refusal that names the file, never a silent skip.
set +e
DV_NOLAKE="$(PATH="${WORK}/dvbin:${ORIG_PATH}" HOME="$DV_HOME" \
	PKDUMP_LAKE_ENV="${WORK}/nosuch-derive.env" \
	bash "${REPO_DIR}/deploy/derive.sh" dvtest 2>&1)"
DV_NOLAKE_RC=$?
set -e
check "no lake.env -> refuses" "1" "$DV_NOLAKE_RC"
check "and names the file" "1" \
	"$(printf '%s' "$DV_NOLAKE" | grep -c 'nosuch-derive.env does not exist' || true)"

# An instance that was never built on this box. Without this the run dies inside
# podman retrying `localhost` as a container REGISTRY for a local-only image.
set +e
DV_NOIMAGE="$(PATH="${WORK}/dvbin:${ORIG_PATH}" HOME="$DV_HOME" \
	PKDUMP_LAKE_ENV="${DV_HOME}/.config/pkdump/lake.env" \
	PKDUMP_TEST_NO_IMAGE=1 \
	bash "${REPO_DIR}/deploy/derive.sh" dvtest 2>&1)"
DV_NOIMAGE_RC=$?
set -e
check "no image -> fails naming the command that builds it" "1" "$DV_NOIMAGE_RC"
check "and names setup.sh" "1" \
	"$(printf '%s' "$DV_NOIMAGE" | grep -c 'deploy/setup.sh dvtest' || true)"
check "never pulls the image" "1" \
	"$(grep -c '^podman run --rm --pull=never' "${REPO_DIR}/deploy/derive.sh" || true)"

reset_store

log "13. The ownership shipment is SCHEDULED, and 3 is its own answer (pd-dxn3)"
# ---------------------------------------------------------------------------
#
# The shipper is the last link in the nightly chain, and the same failure shape
# as §10 and §12 applies to it: a mechanism that works, wired to no schedule,
# is an outbox that grows forever while the offline side learns nothing.
#
# What is different here is the FOURTH status. Every other job on this box has
# three answers; this one has to distinguish "some tenants were skipped" from
# "events were LOST", because the first is a normal night and the second is a
# fact nothing else in the system would ever surface.

reset_store

SH_SVC="${REPO_DIR}/deploy/pkdump-ship.service"
SH_TMR="${REPO_DIR}/deploy/pkdump-ship.timer"

# The ordering, which is the guarantee: this unit and the transform both open
# EVERY tenant's database, so they may never run beside each other.
check "ordered after the transform" "1" \
	"$(grep -c '^After=pkdump-value-snapshots@%i.service$' "$SH_SVC" || true)"
# And NOT Wants=: every job in the chain is a oneshot without RemainAfterExit,
# so pulling one in would re-run it.
check "does not pull the transform in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-value-snapshots@%i.service$' "$SH_SVC" || true)"

# 2 is a partial night. 3 is NOT in SuccessExitStatus, deliberately: a gap is
# an incomplete offline copy that no later run can repair.
check "exit 2 is a success for the unit" "1" \
	"$(grep -c '^SuccessExitStatus=2$' "$SH_SVC" || true)"
check "a sequence gap is NOT a success" "0" \
	"$(grep -c '^SuccessExitStatus=.*3' "$SH_SVC" || true)"
check "a real failure pages" "1" \
	"$(grep -c '^OnFailure=pkdump-alert@%n.service$' "$SH_SVC" || true)"
check "skips a box with no lake config at all" "1" \
	"$(grep -c '^ConditionPathExists=%h/.config/pkdump/lake.env$' "$SH_SVC" || true)"

# The calendar entry is DERIVED from the transform unit's own declared bounds,
# the same computation §10 and §12 make. Move either number there and this
# fails rather than silently leaving the two jobs overlapping.
VS_LATEST=$((
	$(hhmm_secs "$VS_TMR") +
		$(key_secs "$VS_TMR" RandomizedDelaySec) +
		$(key_secs "$VS_SVC" TimeoutStartSec)
))
SH_START=$(($(hhmm_secs "$SH_TMR") + $(key_secs "$SH_TMR" RandomizedDelaySec)))
check "fires no earlier than the transform can finish" "ok" \
	"$([ "$SH_START" -ge "$VS_LATEST" ] && echo ok || echo "starts ${SH_START}s < transform ${VS_LATEST}s")"
check "catches up a missed run" "1" "$(grep -c '^Persistent=true$' "$SH_TMR" || true)"
check "timer is enablable" "1" "$(grep -c '^WantedBy=timers.target$' "$SH_TMR" || true)"
check "units-lib installs it for every instance" "1" \
	"$(grep -c 'pkdump-ship\.\${ext}' "${REPO_DIR}/deploy/units-lib.sh" || true)"
check "teardown disables the timer" "1" \
	"$(grep -c 'pkdump-ship@\${INSTANCE}.timer' "${REPO_DIR}/deploy/teardown.sh" || true)"

# --- What the wrapper does with each exit status ----------------------------
# Driven end to end with a fake podman standing in for the job, because the
# four-way mapping is behaviour rather than a grep — and because 3 has to be
# shown arriving as its own message rather than as a partial or a crash.

SH_HOME="${WORK}/shhome"
mkdir -p "${SH_HOME}/.config/pkdump/shtest" "${WORK}/shbin"
printf 'PKDUMP_LAKE_S3_BUCKET=pdtest\nPKDUMP_LAKE_S3_REGION=us-west-2\nPKDUMP_TENANT_AWS_PROFILE=pkdump-tenant\n' \
	> "${SH_HOME}/.config/pkdump/lake.env"
: > "${SH_HOME}/.config/pkdump/shtest/tenant-master.key"

cat > "${WORK}/shbin/podman" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"secret inspect") exit 1 ;;
"image exists") exit "${PKDUMP_TEST_NO_IMAGE:-0}" ;;
esac
if [ "$1" = run ]; then
	printf '==> shipping 2 tenant(s)\n'
	case "${PKDUMP_TEST_JOB_RC:-0}" in
	2) printf '    skipped 01J0000000000000000000000B: no key state is registered\n' ;;
	3) printf '    !! SEQUENCE GAP alice (01J0000000000000000000000A): seq 3..4 — 2 event(s) LOST\n' ;;
	esac
	exit "${PKDUMP_TEST_JOB_RC:-0}"
fi
exit 0
EOF
chmod +x "${WORK}/shbin/podman"

run_ship() { # run_ship [extra args...]
	set +e
	PATH="${WORK}/shbin:${ORIG_PATH}" HOME="$SH_HOME" \
		PKDUMP_LAKE_ENV="${SH_HOME}/.config/pkdump/lake.env" \
		PKDUMP_ALERTS_ENV="${SH_HOME}/.config/pkdump/nonexistent.env" \
		bash "${REPO_DIR}/deploy/ship.sh" shtest "$@" 2>&1
	printf 'RC=%s' "$?"
	set -e
}

SH_OK="$(run_ship)"
check "a clean run exits 0" "1" "$(printf '%s' "$SH_OK" | grep -c 'RC=0$' || true)"
check "and says so" "1" "$(printf '%s' "$SH_OK" | grep -c 'ship: OK' || true)"
# No date anywhere: every partition value comes out of the data.
check "the wrapper lends the job no clock" "0" \
	"$(printf '%s' "$SH_OK" | grep -c -- "--date" || true)"

SH_PARTIAL="$(PKDUMP_TEST_JOB_RC=2 run_ship)"
check "a partial run exits 2, unflattened" "1" \
	"$(printf '%s' "$SH_PARTIAL" | grep -c 'RC=2$' || true)"
check "…and the warning names WHO was skipped" "1" \
	"$(printf '%s' "$SH_PARTIAL" | grep -c 'PARTIAL — tenants skipped: 01J0000000000000000000000B' || true)"
check "an undeliverable warning is not a failure" "1" \
	"$(printf '%s' "$SH_PARTIAL" | grep -c 'the PARTIAL warning reached nobody' || true)"

# The status this whole item exists to make possible.
SH_GAP="$(PKDUMP_TEST_JOB_RC=3 run_ship)"
check "a sequence gap exits 3, not 1 and not 2" "1" \
	"$(printf '%s' "$SH_GAP" | grep -c 'RC=3$' || true)"
check "…and is alarmed as a GAP, naming the range" "1" \
	"$(printf '%s' "$SH_GAP" | grep -c 'ship: SEQUENCE GAP.*seq 3\.\.4' || true)"
check "…and not as a partial run" "0" \
	"$(printf '%s' "$SH_GAP" | grep -c 'PARTIAL' || true)"

SH_FAILED="$(PKDUMP_TEST_JOB_RC=1 run_ship)"
check "a failed run exits 1" "1" "$(printf '%s' "$SH_FAILED" | grep -c 'RC=1$' || true)"
check "and says nothing shipped" "1" \
	"$(printf '%s' "$SH_FAILED" | grep -c 'ship: FAILED' || true)"
# OnFailure= on the unit pushes the journal tail for a real failure; a second
# push from the wrapper would say less and arrive twice.
check "…without a second alert beside OnFailure=" "0" \
	"$(printf '%s' "$SH_FAILED" | grep -c 'reached nobody' || true)"

# The credential boundary, refused before podman starts — one profile for both
# zones is not a narrow policy, it is no boundary.
set +e
SH_NOPROFILE="$(PATH="${WORK}/shbin:${ORIG_PATH}" HOME="$SH_HOME" \
	PKDUMP_LAKE_ENV="${WORK}/noprofile.env" \
	bash -c "printf 'PKDUMP_LAKE_S3_BUCKET=b\n' > '${WORK}/noprofile.env'; \
	         PKDUMP_LAKE_ENV='${WORK}/noprofile.env' bash '${REPO_DIR}/deploy/ship.sh' shtest" 2>&1)"
SH_NOPROFILE_RC=$?
set -e
check "no tenant profile -> refuses" "1" "$SH_NOPROFILE_RC"
check "and says why the boundary matters" "1" \
	"$(printf '%s' "$SH_NOPROFILE" | grep -c 'PKDUMP_TENANT_AWS_PROFILE' || true)"

# No master key: nothing it ships could be encrypted, so it refuses by name
# rather than shipping plaintext or reporting a container failure.
set +e
SH_NOKEY="$(PATH="${WORK}/shbin:${ORIG_PATH}" HOME="$SH_HOME" \
	PKDUMP_LAKE_ENV="${SH_HOME}/.config/pkdump/lake.env" \
	PKDUMP_KEYS_CONF_DIR="${WORK}/nokeys" \
	bash "${REPO_DIR}/deploy/ship.sh" shtest 2>&1)"
SH_NOKEY_RC=$?
set -e
check "no master key -> refuses" "1" "$SH_NOKEY_RC"
check "and names the command that mints one" "1" \
	"$(printf '%s' "$SH_NOKEY" | grep -c 'deploy/keys.sh shtest init' || true)"

# No lake configured is a refusal that names the file, never a silent skip.
set +e
SH_NOLAKE="$(PATH="${WORK}/shbin:${ORIG_PATH}" HOME="$SH_HOME" \
	PKDUMP_LAKE_ENV="${WORK}/nosuch-ship.env" \
	bash "${REPO_DIR}/deploy/ship.sh" shtest 2>&1)"
SH_NOLAKE_RC=$?
set -e
check "no lake.env -> refuses" "1" "$SH_NOLAKE_RC"
check "and names the file" "1" \
	"$(printf '%s' "$SH_NOLAKE" | grep -c 'nosuch-ship.env does not exist' || true)"

set +e
SH_NOIMAGE="$(PATH="${WORK}/shbin:${ORIG_PATH}" HOME="$SH_HOME" \
	PKDUMP_LAKE_ENV="${SH_HOME}/.config/pkdump/lake.env" \
	PKDUMP_TEST_NO_IMAGE=1 \
	bash "${REPO_DIR}/deploy/ship.sh" shtest 2>&1)"
SH_NOIMAGE_RC=$?
set -e
check "no image -> fails naming the command that builds it" "1" "$SH_NOIMAGE_RC"
check "and names setup.sh" "1" \
	"$(printf '%s' "$SH_NOIMAGE" | grep -c 'deploy/setup.sh shtest' || true)"
check "never pulls the image" "1" \
	"$(grep -c 'podman run --rm --pull=never' "${REPO_DIR}/deploy/ship.sh" || true)"

# The master key is mounted read-ONLY and as a single file: this job DERIVES
# keys, and must never be the thing that can replace one.
check "the master key is mounted read-only" "1" \
	"$(grep -c 'tenant-master.key:ro' "${REPO_DIR}/deploy/ship.sh" || true)"

reset_store

# ---------------------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
