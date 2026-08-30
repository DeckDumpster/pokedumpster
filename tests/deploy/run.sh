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
# Unset first, not captured as-is like PATH/TMPDIR above: this suite asserts
# "no override at all" as a baseline in several places, and a loaded CI box can
# carry one in ambient from an un-torn-down store elsewhere on the runner
# (pd-3zjt/pd-1sy1 — /workspaces/pkdump-nonprod-store predates the tmp_dir
# split). Trusting that ambient value as "the original" would make this suite's
# own baseline depend on host state it does not control, exactly what a
# hermetic test exists to avoid.
unset CONTAINERS_CONF_OVERRIDE
ORIG_CONTAINERS_CONF_OVERRIDE=""
reset_store() {
	unset PKDUMP_STORE_ROOT PKDUMP_STORE_GLOBAL_ARGS
	unset PKDUMP_STORE_PREV_CONTAINERS_CONF_OVERRIDE
	PATH="$ORIG_PATH"
	if [ -n "$ORIG_TMPDIR" ]; then TMPDIR="$ORIG_TMPDIR"; else unset TMPDIR; fi
	# Activation exports this at a store's own containers.conf; leaving one
	# behind would point every later case — and podman itself — at a file under
	# a store that case has finished with.
	if [ -n "$ORIG_CONTAINERS_CONF_OVERRIDE" ]; then
		CONTAINERS_CONF_OVERRIDE="$ORIG_CONTAINERS_CONF_OVERRIDE"
	else
		unset CONTAINERS_CONF_OVERRIDE
	fi
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
[ -n "${PKDUMP_TEST_DEPLOY_PODMAN_LOG:-}" ] && echo "PODMAN: $*" >> "$PKDUMP_TEST_DEPLOY_PODMAN_LOG"
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
PODMAN_LOG="${WORK}/deploy-podman.log"
: > "$SYSCTL_LOG"
: > "$PODMAN_LOG"
DEPLOY_OUT="$(
	PATH="${WORK}/deploybin:${ORIG_PATH}" \
		HOME="$FAKE_HOME" \
		PKDUMP_TEST_SYSTEMCTL_LOG="$SYSCTL_LOG" \
		PKDUMP_TEST_DEPLOY_PODMAN_LOG="$PODMAN_LOG" \
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
	pkdump-derive@.service pkdump-derive@.timer \
	pkdump-prices@.service pkdump-prices@.timer; do
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

# --- The OTHER image this checkout ships (pd-rn4c) --------------------------
#
# `lake/` builds a second image — the PyIceberg runtime the nightly price build
# and the value-snapshots transform run in — and only deploy/setup-lake.sh, the
# ONE-TIME installer, ever built it. So `deploy/deploy.sh prod` shipped the new
# binary and left the job image at whatever version the lakehouse was installed
# with, which is pd-2t6u's bug again against a different image.
#
# It is invisible from outside: the stale jobs keep exiting 0. Prod ran a
# transform six hours older than its own checkout for a day, recording no
# dimension='sealed' row at all — $10,351.47 of sealed product missing from a
# chart that reported a plausible $10,636.81, over "1 tenant(s) snapshotted, 0
# skipped".

# One: a box with no lakehouse must not grow one. Installing one needs a bucket
# name that lives in host config, and most instances — every `--test` one CI
# throws away — have none. Asserted on the runs above, which had no Nessie unit.
check "no lakehouse -> no lake job image built" "0" \
	"$(grep -c 'PODMAN: build .*lake/Containerfile' "$PODMAN_LOG" || true)"
check "and it says the lake image was skipped" "1" \
	"$(printf '%s' "$DEPLOY_OUT" | grep -c 'No lakehouse installed' || true)"

# Two: with a lakehouse installed, a deploy rebuilds its job image. The Nessie
# Quadlet unit is the marker rather than the image, because `setup-lake.sh
# --remove` deletes that unit and deliberately keeps the image — so an
# image-exists test would go on rebuilding for an uninstalled lakehouse.
printf '[Unit]\nDescription=nessie\n\n[Container]\nImage=ghcr.io/projectnessie/nessie\n' \
	> "${QUADLET}/pkdump-nessie-prod.container"
: > "$PODMAN_LOG"
: > "$SYSCTL_LOG"
DEPLOY_OUT3="$(
	PATH="${WORK}/deploybin:${ORIG_PATH}" \
		HOME="$FAKE_HOME" \
		PKDUMP_TEST_SYSTEMCTL_LOG="$SYSCTL_LOG" \
		PKDUMP_TEST_DEPLOY_PODMAN_LOG="$PODMAN_LOG" \
		bash "${REPO_DIR}/deploy/deploy.sh" prod 2>&1
)"
check "a lakehouse -> the job image is rebuilt" "1" \
	"$(grep -c "PODMAN: build -t localhost/pkdump-lake:prod -f ${REPO_DIR}/lake/Containerfile ${REPO_DIR}/lake" "$PODMAN_LOG" || true)"
check "and the app image is still built too" "1" \
	"$(grep -c "PODMAN: build -t pkdump:latest -f ${REPO_DIR}/Containerfile ${REPO_DIR}" "$PODMAN_LOG" || true)"
check "and it names what it rebuilt" "1" \
	"$(printf '%s' "$DEPLOY_OUT3" | grep -c 'Rebuilding the lake job image localhost/pkdump-lake:prod' || true)"

# Three: the ORDER. The app is what serves requests; a lake build that fails
# must never be able to leave the new binary built and not running. So the
# restart happens first, and this is the assertion that keeps it there when
# somebody tidies the file.
check "the app restarts BEFORE the lake image is built" "yes" \
	"$(printf '%s' "$DEPLOY_OUT3" | awk '
		/restarted\. Port:/ { app = NR }
		/Rebuilding the lake job image/ { lake = NR }
		END { print (app && lake && app < lake) ? "yes" : "no" }')"

# Four: one builder. setup-lake.sh installs a lakehouse and deploy.sh ships a
# change to one; the day those two build the image differently is the day a
# deploy produces an image the installer would not have.
check "setup-lake.sh builds it through the helper" "1" \
	"$(grep -c '^pkdump_lake_job_image_build ' "${REPO_DIR}/deploy/setup-lake.sh" || true)"
check "deploy.sh builds it through the same helper" "1" \
	"$(grep -c 'pkdump_lake_job_image_build ' "${REPO_DIR}/deploy/deploy.sh" || true)"
check "and setup-lake.sh no longer runs the builder itself" "0" \
	"$(grep -c 'podman build' "${REPO_DIR}/deploy/setup-lake.sh" || true)"
check "and deploy.sh runs it only for the app image" "1" \
	"$(grep -c 'podman build' "${REPO_DIR}/deploy/deploy.sh" || true)"

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
log "8b. …because the scaffolding is SPLIT, not shared (pd-3zjt)"
# ---------------------------------------------------------------------------
#
# §8 is the repair, and it only ever ran on the side that opts into a store —
# PROD NEVER DOES. So the damage in the direction that mattered was never
# addressed at all: a CI gate's last bridge-network container exits, podman
# RemoveAll's the shared directory, and the store left holding a netns file that
# mounts into nothing is prod's. Every prod container on a user-defined network
# then died at start, which is what failed pkdump-value-snapshots@prod every
# night from 2026-08-12.
#
# The fix is not a better repair, it is removing the sharing: a containers.conf
# written per store moves that store's `[engine] tmp_dir`, and the scaffolding
# goes with it. Measured against podman 4.9.3 for this bead — an activated store
# put its rootless-netns under its own runroot and the shared directory was never
# created. tests/store/netns_split.sh makes that same claim against real podman;
# what is asserted here is everything around it that is shell.

reset_store
PATH="${WORK}/fakebin:${PATH}"

store_runroot() { # store_runroot <store root> — where activation puts it
	printf '%s/pkdump-store-%s' "$XDG_RUNTIME_DIR" \
		"$(printf '%s' "${1}/storage" | sha1sum | cut -c1-8)"
}

export PKDUMP_STORE_ROOT="${WORK}/split"
pkdump_store_activate >/dev/null 2>&1
SPLIT_RUNROOT="$(store_runroot "$PKDUMP_STORE_ROOT")"

check "activation points podman at a containers.conf" "${PKDUMP_STORE_ROOT}/containers.conf" \
	"${CONTAINERS_CONF_OVERRIDE:-unset}"
check "which exists" "present" \
	"$([ -f "${CONTAINERS_CONF_OVERRIDE:-/nonexistent}" ] && echo present || echo gone)"
# The whole of the fix, in one line of generated config.
check "and names this store's own tmp dir" "1" \
	"$(grep -c "^tmp_dir = \"${SPLIT_RUNROOT}/libpod-tmp\"\$" "$CONTAINERS_CONF_OVERRIDE" || true)"
# The one it must never name. Anything under $XDG_RUNTIME_DIR/libpod is the
# directory every other store — prod's included — is already using.
check "never the shared one" "0" \
	"$(grep -c "${XDG_RUNTIME_DIR}/libpod/" "$CONTAINERS_CONF_OVERRIDE" || true)"
# CONTAINERS_CONF_OVERRIDE, not CONTAINERS_CONF: the override MERGES on top of
# whatever containers.conf the box already has, where the plain form would
# replace it and quietly drop every other setting on the machine.
check "merges rather than replaces the box's config" "0" \
	"$(grep -c 'CONTAINERS_CONF=' "${REPO_DIR}/deploy/store-lib.sh" || true)"

# Two stores, two directories — the runroot is keyed by the graph path, so this
# holds for any number of them without a registry of who has which.
reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/split2"
pkdump_store_activate >/dev/null 2>&1
check "another store gets another tmp dir" "1" \
	"$(grep -c "^tmp_dir = \"$(store_runroot "$PKDUMP_STORE_ROOT")/libpod-tmp\"\$" "$CONTAINERS_CONF_OVERRIDE" || true)"

# Prod's exposure, again by construction: prod opts into no store, so nothing
# writes it a containers.conf and podman's own tmp dir is left exactly where
# podman puts it. The isolation is entirely on the side that was doing the damage.
reset_store
pkdump_store_activate >/dev/null 2>&1
check "no store opted in -> no override at all" "unset" "${CONTAINERS_CONF_OVERRIDE:-unset}"

# A box that already sets one for its own reasons gets it back. The store's conf
# is scoped to the process tree that activated the store, like PATH and TMPDIR.
reset_store
PATH="${WORK}/fakebin:${PATH}"
export CONTAINERS_CONF_OVERRIDE="${WORK}/host-owned.conf"
: > "$CONTAINERS_CONF_OVERRIDE"
export PKDUMP_STORE_ROOT="${WORK}/split3"
pkdump_store_activate >/dev/null 2>&1
check "a host's own override is displaced" "${PKDUMP_STORE_ROOT}/containers.conf" \
	"$CONTAINERS_CONF_OVERRIDE"
pkdump_store_deactivate
check "…and handed back on deactivate" "${WORK}/host-owned.conf" \
	"${CONTAINERS_CONF_OVERRIDE:-unset}"
reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/split4"
pkdump_store_activate >/dev/null 2>&1
pkdump_store_deactivate
check "with nothing to hand back, it is unset" "unset" "${CONTAINERS_CONF_OVERRIDE:-unset}"

# Teardown deletes the store root, and the conf goes with it. Podman REFUSES TO
# RUN AT ALL while the variable names a file that is not there — including
# against the default store, which is prod's — so a shell that tore a store down
# would come out of it unable to use podman for anything, complaining about a
# path inside a store that no longer exists.
reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/split-doomed"
pkdump_store_activate >/dev/null 2>&1
pkdump_store_teardown >/dev/null 2>&1
check "teardown leaves no override naming a deleted file" "unset" \
	"${CONTAINERS_CONF_OVERRIDE:-unset}"

# The repair reads whichever scaffolding is AUTHORITATIVE for the store. A store
# created since the split has its own, and `alive` is podman's own marker that it
# has used it — the directory alone proves nothing, since activation creates it.
reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/own-gone"
mkdir -p "${PKDUMP_STORE_ROOT}/storage"
OWN_GONE_NS="${NETNS_DIR}/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"
: > "$OWN_GONE_NS"
mkdir -p "$(store_runroot "$PKDUMP_STORE_ROOT")/libpod-tmp"
: > "$(store_runroot "$PKDUMP_STORE_ROOT")/libpod-tmp/alive"
# The SHARED one is present and busy — which under the pre-split reading meant
# "leave it alone", and would have left this store wedged forever.
mkdir -p "$SCAFFOLD"
pkdump_store_activate >/dev/null 2>&1
check "its own scaffolding gone -> stale, dropped" "gone" \
	"$([ -e "$OWN_GONE_NS" ] && echo present || echo gone)"

reset_store
PATH="${WORK}/fakebin:${PATH}"
export PKDUMP_STORE_ROOT="${WORK}/own-live"
mkdir -p "${PKDUMP_STORE_ROOT}/storage"
OWN_LIVE_NS="${NETNS_DIR}/$(pkdump_store_netns_name "${PKDUMP_STORE_ROOT}/storage")"
: > "$OWN_LIVE_NS"
OWN_LIVE_TMP="$(store_runroot "$PKDUMP_STORE_ROOT")/libpod-tmp"
mkdir -p "${OWN_LIVE_TMP}/rootless-netns/run/user/${UID_N}"
: > "${OWN_LIVE_TMP}/alive"
# And the shared one is gone, which under the pre-split reading meant "stale,
# drop it" — on a namespace this store may be running containers on right now.
rm -rf "${XDG_RUNTIME_DIR}/libpod"
pkdump_store_activate >/dev/null 2>&1
check "its own scaffolding intact -> left alone" "present" \
	"$([ -e "$OWN_LIVE_NS" ] && echo present || echo gone)"
rm -f "$OWN_LIVE_NS"

# A store CREATED before the split keeps sharing prod's scaffolding whatever the
# generated config says — podman pins tmp_dir in the store's database at creation
# — so the fix needs a one-time `deploy/store-teardown.sh` per box. An operator
# action nothing checks is one nobody knows is outstanding: the config would say
# exactly the right thing, every gate would pass, and the store would go on
# sharing prod's scaffolding with no symptom until the night a cleanup lands
# between prod and its namespace. Podman is the only one who can answer, since
# the pin is in its database and not in anything on disk to stat.
reset_store
mkdir -p "${WORK}/splitbin"
cat > "${WORK}/splitbin/podman" <<'EOF'
#!/usr/bin/env bash
cat "${SPLIT_SAYS}" 2>/dev/null
exit 0
EOF
chmod +x "${WORK}/splitbin/podman"
PATH="${WORK}/splitbin:${PATH}"
export SPLIT_SAYS="${WORK}/split-says"
export PKDUMP_STORE_ROOT="${WORK}/pinned"
pkdump_store_activate >/dev/null 2>&1
PINNED_TMP="$(store_runroot "$PKDUMP_STORE_ROOT")/libpod-tmp"

# A store that took it: podman settled on the directory the config asked for.
printf '%s\n' "time=\"…\" level=debug msg=\"Using tmp dir ${PINNED_TMP}\"" > "$SPLIT_SAYS"
check "a split store says nothing" "" "$(pkdump_store_split_check 2>&1)"

# A store that did not, which is every store that existed before this landed.
{
	printf '%s\n' "time=\"…\" level=debug msg=\"Overriding tmp dir \\\"${XDG_RUNTIME_DIR}/libpod/tmp\\\" with \\\"${PINNED_TMP}\\\" from database\""
	printf '%s\n' "time=\"…\" level=debug msg=\"Using tmp dir ${XDG_RUNTIME_DIR}/libpod/tmp\""
} > "$SPLIT_SAYS"
SPLIT_OUT="$(pkdump_store_split_check 2>&1)"
check "a pre-split store is called out" "1" \
	"$(printf '%s' "$SPLIT_OUT" | grep -c 'PREDATES the rootless-netns split' || true)"
check "and it names both directories" "1" \
	"$(printf '%s' "$SPLIT_OUT" | grep -c "using:  ${XDG_RUNTIME_DIR}/libpod/tmp\$" || true)"
# Actionable or it is noise: the command, and what running it costs.
check "and the one command that fixes it" "1" \
	"$(printf '%s' "$SPLIT_OUT" | grep -c 'deploy/store-teardown.sh' || true)"
check "and it is a warning, not a failure" "0" \
	"$(pkdump_store_split_check >/dev/null 2>&1; echo $?)"

# Podman not answering in a shape this understands — a version that words the
# line differently — must be silent. A warning about a store that may be
# perfectly fine is how a real one gets ignored.
printf '%s\n' "time=\"…\" level=debug msg=\"something else entirely\"" > "$SPLIT_SAYS"
check "an unrecognisable answer warns about nothing" "" "$(pkdump_store_split_check 2>&1)"

# And prod, which has no store, is not asked at all.
reset_store
PATH="${WORK}/splitbin:${PATH}"
check "no store opted in -> nothing to check" "" "$(pkdump_store_split_check 2>&1)"
unset SPLIT_SAYS

reset_store

# ---------------------------------------------------------------------------
log "8c. And when it is wedged anyway, the job says so and repairs it"
# ---------------------------------------------------------------------------
#
# pkdump_store_netns_repair is a file-stat that runs at activation and answers
# only for a store this shell opted into. pkdump_store_netns_ensure is the other
# half: it asks PODMAN, about whatever store is active — prod's default store
# included — at the moment a job is about to need a user-defined network.
#
#   podman unshare --rootless-netns true
#
# is the whole probe: same setup a container start runs, same error, no image and
# no container. What it may then repair is bounded by who is on the namespace,
# because dropping the netns file cuts every container already on it off from
# everything started afterwards (measured: a fresh container cannot resolve the
# running one at all).

reset_store
mkdir -p "${WORK}/nsbin"
export NS_STATE="${WORK}/ns-state"

# A podman whose rootless-netns probe fails exactly when podman's does: a netns
# file present with no scaffolding behind it. Dropping the file is what puts it
# back on the branch that rebuilds, so the same fake reports the repair.
cat > "${WORK}/nsbin/podman" <<'EOF'
#!/usr/bin/env bash
args=("$@")
while [[ ${#args[@]} -gt 0 && ${args[0]} == --* ]]; do args=("${args[@]:1}"); done
case "${args[0]:-}" in
unshare)
	if [ -e "${NS_STATE}/probe-unrelated" ]; then
		echo "Error: cannot re-exec process to join the existing user namespace"
		exit 125
	fi
	if [ -e "${NS_STATE}/wedged-forever" ] || [ -e "${NS_NETNS_FILE}" ]; then
		echo "Error: failed to mount runtime directory for rootless netns: no such file or directory"
		exit 125
	fi
	exit 0
	;;
ps) cat "${NS_STATE}/ps" 2>/dev/null; exit 0 ;;
info) cat "${NS_STATE}/graph" 2>/dev/null; exit 0 ;;
restart) echo "${args[1]}" >> "${NS_STATE}/podman-restarts"; exit 0 ;;
esac
exit 0
EOF
chmod +x "${WORK}/nsbin/podman"
# systemd is real on the box this runs on, and the repair restarts units by name.
# A test that reached the real one would restart whatever it named.
cat > "${WORK}/nsbin/systemctl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do case "$a" in *.service) echo "$a" >> "${NS_STATE}/systemctl-restarts" ;; esac; done
exit 0
EOF
chmod +x "${WORK}/nsbin/systemctl"
PATH="${WORK}/nsbin:${PATH}"

ns_case() { # ns_case <name> — a fresh state dir and a fresh netns file
	rm -rf "$NS_STATE"
	mkdir -p "$NS_STATE"
	printf '%s\n' "${WORK}/${1}/storage" > "${NS_STATE}/graph"
	export NS_NETNS_FILE="${NETNS_DIR}/$(pkdump_store_netns_name "${WORK}/${1}/storage")"
	: > "$NS_NETNS_FILE"
}

# Nothing wrong: the probe passes and the function is a tenth of a second and no
# output. This is every night the box is healthy.
ns_case healthy
rm -f "$NS_NETNS_FILE"
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod 2>&1)"
ENS_RC=$?
set -e
check "a working store passes silently" "0" "$ENS_RC"
check "and says nothing" "" "$ENS_OUT"

# A failure that is not this one — no rootless podman at all, a dead pause
# process — is not ours to interpret. The run itself reports it in its own words.
ns_case unrelated
: > "${NS_STATE}/probe-unrelated"
set +e
pkdump_store_netns_ensure pkdump-lake-prod >/dev/null 2>&1
ENS_RC=$?
set -e
check "an unrelated podman failure is not claimed" "0" "$ENS_RC"
check "and the netns is left alone" "present" \
	"$([ -e "$NS_NETNS_FILE" ] && echo present || echo gone)"

# The refusal, and it is the important one. The default store is prod's, and on a
# box where prod shares it with unrelated projects their containers are on that
# namespace too. A nightly job may not restart another project's service to get
# its own work done.
ns_case foreign
printf '%s\n' "household mtgc-net" "pkdump-nessie-prod pkdump-lake-prod" > "${NS_STATE}/ps"
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod 2>&1)"
ENS_RC=$?
set -e
check "someone else's container -> refuses" "1" "$ENS_RC"
check "and names it" "1" "$(printf '%s' "$ENS_OUT" | grep -c 'household' || true)"
check "leaving the namespace up" "present" \
	"$([ -e "$NS_NETNS_FILE" ] && echo present || echo gone)"
check "and telling the operator how to do it by hand" "1" \
	"$(printf '%s' "$ENS_OUT" | grep -c 'systemctl --user restart' || true)"

# What the caller passes to say "the network is usable again". Counting the calls
# is how the polling is observed: asking once and asking until it holds are the
# same function with the same answer, and only the second one is the fix.
READY_SUCCEED_AFTER=0
ready_probe() {
	local n
	n=$(($(cat "${NS_STATE}/ready-calls" 2>/dev/null || echo 0) + 1))
	echo "$n" > "${NS_STATE}/ready-calls"
	[ "$n" -gt "$READY_SUCCEED_AFTER" ]
}
ready_calls() { cat "${NS_STATE}/ready-calls" 2>/dev/null || echo 0; }
# A bounded wait, made cheap: the production numbers (120s / 3s) are sized for a
# JVM, and this asserts the loop, not the clock.
export PKDUMP_NETNS_READY_TIMEOUT=5 PKDUMP_NETNS_READY_INTERVAL=0.1

# Only ours on it: the repair is in scope, and finishing it means restarting them
# — they are on the old namespace and would be unreachable otherwise.
ns_case ours
printf '%s\n' "pkdump-nessie-prod pkdump-lake-prod" > "${NS_STATE}/ps"
READY_SUCCEED_AFTER=0
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod ready_probe 2>&1)"
ENS_RC=$?
set -e
check "only our containers -> repairs" "0" "$ENS_RC"
check "the stale netns is dropped" "gone" \
	"$([ -e "$NS_NETNS_FILE" ] && echo present || echo gone)"
# By unit, not by container: a Quadlet container is named by its unit, and
# restarting it behind systemd's back leaves the unit's idea of it wrong.
check "and what was on it is restarted, by unit" "pkdump-nessie-prod.service" \
	"$(cat "${NS_STATE}/systemctl-restarts" 2>/dev/null || echo none)"

# ---------------------------------------------------------------------------
# pd-p39v. THE REPAIR IS NOT FINISHED WHEN THE RESTART RETURNS.
#
# `systemctl --user restart` returns when the CONTAINER is running. Nessie is a
# JVM and does not answer for another 30-40s, so the repair above used to print
# "Rootless networking repaired" and hand a still-booting catalog to the job that
# asked for it. The job died on a connection error; the next night's run found
# everything healthy. A unit without SuccessExitStatus= paged for a condition
# that had already fixed itself — the false page this repo has now paid for three
# times.
#
# So a repair that RESTARTED something waits for it to ANSWER, and the caller
# says what answering means.

# It is a POLL, not a question asked once. The catalog is not up on the first
# ask — that is the entire scenario — so a fix that checks and gives up is the
# bug with an extra line in it.
ns_case slow-catalog
printf '%s\n' "pkdump-nessie-prod pkdump-lake-prod" > "${NS_STATE}/ps"
READY_SUCCEED_AFTER=3
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod ready_probe 2>&1)"
ENS_RC=$?
set -e
check "a catalog that is still booting -> waits for it" "0" "$ENS_RC"
check "and kept asking until it answered" "4" "$(ready_calls)"
check "saying the repair is complete, not merely started" "1" \
	"$(printf '%s' "$ENS_OUT" | grep -c 'answers again' || true)"

# The wait is BOUNDED, and running out is a FAILURE. Returning success here is
# exactly what produced the false page: the caller starts a job against a service
# that is not there, and reports a fault in the job.
ns_case dead-catalog
printf '%s\n' "pkdump-nessie-prod pkdump-lake-prod" > "${NS_STATE}/ps"
READY_SUCCEED_AFTER=999999
PKDUMP_NETNS_READY_TIMEOUT=1
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod ready_probe 2>&1)"
ENS_RC=$?
set -e
PKDUMP_NETNS_READY_TIMEOUT=5
check "a catalog that never answers -> fails the repair" "1" "$ENS_RC"
check "rather than hanging" "1" \
	"$(printf '%s' "$ENS_OUT" | grep -c 'never answered within' || true)"
# Named in the failure itself, not only in the "Restarting …" line above it: the
# operator reads the last thing the unit said.
check "and names what did not come back" "1" \
	"$(printf '%s\n' "$ENS_OUT" | grep -c '^    pkdump-nessie-prod$' || true)"

# A caller that restarts something and cannot confirm it came back is refused.
# Defaulting to "proceed" would be a silent second copy of this bug, in whatever
# script forgot the argument, found the same way — a page at 07:00.
ns_case unverifiable
printf '%s\n' "pkdump-nessie-prod pkdump-lake-prod" > "${NS_STATE}/ps"
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod 2>&1)"
ENS_RC=$?
set -e
check "a restart with no readiness command -> refuses" "1" "$ENS_RC"
check "and says a repair it cannot confirm is not COMPLETE" "1" \
	"$(printf '%s' "$ENS_OUT" | grep -c 'cannot be reported COMPLETE' || true)"

# Nothing running at all — the usual case for a nightly job on a quiet box. And
# nothing was restarted, so there is nothing to wait for: the caller's own
# container is what rebuilds the namespace. The readiness command is handed over
# anyway and must go unused, or every quiet night pays for a wait it does not
# need. READY_SUCCEED_AFTER is set to never so a call would also FAIL the run,
# not merely be counted.
ns_case empty
: > "${NS_STATE}/ps"
READY_SUCCEED_AFTER=999999
set +e
pkdump_store_netns_ensure pkdump-lake-prod ready_probe >/dev/null 2>&1
ENS_RC=$?
set -e
check "nothing on it -> just rebuilt" "0" "$ENS_RC"
check "netns dropped" "gone" \
	"$([ -e "$NS_NETNS_FILE" ] && echo present || echo gone)"
check "and nothing restarted" "none" \
	"$(cat "${NS_STATE}/systemctl-restarts" 2>/dev/null || echo none)"
check "and no readiness wait was paid for" "0" "$(ready_calls)"
READY_SUCCEED_AFTER=0

# The repair not working is a failure, not a shrug: the job that called this
# cannot run, and saying so here is the difference between a named cause and an
# unexplained mount error 40 lines into a podman run.
ns_case stuck
: > "${NS_STATE}/ps"
: > "${NS_STATE}/wedged-forever"
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod 2>&1)"
ENS_RC=$?
set -e
check "a namespace that does not come back fails" "1" "$ENS_RC"
check "and says so" "1" \
	"$(printf '%s' "$ENS_OUT" | grep -c 'did not come back' || true)"

# Podman is the authority on where the active store's graph root is — the one
# thing this cannot derive, since it runs against stores it did not activate.
# With no answer it stops rather than guessing at a path it would then rm.
ns_case unanswerable
: > "${NS_STATE}/ps"
: > "${NS_STATE}/graph"
set +e
ENS_OUT="$(pkdump_store_netns_ensure pkdump-lake-prod 2>&1)"
ENS_RC=$?
set -e
check "no graph root -> fails without guessing" "1" "$ENS_RC"
check "and says it is not guessing" "1" \
	"$(printf '%s' "$ENS_OUT" | grep -c 'not guessing' || true)"
check "and removed nothing" "present" \
	"$([ -e "$NS_NETNS_FILE" ] && echo present || echo gone)"

# The two jobs that run on a user-defined network are the two that call it. A
# wrapper that starts a container on the lake network without this guard reads a
# bare mount error from podman and reports nothing anyone can act on.
check "the transform tier checks before it runs" "1" \
	"$(grep -c 'pkdump_store_netns_ensure' "${REPO_DIR}/deploy/value-snapshots.sh" || true)"
check "the price build too" "1" \
	"$(grep -c 'pkdump_store_netns_ensure' "${REPO_DIR}/deploy/prices.sh" || true)"

# And each hands it a readiness command, which is the half a repair cannot supply
# for itself. Without one the guard is back to reporting a restart it never
# confirmed, and the unit pages on a condition that healed before anyone read the
# alert (pd-p39v).
check "the transform tier says what answering means" "1" \
	"$(grep -c 'pkdump_lake_catalog_answering "\$NETWORK" "\$JOB_IMAGE"' \
		"${REPO_DIR}/deploy/value-snapshots.sh" || true)"
check "the price build too" "1" \
	"$(grep -c 'pkdump_lake_catalog_answering "\$NETWORK" "\$JOB_IMAGE"' \
		"${REPO_DIR}/deploy/prices.sh" || true)"

# The probe itself, and it is defined ONCE. Two wrappers around one network with
# two copies of its address is how one of them drifts, and this copy runs only
# during a wedge — the least-exercised path there is.
# shellcheck source=deploy/lake-lib.sh
. "${REPO_DIR}/deploy/lake-lib.sh"
check "the catalog URI is the container name setup-lake installs" \
	"http://pkdump-nessie-prod:19120/iceberg/" "$(pkdump_lake_catalog_uri prod)"
check "and lake.env redirects it" "http://nessie-x:19120/iceberg/" \
	"$(PKDUMP_LAKE_NESSIE_URI=http://nessie-x:19120/iceberg/ pkdump_lake_catalog_uri prod)"
# DERIVED from the catalog URI rather than built from the instance a second time:
# a gate that points the job at its own Nessie must not leave the repair waiting
# on prod's.
check "the health URL follows the catalog" "http://nessie-x:19120/api/v2/config" \
	"$(pkdump_lake_health_url http://nessie-x:19120/iceberg/)"
# Asked from a container ON the network, not from the host: the published-port
# path can answer yes about a network the job itself cannot use.
check "asked from where the job will ask" "1" \
	"$(grep -c 'podman run --rm --network "\$1" "\$2"' \
		"${REPO_DIR}/deploy/lake-lib.sh" || true)"

unset NS_NETNS_FILE NS_STATE PKDUMP_NETNS_READY_TIMEOUT PKDUMP_NETNS_READY_INTERVAL
rm -f "${NETNS_DIR}"/rootless-netns-*
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

# …and after the shipment AND its read-back (pd-i08u). This is the line that
# makes the job runnable at all: the online holdings read is deleted, so the
# transform values a collection out of `zone_holdings`, and the second half of
# deploy/ship.sh is the only thing that writes it. Ordered the other way, every
# tenant is refused for want of a staging table — or, worse, valued at whatever
# the last read-back happened to leave behind.
check "ordered after the shipment that fills zone_holdings" "1" \
	"$(grep -c '^After=pkdump-ship@%i.service$' "$VS_SVC" || true)"
check "does not pull the shipment in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-ship@%i.service$' "$VS_SVC" || true)"
# The direction REVERSED in pd-i08u, and the old line has to be gone from the
# other file or systemd has a cycle: two units each ordered after the other is
# a dependency loop, and systemd resolves one by dropping a job rather than by
# failing loudly.
check "the shipment is no longer ordered after this unit" "0" \
	"$(grep -c '^After=pkdump-value-snapshots@%i.service$' "${REPO_DIR}/deploy/pkdump-ship.service" || true)"

# The calendar entry is DERIVED from the unit before it in the chain rather
# than guessed at: last possible start + the time that unit is allowed to take.
# Since pd-i08u that predecessor is the shipment, not the refresh — the two
# swapped places. Move either number in pkdump-ship.* and this fails rather
# than silently leaving the two jobs overlapping.
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
SHIP_LATEST=$((
	$(hhmm_secs "${REPO_DIR}/deploy/pkdump-ship.timer") +
		$(key_secs "${REPO_DIR}/deploy/pkdump-ship.timer" RandomizedDelaySec) +
		$(key_secs "${REPO_DIR}/deploy/pkdump-ship.service" TimeoutStartSec)
))
VS_START=$(($(hhmm_secs "$VS_TMR") + $(key_secs "$VS_TMR" RandomizedDelaySec)))
check "fires no earlier than the refresh can finish" "ok" \
	"$([ "$VS_START" -ge "$REFRESH_LATEST" ] && echo ok || echo "starts ${VS_START}s < refresh ${REFRESH_LATEST}s")"
check "…nor earlier than the shipment can finish" "ok" \
	"$([ "$VS_START" -ge "$SHIP_LATEST" ] && echo ok || echo "starts ${VS_START}s < ship ${SHIP_LATEST}s")"

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
# skip — the same rule every other job in the chain follows.
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
# The scope is a sha1 of the checkout path (pd-sjn7), so the expectation is
# computed the same way rather than hard-coded — a literal here would pass on
# one machine and fail on every other checkout, which is the opposite of the
# property being asserted.
IMG_SCOPE="$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)"
check "unset -> builds from this checkout's Containerfile" \
	"PODMAN: build -t pkdump:x --build-arg CARGO_TARGET_CACHE_SCOPE=${IMG_SCOPE} -f ${REPO_DIR}/Containerfile ${REPO_DIR}" \
	"$(img_run pkdump:x)"
# …and the scope is this checkout's, not a constant. A build arg that never
# varied would satisfy the line above and share one cargo target cache with
# every other tree on the box, which is the bug it exists to close.
check "…scoping the cargo target cache to THIS checkout" "yes" \
	"$([[ "$IMG_SCOPE" == "$(printf '%s' "${REPO_DIR}-other" | sha1sum | cut -c1-8)" ]] && echo no || echo yes)"

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
#
# DIRECTIVES ONLY, both of them. Each unit's comment block explains itself by
# naming the other one — that is the point of the split and the first thing a
# reader needs — and a check that greps the prose would forbid the explanation
# rather than the behaviour. What must not appear is a directive.
check "the landing unit does not derive from raw" "0" \
	"$(grep -v '^#' "${REPO_DIR}/deploy/pkdump-refresh.service" | grep -c 'pkdump-lake-derive' || true)"
# Comments excluded on BOTH sides: since pd-lunn each unit's block explains the
# split by naming the other one, and since pd-llbq the derive's block explains
# its exit statuses by comparing them to the refresh's. Naming the other unit in
# prose is not the same as running it.
check "the derive unit does not fetch upstream" "0" \
	"$(grep -v '^#' "$DV_SVC" | grep -c 'data refresh' || true)"

# The ordering, which is the guarantee: this unit reads the partition the
# landing one wrote, so it may never run beside it.
check "ordered after the landing" "1" \
	"$(grep -c '^After=pkdump-refresh@%i.service$' "$DV_SVC" || true)"
# And NOT Wants=: the landing is a oneshot without RemainAfterExit, so pulling it
# in would re-run the whole catalog fetch a second time every night.
check "does not pull the landing in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-refresh@%i.service$' "$DV_SVC" || true)"
# …and the transform runs after the derive, so the nightly chain is total.
check "the transform is ordered after the derive" "1" \
	"$(grep -c '^After=pkdump-derive@%i.service$' "${REPO_DIR}/deploy/pkdump-value-snapshots.service" || true)"

# There IS one partial success for a catalog, and exactly one (pd-llbq): a
# partition short only in the pokemontcg.io tail. `pkdump data refresh` answers
# that same night with exit 2 and a stale set list (pd-nons), and this unit used
# to answer it with a refusal and a page — two units taking opposite policies on
# one upstream's weather. Everything else is still exit 1: a short TCGCSV prefix
# is the quietly-smaller catalog the original comment was written about.
check "a partial night is a success, not a page" "1" \
	"$(grep -c '^SuccessExitStatus=2$' "$DV_SVC" || true)"
check "and 2 is the ONLY status that is" "1" \
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
# scheduler names the date" and "no lake.env refuses by name" are behaviour,
# not greps.

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
# …and a PARTIAL message about a hand-run rebuild names THAT night, not today's.
# A wrapper that reconstructed the date from the clock would say the wrong one
# in the one situation an operator is reading it most carefully.
check "a partial rebuild of an older date names that date" "1" \
	"$(PKDUMP_TEST_JOB_RC=2 run_derive --ingest-date=2026-08-09 |
		grep -c 'PARTIAL — 2026-08-09 did not complete' || true)"

# The wrapper no longer reads the job's output at all. Item 4 made a gap in
# raw/ a refusal inside the job, so the "correct catalog, unreproducible
# lineage" outcome the old coverage warning existed for cannot occur — that
# shape now exits 1 and pages through OnFailure= like any other failure. A
# wrapper that still grepped for it would be dead code pretending to be a
# safety net.
check "the wrapper greps the job's output for nothing" "0" \
	"$(grep -c 'raw coverage' "${REPO_DIR}/deploy/derive.sh" || true)"

# A PARTIAL night: the catalog WAS rebuilt, from a partition whose tail was
# short. Not a page — the unit's SuccessExitStatus=2 above — and not silent
# either. The status is the whole message; nothing is greped out of the job's
# output to reach it.
DV_PARTIAL="$(PKDUMP_TEST_JOB_RC=2 run_derive)"
check "a partial derive exits 2" "1" "$(printf '%s' "$DV_PARTIAL" | grep -c 'RC=2$' || true)"
check "and says PARTIAL rather than FAILED" "1" \
	"$(printf '%s' "$DV_PARTIAL" | grep -c 'derive: PARTIAL' || true)"
check "and does not claim the catalog was not rebuilt" "0" \
	"$(printf '%s' "$DV_PARTIAL" | grep -c 'derive: FAILED' || true)"
# The warning has to leave the script. alerts.env is deliberately absent in this
# harness, so alert.sh exits non-zero and the wrapper says the warning reached
# nobody — which is the assertion that it tried at all rather than skipping.
check "the partial warning is pushed, not swallowed" "1" \
	"$(printf '%s' "$DV_PARTIAL" | grep -c 'PARTIAL warning reached nobody' || true)"
check "…by the shared alert sink, naming the instance" "1" \
	"$(printf '%s' "$DV_PARTIAL" | grep -c 'derive PARTIAL (dvtest)' || true)"
# …and an unconfigured alert channel must not turn a partial night into a
# failure. `set -euo pipefail` plus a bare alert.sh call would do exactly that.
check "an unreachable channel does not change the status" "1" \
	"$(printf '%s' "$DV_PARTIAL" | grep -c 'RC=2$' || true)"
# The retired flag is gone from the runbook line too — an invocation carrying
# it would now be rejected by clap rather than quietly accepted.
check "the retired flag is not documented as usable" "0" \
	"$(grep -c -- '--no-upstream-fallback' "${REPO_DIR}/deploy/derive.sh" || true)"

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

log "13. The landing half lands, and cannot quietly land nothing (pd-kncd, pd-lunn)"
# ---------------------------------------------------------------------------
#
# The unit was `podman exec systemd-pkdump-%i pkdump data refresh`, and
# `podman exec` does NOT forward the calling process's environment — the exec'd
# process gets the CONTAINER's env plus explicit -e flags and nothing else.
# Measured (pd-vk22):
#
#   $ PKDUMP_LAND_RAW=1 podman exec systemd-pkdump-mutant \
#         sh -c 'echo ${PKDUMP_LAND_RAW:-<unset>}'
#   <unset>
#
# So the documented way to turn landing on — a drop-in setting
# Environment=PKDUMP_LAND_RAW=1 on the refresh unit — produced a green nightly
# timer that landed nothing, with no error anywhere. That is the exact state
# deploy/LAKE.md §3 promises cannot exist, so this section is that promise
# holding still: the settings are FORWARDED, the credentials are MOUNTED, and
# every way of getting it wrong is loud.
#
# pd-lunn removed the switch the original bug rode in on. Landing is not opt-in
# any more — the refresh derives nothing, so a run that does not land does
# nothing at all — which means `lake.env` is REQUIRED and "landed nothing" is
# never a legitimate night. And it added the cutover's own failure mode, which
# is the last two checks here: landing with the DERIVE TIMER OFF is a box that
# is green every night and serves a catalog frozen at the day of the upgrade.

reset_store

RF_SVC="${REPO_DIR}/deploy/pkdump-refresh.service"

# The call that drops the environment must not come back. Stated as an
# assertion rather than left to the wrapper's tests, because this is the whole
# defect: an ExecStart that reaches into the running server's container cannot
# carry an environment OR a mount, however the wrapper beside it is written.
# Directives only — the unit's own comment block quotes the old ExecStart, which
# is how the next person finds out why it is not there any more.
check "the unit does not exec into the running server" "0" \
	"$(grep -v '^#' "$RF_SVC" | grep -c 'podman exec' || true)"
check "it runs the wrapper from this checkout" "1" \
	"$(grep -c '^ExecStart=.*{{REPO_DIR}}/deploy/refresh.sh %i' "$RF_SVC" || true)"
# …and §12's split still holds: the landing half lands, it does not derive.
check "the landing unit still does not derive from raw" "0" \
	"$(grep -v '^#' "$RF_SVC" | grep -c 'pkdump-lake-derive' || true)"
# A catalog has no partial success — the same asymmetry with the transform tier
# deploy/derive.sh documents.
check "no exit status is silently a success" "0" \
	"$(grep -c '^SuccessExitStatus=' "$RF_SVC" || true)"
check "a failure pages" "1" \
	"$(grep -c '^OnFailure=pkdump-alert@%n.service$' "$RF_SVC" || true)"
# The timer's bound is what §10 and §12 derive their calendar entries from, so
# the unit that carries it must keep carrying it.
check "the run is still bounded" "1" \
	"$(grep -c '^TimeoutStartSec=1800$' "$RF_SVC" || true)"

# --- What the wrapper actually does -----------------------------------------
# Driven end to end with a fake podman that records its own argv, because "the
# variable reaches the container" is a claim about a command line.

RF_HOME="${WORK}/rfhome"
mkdir -p "${RF_HOME}/.config/pkdump/rftest/aws" "${WORK}/rfbin"
printf 'PKDUMP_LAKE_S3_BUCKET=pdtest\nPKDUMP_LAKE_S3_REGION=us-west-2\nAWS_PROFILE=pkdump-lake\n' \
	> "${RF_HOME}/.config/pkdump/lake.env"
# The instance's assume-role profile — the non-secret half of the credential
# pair. The secret half is the fake podman's `secret inspect` above.
printf '[profile pkdump-lake]\nrole_arn = arn:aws:iam::0:role/x\nsource_profile = bootstrap\n' \
	> "${RF_HOME}/.config/pkdump/rftest/aws/config"

# `secret inspect` answers for the instance's bootstrap secret, `run` replays a
# canned refresh transcript — including the line landing::open prints before the
# first fetch, which is the wrapper's evidence that the flag survived the trip.
cat > "${WORK}/rfbin/podman" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"secret inspect") exit "${PKDUMP_TEST_NO_SECRET:-0}" ;;
"image exists") exit "${PKDUMP_TEST_NO_IMAGE:-0}" ;;
esac
if [ "$1" = run ]; then
	printf 'PODMAN RUN: %s\n' "$*"
	# Every run lands now, so the line landing::require prints before the first
	# fetch is unconditional — which is what makes its ABSENCE a wiring failure
	# rather than an ordinary non-landing night.
	[ -n "${PKDUMP_TEST_LANDING_LOST:-}" ] ||
		printf 'Landing raw upstream responses in s3://pdtest (ingest_date=2026-08-13)\n'
	printf 'Refresh complete: landed, not derived.\n'
	exit "${PKDUMP_TEST_JOB_RC:-0}"
fi
exit 0
EOF
chmod +x "${WORK}/rfbin/podman"

# The derive timer's state, which the wrapper now refuses to run without
# (pd-lunn). A fake systemctl rather than a skip flag in the script: the branch
# under test is the real one, and a check a harness can switch off is a check
# production can be missing.
cat > "${WORK}/rfbin/systemctl" <<'EOF'
#!/usr/bin/env bash
if [ "$1 $2" = "--user is-enabled" ]; then
	printf '%s\n' "${PKDUMP_TEST_DERIVE_TIMER:-enabled}"
	[ "${PKDUMP_TEST_DERIVE_TIMER:-enabled}" = enabled ] || exit 1
	exit 0
fi
exit 0
EOF
chmod +x "${WORK}/rfbin/systemctl"

run_refresh() { # run_refresh [args...] — env overrides come from the caller
	set +e
	PATH="${WORK}/rfbin:${ORIG_PATH}" HOME="$RF_HOME" \
		PKDUMP_LAKE_ENV="${RF_HOME}/.config/pkdump/lake.env" \
		bash "${REPO_DIR}/deploy/refresh.sh" rftest "$@" 2>&1
	printf 'RC=%s' "$?"
	set -e
}

RF_ENV="$(run_refresh)"
check "a refresh exits 0" "1" "$(printf '%s' "$RF_ENV" | grep -c 'RC=0$' || true)"
check "and runs the app image's pkdump, not its entrypoint" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '--entrypoint pkdump .* data refresh' || true)"
# The app container's own unit sets this; a fresh container that did not would
# read a catalog under ~/.pkdump that has nothing in it.
check "and names the data dir" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '-e PKDUMP_HOME=/data' || true)"
check "…and the run says it opened a landing zone" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c '^Landing raw upstream responses in ' || true)"
# The switch the original bug rode in on is gone (pd-lunn): landing is what the
# command IS, so there is no variable to forward and no flag to pass. A wrapper
# that grew either back would be re-introducing a way to run this job and land
# nothing.
check "no landing flag is forwarded any more" "0" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '-e PKDUMP_LAND_RAW=1' || true)"
check "and the wrapper does not mention one" "0" \
	"$(grep -v '^#' "${REPO_DIR}/deploy/refresh.sh" | grep -c 'PKDUMP_LAND_RAW\|land-raw' || true)"

# The bucket the run is useless without (pd-8gjd): the app container had no
# lake settings at all, so even a forwarded flag would have refused inside.
check "the lake's settings are forwarded" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '-e PKDUMP_LAKE_S3_BUCKET=pdtest' || true)"
check "…region included" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '-e PKDUMP_LAKE_S3_REGION=us-west-2' || true)"
# The LAKE profile, not the backup one — separating the blast radius is the
# whole reason there are two buckets.
check "…and the profile lake.env names" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '-e AWS_PROFILE=pkdump-lake' || true)"
# The retry budget the unit documents as a drop-in. Same trip, same failure
# shape as pd-vk22: a drop-in sets it in the WRAPPER's environment, and a
# wrapper that does not forward it leaves the unit documenting a knob that
# turns nothing.
check "the retry budget reaches the container" "1" \
	"$(printf '%s' "$(PKDUMP_HTTP_RETRY_ATTEMPTS=7 run_refresh)" |
		grep -c -- '-e PKDUMP_HTTP_RETRY_ATTEMPTS=7' || true)"
check "…and so does its base delay" "1" \
	"$(printf '%s' "$(PKDUMP_HTTP_RETRY_BASE_MS=25 run_refresh)" |
		grep -c -- '-e PKDUMP_HTTP_RETRY_BASE_MS=25' || true)"
# Unset stays unset — the binary's own default is 4 attempts at 500ms, and a
# wrapper that passed an empty value would override it with nothing.
check "unset is not forwarded as empty" "0" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '-e PKDUMP_HTTP_RETRY_ATTEMPTS' || true)"

# Credentials are the other half of pd-8gjd, and the assume-role path is the
# project's standing decision: a role profile as a file, the bootstrap key as a
# podman secret, never a long-lived static key on the command line.
check "credentials are mounted when the instance has them" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- '--secret pkdump-rftest-s3-bootstrap,type=mount,target=/aws/credentials' || true)"
check "…with the role profile beside them" "1" \
	"$(printf '%s' "$RF_ENV" | grep -c -- "-v ${RF_HOME}/.config/pkdump/rftest/aws/config:/aws/config:ro" || true)"

# --- Every way to get it wrong, loudly --------------------------------------

# A box with no lake configured at all. This used to be a legitimate state —
# landing was opt-in, and a refresh nobody asked to land refreshed anyway.
# Since pd-lunn it is not: the catalog is built from raw/, so a run with nowhere
# to land has nothing to do, and doing it quietly would be the original bug with
# the flag removed.
set +e
RF_NOLAKE="$(PATH="${WORK}/rfbin:${ORIG_PATH}" HOME="$RF_HOME" \
	PKDUMP_LAKE_ENV="${WORK}/nosuch-refresh.env" \
	bash "${REPO_DIR}/deploy/refresh.sh" rftest 2>&1)"
RF_NOLAKE_RC=$?
set -e
check "no lake.env -> refuses" "1" "$RF_NOLAKE_RC"
check "and names the file to write" "1" \
	"$(printf '%s' "$RF_NOLAKE" | grep -c 'nosuch-refresh.env does not exist' || true)"
check "and lands nothing" "0" \
	"$(printf '%s' "$RF_NOLAKE" | grep -c '^PODMAN RUN' || true)"

# A lake.env that exists and configures nothing the code reads — pd-ub8n, the
# one that actually happened: the file on the box spelled the keys
# PKDUMP_LAKE_BUCKET / _REGION / _RAW_PREFIX, which nothing reads. The binary
# refuses on this too, but from inside the container, where its message names
# /root/.config/pkdump/lake.env — a path that exists on neither side. The host
# is where the file is, so the host is where the refusal has to be able to name
# it.
printf 'PKDUMP_LAKE_BUCKET=pdtest\nPKDUMP_LAKE_REGION=us-west-2\n' \
	> "${RF_HOME}/.config/pkdump/lake-oldnames.env"
set +e
RF_OLDNAMES="$(PATH="${WORK}/rfbin:${ORIG_PATH}" HOME="$RF_HOME" \
	PKDUMP_LAKE_ENV="${RF_HOME}/.config/pkdump/lake-oldnames.env" \
	bash "${REPO_DIR}/deploy/refresh.sh" rftest 2>&1)"
RF_OLDNAMES_RC=$?
set -e
check "a lake.env the code cannot read -> refuses" "1" "$RF_OLDNAMES_RC"
check "and names the host file, not the container's" "1" \
	"$(printf '%s' "$RF_OLDNAMES" | grep -c "${RF_HOME}/.config/pkdump/lake-oldnames.env" || true)"
check "and names the two variables it wanted" "1" \
	"$(printf '%s' "$RF_OLDNAMES" | grep -c 'PKDUMP_LAKE_S3_BUCKET and PKDUMP_LAKE_S3_REGION' || true)"
check "and lands nothing" "0" \
	"$(printf '%s' "$RF_OLDNAMES" | grep -c '^PODMAN RUN' || true)"
# …and it is a refusal, not an alias table: teaching the wrapper to accept the
# old spellings is the fallback logic the No-Fallback convention forbids, and a
# half-configured lake that half-works is worse than one that stops.
# Named in the refusal's prose, expanded nowhere — a `$` in front of one of
# them would be the alias arriving.
check "the old spellings are never expanded" "0" \
	"$(grep -cE '\$\{?PKDUMP_LAKE_(BUCKET|REGION|RAW_PREFIX|TABLE_PREFIX)' \
		"${REPO_DIR}/deploy/refresh.sh" || true)"
# Landing into real S3 with no credentials mounted. Not a refusal — an
# endpoint-backed stand-in can be reached with keys already in the environment,
# and the run fails at its first PUT either way — but "AccessDenied on
# part-0000" is a far worse first clue than a sentence naming the two files.
RF_NOCRED="$(PKDUMP_TEST_NO_SECRET=1 run_refresh)"
check "landing with no credentials says so" "1" \
	"$(printf '%s' "$RF_NOCRED" | grep -c 'no credentials mounted' || true)"
check "…naming the secret it wanted" "1" \
	"$(printf '%s' "$RF_NOCRED" | grep -c 'pkdump-rftest-s3-bootstrap' || true)"
check "…and does not mount a half-set" "0" \
	"$(printf '%s' "$RF_NOCRED" | grep -c -- '--secret' || true)"
# A directory-backed lake is the hermetic test tier's substrate and needs no
# credentials at all, so it gets no warning.
check "a directory-backed lake needs no credentials" "0" \
	"$(PKDUMP_TEST_NO_SECRET=1 PKDUMP_LAKE_DIR="${WORK}/rawdir" run_refresh |
		grep -c 'no credentials mounted' || true)"

# The silent green no-op itself, simulated at its last remaining hiding place:
# the process runs, exits 0, and never opens a landing zone. The wrapper must
# read that as a wiring failure rather than a successful night. It is
# unconditional now — there is no shape of this job that legitimately lands
# nothing, so there is no case to scope the check to.
RF_LOST="$(PKDUMP_TEST_LANDING_LOST=1 run_refresh)"
check "landed nothing -> fails" "1" \
	"$(printf '%s' "$RF_LOST" | grep -c 'RC=1$' || true)"
check "and says the lake settings never reached the process" "1" \
	"$(printf '%s' "$RF_LOST" | grep -c 'never opened a landing zone' || true)"

RF_FAILED="$(PKDUMP_TEST_JOB_RC=1 run_refresh)"
check "a failed refresh exits 1" "1" "$(printf '%s' "$RF_FAILED" | grep -c 'RC=1$' || true)"
check "and says nothing was landed" "1" \
	"$(printf '%s' "$RF_FAILED" | grep -c 'refresh: FAILED' || true)"

# --- The cutover's own failure mode (pd-lunn) -------------------------------
#
# This job lands; pkdump-derive@ builds. With the inline derive deleted, the two
# are a pair — and the half that goes missing is silent, because the landing
# succeeds and the thing that did not happen has no unit to fail. Every night
# green, every timer healthy, and a catalog frozen at the day of the upgrade.
#
# So it is checked by name, BEFORE anything is fetched: a refusal that costs an
# hour of somebody else's API would be a bad refusal.
RF_NODERIVE="$(PKDUMP_TEST_DERIVE_TIMER=disabled run_refresh)"
check "the derive timer disabled -> the landing run refuses" "1" \
	"$(printf '%s' "$RF_NODERIVE" | grep -c 'RC=1$' || true)"
check "…naming the timer to enable" "1" \
	"$(printf '%s' "$RF_NODERIVE" | grep -c 'systemctl --user enable --now pkdump-derive@rftest.timer' || true)"
check "…and fetches nothing while it is off" "0" \
	"$(printf '%s' "$RF_NODERIVE" | grep -c '^PODMAN RUN' || true)"
# A timer that was never installed is the same answer, not a different one: the
# catalog is not being built either way.
check "a timer that is not installed refuses too" "1" \
	"$(printf '%s' "$(PKDUMP_TEST_DERIVE_TIMER=not-found run_refresh)" | grep -c 'RC=1$' || true)"

# An instance that was never built on this box.
RF_NOIMAGE="$(PKDUMP_TEST_NO_IMAGE=1 run_refresh)"
check "no image -> fails naming the command that builds it" "1" \
	"$(printf '%s' "$RF_NOIMAGE" | grep -c 'RC=1$' || true)"
check "and names setup.sh" "1" \
	"$(printf '%s' "$RF_NOIMAGE" | grep -c 'deploy/setup.sh rftest' || true)"
check "never pulls the image" "1" \
	"$(grep -c '^podman run --rm --pull=never' "${REPO_DIR}/deploy/refresh.sh" || true)"

reset_store

# ---------------------------------------------------------------------------
log "13. The price build is SCHEDULED, and the alarm is on AGE (pd-up36)"
# ---------------------------------------------------------------------------
#
# pd-8m5c scheduled the transform, which values every tenant from
# catalog.prices. Nothing scheduled the job that FILLS catalog.prices — it was
# a hand-run podman invocation. So the nightly snapshot was correct arithmetic
# over whatever day someone last built by hand: advancing every night, with
# nothing anywhere saying the prices had stopped moving.
#
# Two properties, and the second is the one that decays quietly:
#
#   THE CHAIN IS TOTAL — land -> derive -> prices -> ship -> transform, each ordered
#   after the last. A unit missing from the middle is the bug above.
#
#   THE ALARM IS ON AGE, NOT ON COMPLETENESS — `complete` is conservative
#   across datasets, so a pokemontcg.io tail that died marks the prices
#   manifest incomplete on a night when every price fetch succeeded. Failing
#   there would page most nights, and a pager that cries wolf gets ignored
#   (pd-me6h). So the build passes --allow-incomplete and the alarm moves onto
#   how old the newest partition is.

reset_store

PX_SVC="${REPO_DIR}/deploy/pkdump-prices.service"
PX_TMR="${REPO_DIR}/deploy/pkdump-prices.timer"

# The chain, declared rather than assumed from three timers sharing a calendar
# entry. Each link is an ordering dependency on the unit before it.
check "ordered after the landing" "1" \
	"$(grep -c '^After=pkdump-refresh@%i.service$' "$PX_SVC" || true)"
check "ordered after the derive" "1" \
	"$(grep -c '^After=pkdump-derive@%i.service$' "$PX_SVC" || true)"
check "the transform is ordered after the price build" "1" \
	"$(grep -c '^After=pkdump-prices@%i.service$' "${REPO_DIR}/deploy/pkdump-value-snapshots.service" || true)"
# The link pd-i08u added, asserted here too so the chain is checked as a whole
# in one place rather than only from each end: the shipment sits between the
# price build and the transform, because the transform values what its
# read-back writes.
check "the shipment is ordered after the price build" "1" \
	"$(grep -c '^After=pkdump-prices@%i.service$' "${REPO_DIR}/deploy/pkdump-ship.service" || true)"
check "…and the transform after the shipment" "1" \
	"$(grep -c '^After=pkdump-ship@%i.service$' "${REPO_DIR}/deploy/pkdump-value-snapshots.service" || true)"
# And NOT Wants=: the landing is a oneshot without RemainAfterExit, so pulling
# it in would re-run the whole catalog fetch a second time every night.
check "does not pull the landing in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-\(refresh\|derive\)@%i.service$' "$PX_SVC" || true)"

# 0 / 2 / 1 are three answers. 2 means "no partition for today, and the table is
# still fresh" — one missed night, which tomorrow's build fills in. A unit that
# called that a failure would page on a normal flaky night.
check "exit 2 is a success for the unit" "1" \
	"$(grep -c '^SuccessExitStatus=2$' "$PX_SVC" || true)"
check "a stale table still pages" "1" \
	"$(grep -c '^OnFailure=pkdump-alert@%n.service$' "$PX_SVC" || true)"
check "skips a box with no lake config at all" "1" \
	"$(grep -c '^ConditionPathExists=%h/.config/pkdump/lake.env$' "$PX_SVC" || true)"

# The calendar entry is DERIVED from the landing unit's own declared bounds —
# the same computation §10 and §12 make. REFRESH_LATEST is theirs.
PX_START=$(($(hhmm_secs "$PX_TMR") + $(key_secs "$PX_TMR" RandomizedDelaySec)))
check "fires no earlier than the landing can finish" "ok" \
	"$([ "$PX_START" -ge "$REFRESH_LATEST" ] && echo ok || echo "starts ${PX_START}s < landing ${REFRESH_LATEST}s")"
check "catches up a missed run" "1" "$(grep -c '^Persistent=true$' "$PX_TMR" || true)"
check "timer is enablable" "1" "$(grep -c '^WantedBy=timers.target$' "$PX_TMR" || true)"
check "the deploy installs it" "1" \
	"$(grep -c 'pkdump-prices@\.\${ext}' "${REPO_DIR}/deploy/units-lib.sh" || true)"
check "teardown disables the timer" "1" \
	"$(grep -c 'pkdump-prices@\${INSTANCE}.timer' "${REPO_DIR}/deploy/teardown.sh" || true)"

# --- What the wrapper does with each combination ----------------------------
# Driven end to end with a fake podman standing in for both jobs, because "the
# freshness check runs on EVERY path", "a missed day over a fresh table is a
# warning" and "a stale table is an alarm" are behaviour, not greps.

PX_HOME="${WORK}/pxhome"
mkdir -p "${PX_HOME}/.config/pkdump" "${WORK}/pxbin"
printf 'PKDUMP_LAKE_S3_BUCKET=pdtest\nPKDUMP_LAKE_S3_REGION=us-west-2\n' \
	> "${PX_HOME}/.config/pkdump/lake.env"

# Each job's status is dialled independently, and every invocation is logged so
# a test can assert which jobs actually ran and with what arguments.
cat > "${WORK}/pxbin/podman" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"secret inspect") exit 1 ;;
"image exists" | "network exists") exit "${PKDUMP_TEST_NO_LAKEHOUSE:-0}" ;;
esac
if [ "$1" = run ]; then
	echo "JOB: $*" >> "$PKDUMP_TEST_PRICES_LOG"
	case " $* " in
	*" pkdump-lake-build-prices "*)
		printf '==> reading source=tcgcsv dataset=prices\n'
		[ -n "${PKDUMP_TEST_INCOMPLETE_DAY:-}" ] &&
			printf "    provenance {'pkdump.raw-complete': 'false'}\n"
		exit "${PKDUMP_TEST_BUILD_RC:-0}" ;;
	*" pkdump-lake-prices-age "*)
		printf '==> catalog.prices at main: newest partition\n'
		exit "${PKDUMP_TEST_AGE_RC:-0}" ;;
	esac
fi
exit 0
EOF
chmod +x "${WORK}/pxbin/podman"

run_prices() { # run_prices <build rc> <age rc> [extra args...]
	local build_rc="$1" age_rc="$2"
	shift 2
	: > "${WORK}/prices-jobs.log"
	set +e
	# Blanked rather than merely unconfigured: two cases below provoke a real
	# alarm on purpose, and a gate that pages an operator when it works as
	# designed is pd-n0lf. alert.sh reads an empty token as unconfigured.
	PATH="${WORK}/pxbin:${ORIG_PATH}" HOME="$PX_HOME" \
		PKDUMP_LAKE_ENV="${PX_HOME}/.config/pkdump/lake.env" \
		PKDUMP_ALERTS_ENV="${PX_HOME}/.config/pkdump/nonexistent.env" \
		PUSHOVER_TOKEN= PUSHOVER_USER= \
		PKDUMP_TEST_PRICES_LOG="${WORK}/prices-jobs.log" \
		PKDUMP_TEST_BUILD_RC="$build_rc" PKDUMP_TEST_AGE_RC="$age_rc" \
		bash "${REPO_DIR}/deploy/prices.sh" pxtest "$@" 2>&1
	printf 'RC=%s' "$?"
	set -e
}

PX_OK="$(run_prices 0 0)"
check "built and fresh exits 0" "1" "$(printf '%s' "$PX_OK" | grep -c 'RC=0$' || true)"
check "and says both halves happened" "1" \
	"$(printf '%s' "$PX_OK" | grep -c "prices: OK — today's partition built, catalog.prices is fresh" || true)"
# THE property that decays: a freshness check wired only to the failure path
# would run on almost no night, and nobody would notice it had broken. It runs
# on the success path too, where it is also the only thing that asks the TABLE
# rather than believing the job's report of itself.
check "the freshness check runs even when the build succeeded" "1" \
	"$(grep -c 'pkdump-lake-prices-age' "${WORK}/prices-jobs.log" || true)"
# The date the jobs refuse to default from the clock is supplied by the
# scheduler. UTC, because that is what a raw/ ingest_date partition is named in.
check "the wrapper names today's UTC date to the build" "1" \
	"$(grep -c -- "pkdump-lake-build-prices --ingest-date $(date -u +%F)" "${WORK}/prices-jobs.log" || true)"
check "…and measures age against the same day" "1" \
	"$(grep -c -- "pkdump-lake-prices-age --as-of $(date -u +%F)" "${WORK}/prices-jobs.log" || true)"
# The decision this bead had to make, spelled into the command line: build the
# day even when its landing run did not finish, and let the snapshot record it.
check "the build is allowed an incomplete day" "1" \
	"$(grep -c -- '--allow-incomplete' "${WORK}/prices-jobs.log" || true)"
check "the freshness window is passed explicitly" "1" \
	"$(grep -c -- '--max-age-days 2' "${WORK}/prices-jobs.log" || true)"

# An incomplete-but-recent day is NOT an alarm. This is the exact false
# positive pd-me6h already cost a day of; reintroducing it here in a new shape
# would be worse than having no alarm.
PX_INCOMPLETE="$(PKDUMP_TEST_INCOMPLETE_DAY=1 run_prices 0 0)"
check "an incomplete day still exits 0" "1" \
	"$(printf '%s' "$PX_INCOMPLETE" | grep -c 'RC=0$' || true)"
check "…and raises nothing" "0" \
	"$(printf '%s' "$PX_INCOMPLETE" | grep -c 'STALE\|MISSED' || true)"

# A build that produced nothing today, over a table that still holds a recent
# day. Not an outage: yesterday's prices value today's collection, and
# tomorrow's build fills the day in.
PX_MISSED="$(run_prices 1 0)"
check "a missed day over a fresh table exits 2, not 1" "1" \
	"$(printf '%s' "$PX_MISSED" | grep -c 'RC=2$' || true)"
check "and says which day it missed" "1" \
	"$(printf '%s' "$PX_MISSED" | grep -c "MISSED — no partition built for $(date -u +%F)" || true)"
# Not silent either — and an undeliverable warning is reported rather than
# promoted to a failure. No Pushover channel is configured here, which is every
# test instance.
check "an undeliverable warning is not a failure" "1" \
	"$(printf '%s' "$PX_MISSED" | grep -c 'the MISSED warning reached nobody' || true)"

# The alarm the bead exists for: correct arithmetic over prices that stopped
# arriving. Detected by AGE, and it fires whatever the build did.
PX_STALE="$(run_prices 1 3)"
check "a stale table exits 1" "1" "$(printf '%s' "$PX_STALE" | grep -c 'RC=1$' || true)"
check "and says the prices stopped advancing" "1" \
	"$(printf '%s' "$PX_STALE" | grep -c 'prices: STALE' || true)"
# Even on a night the build itself succeeded — a build that writes a day the
# table does not end up holding is exactly the silence being ended.
PX_STALE_BUILT="$(run_prices 0 3)"
check "a stale table pages even after a clean build" "1" \
	"$(printf '%s' "$PX_STALE_BUILT" | grep -c 'RC=1$' || true)"

# Not being able to ask is not the same answer as "fine" — the rule
# deploy/backup-check.sh is built on.
PX_UNASKABLE="$(run_prices 0 1)"
check "an unanswerable freshness check fails" "1" \
	"$(printf '%s' "$PX_UNASKABLE" | grep -c 'RC=1$' || true)"
check "and says the age could not be established" "1" \
	"$(printf '%s' "$PX_UNASKABLE" | grep -c 'could not establish the age' || true)"

# An explicit date wins — this is how rebuilding an older day runs through the
# same path.
: > "${WORK}/prices-jobs.log"
PATH="${WORK}/pxbin:${ORIG_PATH}" HOME="$PX_HOME" \
	PKDUMP_LAKE_ENV="${PX_HOME}/.config/pkdump/lake.env" \
	PKDUMP_TEST_PRICES_LOG="${WORK}/prices-jobs.log" \
	bash "${REPO_DIR}/deploy/prices.sh" pxtest --ingest-date 2026-08-09 >/dev/null 2>&1
check "an explicit --ingest-date is not overridden" "1" \
	"$(grep -c -- '--ingest-date 2026-08-09' "${WORK}/prices-jobs.log" || true)"
check "…and is not doubled" "0" \
	"$(grep -c -- "--ingest-date $(date -u +%F)" "${WORK}/prices-jobs.log" || true)"
# …and a failure names the day that was actually asked for. A journal line that
# says "today" about a rebuild of last Tuesday sends the reader to the wrong
# partition.
check "a missed rebuild names the day it was asked for" "1" \
	"$(run_prices 1 0 --ingest-date 2026-08-09 | grep -c 'MISSED — no partition built for 2026-08-09' || true)"
# The freshness question is always about NOW, though: rebuilding an old day
# says nothing about whether the table has stopped advancing.
check "…while age is still measured against today" "1" \
	"$(grep -c -- "--as-of $(date -u +%F)" "${WORK}/prices-jobs.log" || true)"

# The window is host config, like every other threshold on this box.
: > "${WORK}/prices-jobs.log"
PATH="${WORK}/pxbin:${ORIG_PATH}" HOME="$PX_HOME" \
	PKDUMP_LAKE_ENV="${PX_HOME}/.config/pkdump/lake.env" \
	PKDUMP_TEST_PRICES_LOG="${WORK}/prices-jobs.log" \
	PKDUMP_LAKE_PRICES_MAX_AGE_DAYS=5 \
	bash "${REPO_DIR}/deploy/prices.sh" pxtest >/dev/null 2>&1
check "the freshness window is overridable per box" "1" \
	"$(grep -c -- '--max-age-days 5' "${WORK}/prices-jobs.log" || true)"

# No lake configured is a refusal that names the file, never a silent skip.
set +e
PX_NOLAKE="$(PATH="${WORK}/pxbin:${ORIG_PATH}" HOME="$PX_HOME" \
	PKDUMP_LAKE_ENV="${WORK}/nosuch-prices.env" \
	PKDUMP_TEST_PRICES_LOG="${WORK}/prices-jobs.log" \
	bash "${REPO_DIR}/deploy/prices.sh" pxtest 2>&1)"
PX_NOLAKE_RC=$?
set -e
check "no lake.env -> refuses" "1" "$PX_NOLAKE_RC"
check "and names the file" "1" \
	"$(printf '%s' "$PX_NOLAKE" | grep -c 'nosuch-prices.env does not exist' || true)"

# An instance whose lakehouse was never installed. A timer armed at nothing is a
# price table that never advances, so this fails rather than skipping.
set +e
PX_NOLAKEHOUSE="$(PATH="${WORK}/pxbin:${ORIG_PATH}" HOME="$PX_HOME" \
	PKDUMP_LAKE_ENV="${PX_HOME}/.config/pkdump/lake.env" \
	PKDUMP_TEST_PRICES_LOG="${WORK}/prices-jobs.log" \
	PKDUMP_TEST_NO_LAKEHOUSE=1 \
	bash "${REPO_DIR}/deploy/prices.sh" pxtest 2>&1)"
PX_NOLAKEHOUSE_RC=$?
set -e
check "no job image -> fails" "1" "$PX_NOLAKEHOUSE_RC"
check "and names the command that installs it" "1" \
	"$(printf '%s' "$PX_NOLAKEHOUSE" | grep -c 'deploy/setup-lake.sh pxtest' || true)"
check "never pulls the job image" "1" \
	"$(grep -c '^[[:space:]]*podman run --rm --pull=never' "${REPO_DIR}/deploy/prices.sh" || true)"
# Nothing keyed by a tenant is within this job's reach: it reads raw/ and writes
# Iceberg, and mounts no data volume at all.
check "no tenant data is mounted into the job" "0" \
	"$(grep -c '^\s*-v "\${DATA}' "${REPO_DIR}/deploy/prices.sh" || true)"

reset_store

log "14. The ownership shipment is SCHEDULED, and 3 is its own answer (pd-dxn3)"
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
# EVERY tenant's database, so they may never run beside each other. Since
# pd-i08u it is also a DATA dependency, and it points the other way — the
# transform reads what this unit's second half writes — so the After= line
# lives over there (§10 asserts it) and this unit follows the price build.
check "ordered after the price build" "1" \
	"$(grep -c '^After=pkdump-prices@%i.service$' "$SH_SVC" || true)"
check "…and after the landing it ultimately depends on" "1" \
	"$(grep -c '^After=pkdump-refresh@%i.service$' "$SH_SVC" || true)"
# And NOT Wants=: every job in the chain is a oneshot without RemainAfterExit,
# so pulling one in would re-run it.
check "does not pull the price build in" "0" \
	"$(grep -c '^\(Wants\|Requires\)=pkdump-prices@%i.service$' "$SH_SVC" || true)"

# The read-back is the half pd-i08u armed, and it is armed HERE rather than in
# its own unit: `zone_holdings` is only correct when it was read immediately
# after a ship, and both halves need the master key and the tenant profile that
# nothing else on this box holds. A `run` with no `holdings` beside it is a
# transform that skips every tenant, silently, on every box.
check "the wrapper ships AND reads back" "1" \
	"$(grep -c 'pkdump_ship holdings' "${REPO_DIR}/deploy/ship.sh" || true)"

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

# The calendar entry is DERIVED from the landing unit's own declared bounds,
# the same computation §10 and §12 make. This unit used to be derived from the
# transform because it ran last; pd-i08u put it BEFORE the transform, so it now
# joins the 07:00 wave and the transform is the one derived from it (§10).
SH_START=$(($(hhmm_secs "$SH_TMR") + $(key_secs "$SH_TMR" RandomizedDelaySec)))
check "fires no earlier than the refresh can finish" "ok" \
	"$([ "$SH_START" -ge "$REFRESH_LATEST" ] && echo ok || echo "starts ${SH_START}s < refresh ${REFRESH_LATEST}s")"
# …and strictly before the transform is even asked for, so the two halves of
# the round trip are over before anything values them. §10 asserts the same
# relationship from the other side; this is the one that fails if somebody
# moves THIS timer.
VS_START_AGAIN=$(($(hhmm_secs "$VS_TMR") + $(key_secs "$VS_TMR" RandomizedDelaySec)))
check "is asked for before the transform is" "ok" \
	"$([ "$SH_START" -lt "$VS_START_AGAIN" ] && echo ok || echo "ship ${SH_START}s >= transform ${VS_START_AGAIN}s")"
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

# Two subcommands now (pd-i08u), so the fake has to tell them apart: the
# wrapper runs `pkdump-ship run` and then `pkdump-ship holdings` through the
# same `podman run`, and the whole point of the section below is which of them
# produced which status.
cat > "${WORK}/shbin/podman" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
"secret inspect") exit 1 ;;
"image exists") exit "${PKDUMP_TEST_NO_IMAGE:-0}" ;;
esac
if [ "$1" = run ]; then
	SUB=ship
	for ARG in "$@"; do
		[ "$ARG" = holdings ] && SUB=holdings
	done
	# The argv the wrapper actually built, so the checks below can assert which
	# arguments reached which half rather than only which half ran.
	printf 'FAKE %s ARGV %s\n' "$SUB" "$*"
	if [ "$SUB" = holdings ]; then
		case "${PKDUMP_TEST_READBACK_RC:-0}" in
		2) printf '    skipped 01J0000000000000000000000B: no database at /data/tenants\n' ;;
		esac
		exit "${PKDUMP_TEST_READBACK_RC:-0}"
	fi
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

# --- The read-back half, and how the two statuses compose (pd-i08u) ---------
# `zone_holdings` is the only thing the transform can value a collection from,
# so "the shipment worked" is no longer the whole story. Four numbers, two
# halves, and the precedence 3 > 1 > 2 > 0 — asserted as behaviour because a
# wrapper that reported only the first half would leave the transform skipping
# everybody while the unit sat green.

check "a clean run reads the zone back too" "1" \
	"$(printf '%s' "$SH_OK" | grep -c 'FAKE holdings ARGV' || true)"
check "…and says so" "1" \
	"$(printf '%s' "$SH_OK" | grep -c 'ship: READ BACK' || true)"

# A shipment that shipped NOTHING does not go on to read back: same unreachable
# bucket, same missing key, and one clear failure beats two confusing ones.
check "a failed shipment does not attempt the read-back" "0" \
	"$(printf '%s' "$SH_FAILED" | grep -c 'FAKE holdings ARGV' || true)"
check "…and says why it did not" "1" \
	"$(printf '%s' "$SH_FAILED" | grep -c 'the zone was not read back' || true)"

# A GAP still reads back: the events that survived ARE in the zone, and
# withholding them from tonight's valuation would add a second loss to the
# first. Same argument `pkdump-ship run` makes for shipping past a gap at all.
check "a sequence gap still reads the zone back" "1" \
	"$(printf '%s' "$SH_GAP" | grep -c 'FAKE holdings ARGV' || true)"
check "…and 3 still wins over the read-back's own status" "1" \
	"$(printf '%s' "$SH_GAP" | grep -c 'RC=3$' || true)"

# The read-back's own three statuses, with a clean shipment underneath so the
# number can only have come from the second half.
SH_RB_PARTIAL="$(PKDUMP_TEST_READBACK_RC=2 run_ship)"
check "a partial read-back exits 2" "1" \
	"$(printf '%s' "$SH_RB_PARTIAL" | grep -c 'RC=2$' || true)"
check "…named as the READ BACK half, not as a shipping problem" "1" \
	"$(printf '%s' "$SH_RB_PARTIAL" | grep -c 'READ BACK PARTIAL — tenants skipped: 01J0000000000000000000000B' || true)"

SH_RB_FAILED="$(PKDUMP_TEST_READBACK_RC=1 run_ship)"
check "a read-back that reached nobody exits 1, not 0" "1" \
	"$(printf '%s' "$SH_RB_FAILED" | grep -c 'RC=1$' || true)"
# The whole diagnosis in one line: the half that worked and the half that did
# not. Without it the journal says "ship: OK" and the unit fails, which reads
# as a crash rather than as tonight's valuation having no input.
check "…saying the shipment worked and the read-back did not" "1" \
	"$(printf '%s' "$SH_RB_FAILED" | grep -c 'READ BACK FAILED' || true)"
check "…while the shipment half still reported its own OK" "1" \
	"$(printf '%s' "$SH_RB_FAILED" | grep -c 'ship: OK' || true)"

# 1 beats 2: a partial shipment whose read-back reached nobody is a failure,
# because nobody gets valued either way.
SH_MIXED="$(PKDUMP_TEST_JOB_RC=2 PKDUMP_TEST_READBACK_RC=1 run_ship)"
check "the worse of the two halves is what the unit is told" "1" \
	"$(printf '%s' "$SH_MIXED" | grep -c 'RC=1$' || true)"

# --- Which arguments reach which half ---------------------------------------
# `--tenant` is the one argument both subcommands take, and a shipment scoped
# to one tenant that then read EVERYBODY back would be a surprise. Everything
# else is a `run` argument and must not be handed to the read-back, which
# would reject it and turn a good night into a clap error.
SH_SCOPED="$(run_ship --tenant alice)"
check "--tenant reaches the shipment" "1" \
	"$(printf '%s' "$SH_SCOPED" | grep -c 'FAKE ship ARGV.*run --tenant alice' || true)"
check "…and the read-back is scoped to the same tenant" "1" \
	"$(printf '%s' "$SH_SCOPED" | grep -c 'FAKE holdings ARGV.*holdings --tenant alice' || true)"

SH_MAXROWS="$(run_ship --max-rows 500)"
check "--max-rows reaches the shipment" "1" \
	"$(printf '%s' "$SH_MAXROWS" | grep -c 'FAKE ship ARGV.*--max-rows 500' || true)"
check "…and is NOT handed to the read-back, which has no such flag" "0" \
	"$(printf '%s' "$SH_MAXROWS" | grep -c 'FAKE holdings ARGV.*--max-rows' || true)"

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
log "15. ABSENT, FORBIDDEN and WRONG are three answers, not one (pd-2hnp)"
# ---------------------------------------------------------------------------
#
# setup-tenant-zone.sh --check printed one sentence — "no lifecycle
# configuration" — for a genuinely absent rule AND for an AccessDenied. "There
# is no retention rule" and "I am not allowed to look" are opposite facts, and
# the subject here is a rule whose job is DELETING TENANT DATA AFTER 90 DAYS.
#
# The direction that hurts is the inverse of the one it was found in: once the
# rule IS applied, an operator whose credentials cannot read it is told the rule
# was never applied, and the repair for that is to apply or widen one.
#
# The classification is pure string work over what the aws CLI says, so it is
# tested here rather than in the container tier — hermetic, sub-second, and
# every case is a red one. tests/lake/tenant_zone.sh §2 is where a REAL denial
# by a REAL credential against a REAL bucket produces exit 4; this is where the
# six-way answer is pinned, including the two the container tier cannot easily
# stage (an unrecognised error, and a lifecycle that exists but says nothing
# about the tenant zone).

TZ_HOME="${WORK}/tzhome"
mkdir -p "$TZ_HOME"
: >"${WORK}/tz-lake.env"

# A stand-in for the aws CLI. setup-tenant-zone.sh takes the whole command as
# an override (PKDUMP_AWS) so the container tier can hand it a podman
# invocation; the same seam is what lets this file hand it a fixture.
cat >"${WORK}/tz-aws.sh" <<'EOF'
#!/usr/bin/env bash
args="$*"
case "$args" in
*"sts get-caller-identity"*)
	[ -n "${STUB_IDENTITY:-}" ] && {
		printf '%s\n' "$STUB_IDENTITY"
		exit 0
	}
	echo "Could not connect to the endpoint URL" >&2
	exit 255
	;;
*get-bucket-lifecycle-configuration*)
	case "${STUB_MODE:-}" in
	absent)
		echo "An error occurred (NoSuchLifecycleConfiguration) when calling the GetBucketLifecycleConfiguration operation: The lifecycle configuration does not exist" >&2
		exit 254
		;;
	denied)
		echo "An error occurred (AccessDenied) when calling the GetBucketLifecycleConfiguration operation: User: arn:aws:iam::111:user/gantt-mtgc-backup is not authorized to perform: s3:GetLifecycleConfiguration" >&2
		exit 254
		;;
	weird)
		echo "An error occurred (ServiceUnavailable) when calling the GetBucketLifecycleConfiguration operation: please retry" >&2
		exit 254
		;;
	wrongdays) echo '{"Rules":[{"ID":"too-long","Status":"Enabled","Filter":{"Prefix":"tenant/"},"Expiration":{"Days":365}}]}' ;;
	reachescatalog) echo '{"Rules":[{"ID":"tenant","Status":"Enabled","Filter":{"Prefix":"tenant/"},"Expiration":{"Days":90}},{"ID":"tidy-raw","Status":"Enabled","Filter":{"Prefix":"raw/"},"Expiration":{"Days":30}}]}' ;;
	notenant) echo '{"Rules":[{"ID":"scratch","Status":"Enabled","Filter":{"Prefix":"scratch/"},"Expiration":{"Days":7}}]}' ;;
	correct) echo '{"Rules":[{"ID":"pkdump-tenant-zone-90-day-expiry","Status":"Enabled","Filter":{"Prefix":"tenant/"},"Expiration":{"Days":90}}]}' ;;
	esac
	;;
esac
EOF
chmod +x "${WORK}/tz-aws.sh"

tz_check() { # tz_check <STUB_MODE> [extra args...]; prints output then RC=<n>
	local mode="$1"
	shift
	set +e
	STUB_MODE="$mode" STUB_IDENTITY="${STUB_IDENTITY:-}" \
		PKDUMP_AWS="bash ${WORK}/tz-aws.sh" HOME="$TZ_HOME" \
		PKDUMP_LAKE_ENV="${WORK}/tz-lake.env" \
		bash "${REPO_DIR}/deploy/setup-tenant-zone.sh" --check \
		--bucket pdtz-hermetic "$@" 2>&1
	printf 'RC=%s' "$?"
	set -e
}

tz_has() { # tz_has <output> <fixed string> -> 1 | 0
	printf '%s' "$1" | grep -qF -- "$2" && echo 1 || echo 0
}

TZ_OK="$(tz_check correct)"
check "the correct rule -> exit 0" "1" "$(printf '%s' "$TZ_OK" | grep -c 'RC=0$' || true)"

# ABSENT. The one case where "no lifecycle configuration" is the truth.
TZ_ABSENT="$(tz_check absent)"
check "NoSuchLifecycleConfiguration -> exit 3" "1" \
	"$(printf '%s' "$TZ_ABSENT" | grep -c 'RC=3$' || true)"
check "…and says ABSENT" "1" "$(tz_has "$TZ_ABSENT" 'ABSENT')"
check "…and names the repair" "1" \
	"$(tz_has "$TZ_ABSENT" 'setup-tenant-zone.sh --apply')"

# A lifecycle that exists and says nothing about tenant/ is ALSO absent
# retention, with the same repair — not a wrong rule.
TZ_NOTENANT="$(tz_check notenant)"
check "a lifecycle with no tenant/ rule -> exit 3" "1" \
	"$(printf '%s' "$TZ_NOTENANT" | grep -c 'RC=3$' || true)"

# FORBIDDEN. The bug: this used to be indistinguishable from the case above.
TZ_DENIED="$(tz_check denied)"
check "AccessDenied -> exit 4, NOT 3" "1" \
	"$(printf '%s' "$TZ_DENIED" | grep -c 'RC=4$' || true)"
check "…and says CANNOT VERIFY" "1" "$(tz_has "$TZ_DENIED" 'CANNOT VERIFY')"
# The regression this whole section exists to prevent. ABSENT is the verdict
# label, so a denied run must never carry it — it may only mention the word in
# the sentence saying this is NOT that answer.
check "…and never renders the ABSENT verdict" "0" \
	"$(tz_has "$TZ_DENIED" 'ABSENT')"
check "…and names the missing permission" "1" \
	"$(tz_has "$TZ_DENIED" 's3:GetLifecycleConfiguration')"
check "…and quotes what aws actually said" "1" \
	"$(tz_has "$TZ_DENIED" 'gantt-mtgc-backup')"

# Fail closed: an error nobody anticipated is "cannot verify", never absence.
TZ_WEIRD="$(tz_check weird)"
check "an unrecognised error -> exit 4, not 3 and not 0" "1" \
	"$(printf '%s' "$TZ_WEIRD" | grep -c 'RC=4$' || true)"
check "…and never renders the ABSENT verdict" "0" \
	"$(tz_has "$TZ_WEIRD" 'ABSENT')"

# PRESENT BUT WRONG keeps exit 1 — both shapes of it.
TZ_DAYS="$(tz_check wrongdays)"
check "365 days on tenant/ -> exit 1" "1" \
	"$(printf '%s' "$TZ_DAYS" | grep -c 'RC=1$' || true)"
TZ_CAT="$(tz_check reachescatalog)"
check "a second rule reaching raw/ -> exit 1" "1" \
	"$(printf '%s' "$TZ_CAT" | grep -c 'RC=1$' || true)"

# The identity. Tonight's real failure was a script silently acting as another
# project's backup user; whatever else it does, it has to SAY who it is.
check "the identity is printed before anything acts" "1" \
	"$(tz_has "$TZ_OK" '==> Identity:')"
check "…and ambient credentials are called ambient" "1" \
	"$(tz_has "$TZ_OK" 'NO --profile GIVEN')"
TZ_RESOLVED="$(STUB_IDENTITY=arn:aws:sts::237707363372:assumed-role/pokedump-data/x \
	tz_check correct --profile pkdump)"
check "a resolved ARN is printed verbatim" "1" \
	"$(tz_has "$TZ_RESOLVED" 'assumed-role/pokedump-data')"
check "…and --profile is named instead of the ambient warning" "0" \
	"$(tz_has "$TZ_RESOLVED" 'NO --profile GIVEN')"
# An endpoint that does not implement sts (MinIO does not) must not stop a
# check — a governance script that cannot run against a stand-in bucket is one
# that cannot be tested.
check "an unresolvable identity is printed as unresolved, not fatal" "1" \
	"$(tz_has "$TZ_OK" 'UNRESOLVED')"


# ---------------------------------------------------------------------------
log "16. Host-wide units belong to the deploy clone (pd-onyd)"
# ---------------------------------------------------------------------------
#
# Everything under ~/.config/systemd/user is ONE FILE PER BOX. The %i templates
# look per-instance and are not — pkdump-refresh@.service backs prod and every
# CI instance at once — and each bakes {{REPO_DIR}} into an ExecStart. So
# "install the units" meant "point prod's alerting, landing and disk check at
# whichever checkout ran setup.sh LAST".
#
# deploy/ci.sh runs setup.sh from a per-checkout worktree and `gt done` deletes
# that worktree. Observed on the deployment box 2026-08-09: `deploy/setup.sh
# vault-unitfix --test` from a polecat worktree left prod's units executing
# .../polecats/vault/pokedumpster/deploy/alert.sh — 203/EXEC the moment the
# branch landed, which is the Jun 2026 backup outage's exact shape.

reset_store

HOSTHOME="${WORK}/hosthome"
HOST_UNITS="${HOSTHOME}/.config/systemd/user"
HOST_QUADLET="${HOSTHOME}/.config/containers/systemd"

# The library under test. Sourced once; every case drives it with HOME pointed
# at a fake box, so nothing here can touch the operator's real units.
# shellcheck source=deploy/units-lib.sh
. "${REPO_DIR}/deploy/units-lib.sh"

install_as() { # install_as <instance> [env=val ...] -> stdout of the install
	local inst="$1"; shift
	env HOME="$HOSTHOME" "$@" bash -c '
		. "$1/deploy/store-lib.sh"; . "$1/deploy/units-lib.sh"
		pkdump_units_install "$2" 0
	' _ "$REPO_DIR" "$inst" 2>&1
}
owners_of() { HOME="$HOSTHOME" pkdump_units_host_owners; }

# --- A fresh box: somebody has to install them, and it may be anyone ---------
rm -rf "$HOSTHOME"; mkdir -p "$HOST_UNITS" "$HOST_QUADLET"
install_as ci-9f2c1a >/dev/null
check "a fresh box gets its host-wide units from whoever asked first" "$REPO_DIR" \
	"$(owners_of)"

# --- The classification covers the directory, both ways ---------------------
# A shared unit written but not declared would be unguarded by everything
# below; a name declared but never written would make the owner probe read a
# file that no deploy can ever correct. Neither fails loudly on its own.
check "every installed host-wide unit is declared" "" \
	"$(comm -23 <(ls "$HOST_UNITS" | sort) \
	            <(printf '%s\n' "${PKDUMP_HOST_WIDE_UNIT_FILES[@]}" | sort) | tr '\n' ' ' | sed 's/ $//')"
check "every declared host-wide unit is installed" "" \
	"$(comm -13 <(ls "$HOST_UNITS" | sort) \
	            <(printf '%s\n' "${PKDUMP_HOST_WIDE_UNIT_FILES[@]}" | sort) | tr '\n' ' ' | sed 's/ $//')"
# The Quadlet side is per-instance by construction — the instance is in the
# file name — which is why it is written unconditionally and is not in the list.
check "the Quadlet units are per-instance" "pkdump-ci-9f2c1a.container pkdump-litestream-ci-9f2c1a.container" \
	"$(ls "$HOST_QUADLET" | sort | tr '\n' ' ' | sed 's/ $//')"

# --- The bug: another checkout must not take them over ----------------------
OTHER="${WORK}/otherclone"
mkdir -p "${OTHER}/deploy"
grep -rl "$REPO_DIR" "$HOST_UNITS" | xargs sed -i "s|${REPO_DIR}|${OTHER}|g"

SKIP_OUT="$(install_as ci-9f2c1a)"
check "a throwaway instance does NOT repoint them" "$OTHER" "$(owners_of)"
check "and says whose they are" "1" \
	"$(printf '%s' "$SKIP_OUT" | grep -c "belong to ${OTHER}" || true)"
check "and names the override" "1" \
	"$(printf '%s' "$SKIP_OUT" | grep -c 'PKDUMP_INSTALL_HOST_UNITS=1' || true)"
# Refusing the host-wide units must not refuse the instance: standing up a
# throwaway is the normal case here, not the anomaly.
check "the instance's own Quadlet is still installed" "yes" \
	"$([ -f "${HOST_QUADLET}/pkdump-ci-9f2c1a.container" ] && echo yes || echo no)"

# --- The real deployment always wins ----------------------------------------
install_as prod >/dev/null
check "an install for prod takes them" "$REPO_DIR" "$(owners_of)"

# ...and 'prod' is not hardcoded here any more than it is in the alert gate.
grep -rl "$REPO_DIR" "$HOST_UNITS" | xargs sed -i "s|${REPO_DIR}|${OTHER}|g"
install_as staging PKDUMP_ALERT_INSTANCES="prod staging" >/dev/null
check "so does one for a second declared deployment" "$REPO_DIR" "$(owners_of)"

# --- An owner that is gone is a repair, not a theft --------------------------
grep -rl "$REPO_DIR" "$HOST_UNITS" | xargs sed -i "s|${REPO_DIR}|${OTHER}|g"
rm -rf "$OTHER"
install_as ci-9f2c1a >/dev/null
check "units left pointing at a deleted worktree are taken over" "$REPO_DIR" "$(owners_of)"

# --- Both overrides ----------------------------------------------------------
mkdir -p "${OTHER}/deploy"
grep -rl "$REPO_DIR" "$HOST_UNITS" | xargs sed -i "s|${REPO_DIR}|${OTHER}|g"
install_as ci-9f2c1a PKDUMP_INSTALL_HOST_UNITS=1 >/dev/null
check "=1 forces the write" "$REPO_DIR" "$(owners_of)"

grep -rl "$REPO_DIR" "$HOST_UNITS" | xargs sed -i "s|${REPO_DIR}|${OTHER}|g"
OFF_OUT="$(install_as prod PKDUMP_INSTALL_HOST_UNITS=0)"
check "=0 forces the skip, even for prod" "$OTHER" "$(owners_of)"
check "and says which reason it skipped for" "1" \
	"$(printf '%s' "$OFF_OUT" | grep -c 'PKDUMP_INSTALL_HOST_UNITS=0' || true)"

# --- The owner is read out of the unit, so it cannot disagree with systemd ---
# There is no marker file to drift: what the probe reports is the directory the
# ExecStart will actually execute.
check "no marker file is kept beside the units" "0" \
	"$(find "$HOSTHOME" -name '*owner*' -o -name '*.host-units*' | wc -l)"

rm -rf "$HOSTHOME" "$OTHER"
reset_store

# ---------------------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
