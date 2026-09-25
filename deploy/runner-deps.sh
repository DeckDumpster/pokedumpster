#!/usr/bin/env bash
#
# The system dependencies deploy/ci.sh needs, as a script rather than a
# paragraph.
#
# THIS IS THE DEVELOPMENT DEPENDENCY LIST, not a CI-only file. The name says
# "runner" because an ephemeral CI runner is what forced it to exist, but a
# developer, a polecat and CI all run deploy/ci.sh and therefore all need
# exactly these things. deploy/README.md's "Prerequisites" points here rather
# than restating a subset -- it used to restate one, and that copy went stale.
#
#   bash deploy/runner-deps.sh            install anything missing (idempotent)
#   bash deploy/runner-deps.sh --check    report what is missing; exit 1 if any
#
# WHY THIS EXISTS. CI used to run on one long-lived box where somebody had
# installed cargo, node and podman by hand, under that user's $HOME. The
# workflow knew it -- it carried a step called "Put the per-user toolchains on
# PATH" that prepended ~/.cargo/bin and ~/.local/bin, added because the very
# first run of that workflow died with `cargo: command not found`. That step
# was the dependency list, written as a PATH fixup, in a file nothing else
# reads.
#
# On a per-run VM there is no such box. The list has to be executable, it has
# to be checkable, and it has to live next to the script that needs it. Adding
# a tool to ci.sh means adding it here.
#
# IDEMPOTENT BY CONSTRUCTION. Every action checks for its own outcome first, so
# the fast path on an already-provisioned box is a handful of `command -v`
# calls and no package manager at all.
#
# NOT RUN AUTOMATICALLY BY ci.sh. Installing packages needs sudo, and a test
# script that silently apt-installs on a developer's laptop is a worse problem
# than the one it solves. ci.sh only *checks*; the caller installs.
set -uo pipefail

MODE=install
case "${1:-}" in
    --check) MODE=check ;;
    "")      MODE=install ;;
    *)       printf 'usage: runner-deps.sh [--check]\n' >&2; exit 2 ;;
esac

MISSING=()
note()  { printf 'runner-deps: %s\n' "$*"; }
lack()  { MISSING+=("$1"); printf 'runner-deps: MISSING %s -- %s\n' "$1" "$2" >&2; }

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        SUDO="sudo -n"
    fi
fi

# The per-user toolchains install under $HOME and a non-login shell does not
# have them on PATH. Doing this here rather than in the workflow means a
# developer, a polecat and CI all get the same answer from `command -v cargo`.
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

# installable <pkg> -- true when apt can actually install this name.
#
# `apt-cache show` is NOT this test. Ubuntu 24.04's 64-bit time_t transition
# renamed a dozen library packages with a `t64` suffix and left the OLD name in
# the cache as a record with no installation candidate, so `apt-cache show
# libasound2` succeeds while `apt-get install libasound2` fails with "has no
# installation candidate" -- and apt installs a list as one transaction, so a
# single unusable name aborts everything alongside it. Ask for the candidate
# version, which is the question actually being asked.
installable() {
    local cand
    cand="$(apt-cache policy "$1" 2>/dev/null | sed -n 's/^  Candidate: //p')"
    [ -n "$cand" ] && [ "$cand" != "(none)" ]
}

APT_UPDATED=0
# WAIT FOR THE DPKG LOCK RATHER THAN FAILING ON IT.
#
# A per-run VM boots, systemd starts unattended-upgrades, and this script starts
# installing — in that order, within seconds of each other. Whoever reaches
# /var/lib/dpkg/lock-frontend second gets "Could not get lock ... held by
# process N (unattended-upgr)" and, without this, simply gives up:
#
#   runner-deps: could not install sqlite3
#   runner-deps: could not install podman
#   ...
#   runner-deps: still missing after install: base tools podman chromium libraries
#
# and the job dies before deploy/ci.sh is ever reached. It is a RACE, so it
# fails perhaps one run in several and looks like a broken dependency list
# rather than a timing bug — the same list had installed cleanly on the runs
# either side of it.
#
# apt has had a lock timeout since 1.9.11; this is the whole fix, and it is
# better than a retry loop because it waits on the lock itself rather than
# sleeping and racing again. Unquoted on purpose: it must expand to two words
# or to nothing on an apt too old to know the option.
#
# The durable fix is the image — a VM that lives thirty minutes and is then
# destroyed has nothing to gain from unattended-upgrades — but this holds on any
# box, including a developer laptop that happens to be mid-upgrade.
APT_LOCK_WAIT="-o DPkg::Lock::Timeout=600"

apt_install() {
    local want=() p
    for p in "$@"; do
        dpkg -s "$p" >/dev/null 2>&1 && continue
        dpkg -s "${p}t64" >/dev/null 2>&1 && continue
        if installable "$p"; then
            want+=("$p")
        elif installable "${p}t64"; then
            want+=("${p}t64")
        else
            note "no installable package named $p or ${p}t64 on this release -- skipping"
        fi
    done
    [ ${#want[@]} -gt 0 ] || return 0
    [ -n "$SUDO" ] || { printf 'runner-deps: need root to install: %s\n' "${want[*]}" >&2; return 1; }
    if [ "$APT_UPDATED" = 0 ]; then
        $SUDO apt-get update -qq $APT_LOCK_WAIT || true
        APT_UPDATED=1
    fi
    note "installing ${want[*]}"
    if DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq $APT_LOCK_WAIT "${want[@]}"; then
        return 0
    fi
    # Individually, because ONE uninstallable package must not cost the other
    # nine. Note this cannot rescue a lock failure — the retry would race the
    # same holder — which is what APT_LOCK_WAIT above is for.
    note "batch install failed -- retrying individually"
    for p in "${want[@]}"; do
        DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq $APT_LOCK_WAIT "$p" \
            || note "could not install $p"
    done
}

# ---------------------------------------------------------------------------
# Plain command-line tools ci.sh and the deploy scripts shell out to.
# ---------------------------------------------------------------------------
# build-essential is NOT optional and NOT a guess. Rust shells out to `cc` to
# LINK, so without a C compiler every cargo invocation dies at
#
#     error: linker `cc` not found
#     error: could not compile `proc-macro2` (build script)
#
# which reads as a broken Rust toolchain and is a missing Debian package. It was
# on the old runner because somebody had put it there; nothing said so. The
# Containerfile's builder stage is `rust:1.94-slim-bookworm`, whose only build
# tooling is gcc and libc6-dev — so that set is provably sufficient for
# everything the release build compiles, and build-essential is a superset.
#
# pkg-config is for the dev-dependencies the image never builds: `cargo test`
# compiles a strictly larger graph than `cargo build --release` does, so the
# container is a lower bound on what CI needs, not an upper one.
BASE_PKGS=(git curl jq sqlite3 xz-utils ca-certificates build-essential pkg-config)
BASE_CMDS=(git curl jq sqlite3 python3 cc make)

check_base() {
    local c missing=()
    for c in "${BASE_CMDS[@]}"; do command -v "$c" >/dev/null 2>&1 || missing+=("$c"); done
    [ ${#missing[@]} -eq 0 ] && return 0
    lack "base tools (${missing[*]})" "deploy/ci.sh and the deploy scripts call these directly; cargo needs cc to LINK"
    return 1
}

# ---------------------------------------------------------------------------
# Podman, rootless-capable.
#
# uidmap          newuidmap/newgidmap; without it `podman run` fails with
#                 "newuidmap not found".
# passt           provides `pasta`, which podman 5.x uses as its rootless
#                 network backend AND runs inside the rootless network
#                 namespace that user-defined (bridge) networks require.
#                 Without it a container on the default network still works --
#                 which is why this went unnoticed -- but anything on a bridge
#                 fails at namespace setup with
#
#                     Error: rootless netns: cleanup: 1 error occurred:
#                       * rootless netns: kill network process: permission denied
#
#                 naming neither pasta nor a missing package.
# slirp4netns     the older rootless network backend, kept as the fallback
#                 podman uses when pasta is absent or refuses.
# fuse-overlayfs  rootless overlay storage. Without it podman falls back to
#                 vfs, where the builder stage takes minutes instead of seconds.
# ---------------------------------------------------------------------------
PODMAN_MIN_MAJOR=4
PODMAN_MIN_MINOR=4

check_podman() {
    command -v podman >/dev/null 2>&1 || { lack podman "deploy/ci.sh builds the image and starts a --test instance"; return 1; }
    local raw v major minor
    # Keep stderr: when this fails to parse, an empty string says nothing about
    # why, and "cannot parse podman version []" was the entire record of it on
    # the first cold VM. Whatever podman said about itself is the answer.
    raw="$(podman --version 2>&1)"
    v="$(printf '%s\n' "$raw" | awk '{print $3}')"
    major="${v%%.*}"; minor="${v#*.}"; minor="${minor%%.*}"
    case "${major:-x}${minor:-x}" in *[!0-9]*) note "cannot parse podman version from [$raw] -- not enforcing the floor"; return 0 ;; esac
    if [ "$major" -lt "$PODMAN_MIN_MAJOR" ] || { [ "$major" -eq "$PODMAN_MIN_MAJOR" ] && [ "$minor" -lt "$PODMAN_MIN_MINOR" ]; }; then
        # Not a missing binary -- a silently wrong one. Quadlet .container
        # support arrived in 4.4; older podman IGNORES .container files, so
        # `systemctl --user start pkdump-<instance>` succeeds having started
        # nothing and every later port lookup returns empty.
        lack "podman>=${PODMAN_MIN_MAJOR}.${PODMAN_MIN_MINOR}" \
             "found $v; deploy/*.container are Quadlet units and are ignored below ${PODMAN_MIN_MAJOR}.${PODMAN_MIN_MINOR}"
        return 1
    fi
    note "container engine $v"

    # A rootless network backend must exist, and `podman --version` says nothing
    # about whether one does. pasta is podman 5.x's default and the one the
    # rootless netns needs; slirp4netns is the fallback. Either satisfies this.
    if ! command -v pasta >/dev/null 2>&1 && ! command -v slirp4netns >/dev/null 2>&1; then
        lack "pasta or slirp4netns" "rootless podman has no network backend; bridge networks fail at namespace setup"
        return 1
    fi
    note "rootless network backend: $(command -v pasta || command -v slirp4netns)"

    # PASTA IS PRESENT AND UNUSABLE ON THIS PLATFORM, SO A BACKEND BEING
    # INSTALLED IS NOT THE QUESTION.
    #
    # Ubuntu confines pasta with /etc/apparmor.d/usr.bin.pasta, which declares no
    # `signal` rules at all. podman launches pasta, cannot signal the process it
    # just launched, and every rootless container on a USER-DEFINED network dies
    # in cleanup with
    #
    #     rootless netns: kill network process: permission denied
    #
    # Measured on podman 5.7.0 + passt 0.0~git20260120, Ubuntu 26.04: this
    # affects the DEFAULT store too, not only the alternate stores store-lib.sh
    # creates, so it is a property of the box rather than of any one store.
    # (Under $HOME it fails a step earlier: the profile grants `owner @{HOME}/** w`
    # — write only — and pasta_open_ns() needs READ, so it cannot open the netns
    # file podman hands it at all.)
    #
    # The drop-in below selects slirp4netns, which carries no such profile. It is
    # what the deploy layer will need on this image too; CI is simply the first
    # thing to run here.
    local dropin="${HOME}/.config/containers/containers.conf.d/10-rootless-network.conf"
    if ! grep -qs 'default_rootless_network_cmd' "$dropin" 2>/dev/null; then
        lack "rootless network backend selection" \
             "podman defaults to pasta, which Ubuntu's AppArmor profile makes unusable; write $dropin"
        return 1
    fi

    # UNPRIVILEGED USER NAMESPACES MUST NOT BE APPARMOR-CONFINED.
    #
    # Ubuntu 24.04 turned on kernel.apparmor_restrict_unprivileged_userns, which
    # drops a process that creates an unprivileged userns into a restricted
    # AppArmor domain. podman starts pasta inside the rootless network namespace
    # that user-defined (bridge) networks need, pasta lands in that domain, and
    # podman -- in a different one -- can no longer signal it:
    #
    #     Error: rootless netns: cleanup: 1 error occurred:
    #       * rootless netns: kill network process: permission denied
    #
    # which names neither AppArmor nor a namespace, and reproduces with pasta
    # correctly installed. Port publishing on the DEFAULT network is unaffected,
    # which is why a suite that only does that never sees it.
    #
    # Reported with its evidence, because the next person to read this will
    # otherwise be looking at the same nameless EPERM.
    local aa="/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
    if [ -r "$aa" ] && [ "$(cat "$aa" 2>/dev/null)" = "1" ]; then
        lack "unconfined unprivileged userns" \
             "kernel.apparmor_restrict_unprivileged_userns=1; podman cannot signal pasta inside a rootless netns, and bridge networks fail with 'kill network process: permission denied'"
        return 1
    fi
}

check_subid() {
    local u; u="$(id -un)"
    grep -q "^${u}:" /etc/subuid 2>/dev/null && grep -q "^${u}:" /etc/subgid 2>/dev/null && return 0
    lack "subuid/subgid for ${u}" "rootless podman cannot map users without them"
    return 1
}

# LINGER IS THE FIX; THE RUNTIME DIRECTORY IS THE PROPERTY.
#
# `loginctl show-user -p Linger` reports a SETTING. What podman and
# `systemctl --user` actually need is /run/user/<uid> to exist, which is what a
# running user manager creates. Those are not the same claim: a box can report
# Linger=yes with no runtime directory, and the failure then arrives as
#
#     Failed to obtain podman configuration:
#     lstat /run/user/1000: no such file or directory
#
# from six assertions about a container store, naming neither linger nor
# systemd. Check the directory; enable linger to get it.
check_linger() {
    local u uid rt
    u="$(id -un)"; uid="$(id -u)"
    rt="${XDG_RUNTIME_DIR:-/run/user/${uid}}"
    [ -d "$rt" ] && return 0
    lack "XDG_RUNTIME_DIR (${rt})" "podman and systemctl --user need a live user manager; enable linger for ${u}"
    return 1
}

# ---------------------------------------------------------------------------
# Rust.
#
# rust-toolchain.toml pins the channel and the components, and rustup honours
# it automatically on the first cargo invocation inside the repo. So this only
# has to ensure rustup itself exists -- pinning the version HERE as well would
# create a second place for the toolchain to be declared, and the two would
# drift. The distro's rustc is NOT acceptable: it is a different version with
# no rustup shim, so rust-toolchain.toml is silently ignored and `cargo fmt
# --check` runs a formatter the repo was never formatted with.
# ---------------------------------------------------------------------------
check_rust() {
    command -v rustup >/dev/null 2>&1 || { lack rustup "rust-toolchain.toml pins the channel and only rustup honours it"; return 1; }
    command -v cargo  >/dev/null 2>&1 || { lack cargo "deploy/ci.sh runs cargo test, clippy and fmt"; return 1; }
    note "rust $(rustc --version 2>/dev/null || echo '(version unknown)')"
}

# ---------------------------------------------------------------------------
# Node.
#
# frontend/ is on vite 8, which requires Node >= 20.19. Ubuntu 24.04 ships
# Node 18, so the distro package is not merely old -- it is below the floor,
# and the failure is a vite startup error rather than anything naming a Node
# version. The floor is checked, not just the binary's presence.
# ---------------------------------------------------------------------------
NODE_MIN_MAJOR=20
NODE_MAJOR_INSTALL=22        # what the installer fetches; must be >= the floor

check_node() {
    command -v node >/dev/null 2>&1 || { lack node "deploy/ci.sh runs npm ci, npm test, npm run check and npm run build"; return 1; }
    command -v npm  >/dev/null 2>&1 || { lack npm  "deploy/ci.sh runs npm ci"; return 1; }
    local v major
    v="$(node --version 2>/dev/null)"; v="${v#v}"
    major="${v%%.*}"
    case "${major:-x}" in *[!0-9]*) note "cannot parse node version [$v] -- not enforcing the floor"; return 0 ;; esac
    if [ "$major" -lt "$NODE_MIN_MAJOR" ]; then
        lack "node>=${NODE_MIN_MAJOR}" "found $v; frontend/ is on vite 8, which refuses to start below ${NODE_MIN_MAJOR}.19"
        return 1
    fi
    note "node v$v, npm $(npm --version 2>/dev/null)"
}

# ---------------------------------------------------------------------------
# Chromium's shared libraries.
#
# `npx playwright install chromium` downloads the Chromium binary and none of
# its system dependencies. Without these the download succeeds and the first
# browser launch fails with `error while loading shared libraries: libnss3.so`
# -- mid-suite, long after everything looks fine.
# ---------------------------------------------------------------------------
CHROMIUM_LIBS=(libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2
               libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 libxrandr2
               libgbm1 libasound2 libpango-1.0-0 libpangocairo-1.0-0)

check_chromium_libs() {
    command -v dpkg >/dev/null 2>&1 || return 0
    local p missing=()
    for p in "${CHROMIUM_LIBS[@]}"; do
        dpkg -s "$p" >/dev/null 2>&1 && continue
        dpkg -s "${p}t64" >/dev/null 2>&1 && continue
        missing+=("$p")
    done
    [ ${#missing[@]} -eq 0 ] && return 0
    lack "chromium libraries (${missing[*]})" "the visual tier launches headless Chromium via Playwright"
    return 1
}

# ---------------------------------------------------------------------------
# The fonts the visual baselines were recorded against.
#
# Separate from CHROMIUM_LIBS because it is a different failure mode: a missing
# library stops the browser from launching and says so, while a missing font
# launches fine and renders the page in a substitute -- so the suite fails as a
# diff against the baselines, which reads as a CSS regression and is not one.
# That is how this arrived: the first ephemeral run put 34 of 82 visual tests
# red with the page chrome around the changed pixels byte-identical.
#
# Two independent gaps, two packages:
#
#   fonts-liberation       The app's own text is not affected -- its stack
#                          starts at `system-ui`, which fontconfig answers with
#                          DejaVu either way. Form controls are. Blink does not
#                          style `<input>`, `<select>` and `<button>` from the
#                          page's font stack; it resolves them through the Arial
#                          alias, and with no metric-compatible Arial installed
#                          fontconfig substitutes the wider DejaVu Sans. Every
#                          route in routes.json carrying a control then differs
#                          from its baseline and the seven carrying none do not
#                          -- which was exactly the split. Installing it moves
#                          nothing else: `fc-match sans-serif` still answers
#                          DejaVu, so only the controls change.
#
#   fonts-noto-color-emoji The emoji in the app's own copy -- the cart on
#                          "Buy missing", the popper on the empty unresolved
#                          queue. No font on a base Ubuntu covers U+1F6D2 or
#                          U+1F389, so they render as tofu boxes. Four routes,
#                          and the only ones left once Liberation was in.
#
# The shared box this suite used to run on had both, because an earlier
# `playwright install-deps` pulled them in. Nothing recorded that, which is why
# neither came along with the move to a per-run VM.
# ---------------------------------------------------------------------------
# fontconfig for fc-match/fc-list themselves: the base cloud image carries
# libfontconfig1 but not the binaries, so without it check_fonts cannot run the
# measurement it exists to make.
FONT_PKGS=(fontconfig fonts-liberation fonts-noto-color-emoji)

# Codepoints the app actually renders, not a package list: a box that covers
# them some other way is not made to install ours.
FONT_GLYPHS=(1f6d2 1f389)   # shopping cart, party popper

check_fonts() {
    command -v fc-match >/dev/null 2>&1 || {
        lack "fontconfig (fc-match, fc-list)" \
             "the visual baselines depend on which faces fontconfig picks"
        return 1
    }

    local rc=0 cp
    case "$(fc-match -f '%{family}' Arial 2>/dev/null)" in
        *Liberation*|*Arimo*|*Arial*) ;;
        *)  lack "a metric-compatible Arial (fonts-liberation)" \
                 "without it Blink draws every form control in DejaVu and the visual tier fails as a diff"
            rc=1 ;;
    esac

    for cp in "${FONT_GLYPHS[@]}"; do
        [ -n "$(fc-list ":charset=${cp}" family 2>/dev/null)" ] && continue
        lack "an emoji face covering U+${cp^^} (fonts-noto-color-emoji)" \
             "the app renders that character and it comes out as a tofu box"
        rc=1
    done
    return $rc
}

# ── THE MACHINE IS A DEPENDENCY, and nothing here used to say what it was ────
#
# Every timing constant in this suite was measured on one specific box, and
# some record it in prose: ci-parallel.sh's gate cap says "a 15G box with four
# cores that also runs prod". CI now runs on a per-run ephemeral VM with its
# own size, and no run logged what it got -- so a release build that
# Containerfile:24 documents as a cold "3-4 minute" rebuild measured 8m21s on
# 2026-09-15 and nothing flagged it, because there was nothing to compare it
# against. A run that does not say what it ran on cannot be compared with one
# that did.
#
# getconf, not nproc: getconf _NPROCESSORS_ONLN reads from sysfs and ignores
# cgroup CPU quota. nproc honours the quota — CPUQuota=70% on a 24-core box
# reports 1 — so running this check inside a constrained unit (e.g. a Spira
# aeon) would fail the floor for a machine property the branch cannot affect.
# A quota throttles TOTAL CPU TIME; it does not stop cargo from dispatching
# work to all physical cores. We want to know the host's capacity, not how
# much of it this process has been lent.
#
# Deliberately no disk figure here. deploy/diskcheck.sh owns the disk and
# ci-parallel.sh warns in its own header that a second enumeration is how one
# of them stops covering a path (pd-20ia, pd-6jyd). One owner per resource.
#
# The floor is what the suite is KNOWN to pass on, not what it deserves. CI was
# green on 8 GiB / 4 cores on 2026-09-15; raising the floor above that is a
# decision to make against a measurement, because a floor that fails a box CI
# is currently green on is an outage rather than a finding. 7, not 8: a VM
# given 8 GiB reports ~7.6 to /proc/meminfo, and the kernel never hands back
# all of it.
#
# 5 is a stopgap set on 2026-09-25 to fit the shared 6144 MiB runner (template
# 107, MemTotal ~5.3 GiB, which integer-divides to 5). db-zbck replaces this
# with a measured floor and memory-aware gate parallelism.
MACHINE_MIN_CORES=4
MACHINE_MIN_MEM_GIB=5

check_machine() {
    local cores mem_kib mem_gib mem_disp rc=0
    cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null \
        || ls -d /sys/devices/system/cpu/cpu[0-9]* 2>/dev/null | wc -l \
        || echo 1)"
    mem_kib="$(awk '/^MemTotal:/ {print $2; exit}' /proc/meminfo 2>/dev/null)"
    case "${cores}${mem_kib:-}" in *[!0-9]*|'') cores=0; mem_kib=0 ;; esac
    : "${mem_kib:=0}"
    mem_gib=$(( mem_kib / 1048576 ))
    mem_disp="$(awk -v k="$mem_kib" 'BEGIN{printf "%.1f", k/1048576}')"
    note "machine $(uname -m): ${cores} cores, ${mem_disp} GiB RAM"
    if [ "$cores" -lt "$MACHINE_MIN_CORES" ]; then
        lack "${MACHINE_MIN_CORES} cores" \
             "found ${cores}; every timing in this suite, and the gate cap, assume at least that many"
        rc=1
    fi
    if [ "$mem_gib" -lt "$MACHINE_MIN_MEM_GIB" ]; then
        lack "${MACHINE_MIN_MEM_GIB} GiB RAM" \
             "found ${mem_disp}; each container gate stands up a MinIO, sometimes a 1 GB JVM and a whole pkdump instance, and below this they fail on contention rather than on what they assert"
        rc=1
    fi
    return $rc
}

run_checks() {
    MISSING=()
    check_base;   check_podman
    check_subid;  check_linger
    check_rust;   check_node
    check_chromium_libs
    check_fonts
    check_machine
}

if [ "$MODE" = check ]; then
    run_checks
    if [ ${#MISSING[@]} -gt 0 ]; then
        printf '\nrunner-deps: %d dependency group(s) missing on %s.\n' "${#MISSING[@]}" "$(hostname)" >&2
        printf 'runner-deps: install them with:  bash deploy/runner-deps.sh\n' >&2
        exit 1
    fi
    note "all dependencies present"
    exit 0
fi

# --------------------------------- install ---------------------------------
note "installing dependencies for $(id -un) on $(hostname)"

apt_install "${BASE_PKGS[@]}"
command -v podman >/dev/null 2>&1 || apt_install podman uidmap fuse-overlayfs
# Separately from podman's own presence: a box that already had podman may still
# lack pasta, and a bridge network is the only thing that notices.
apt_install passt slirp4netns
apt_install "${CHROMIUM_LIBS[@]}"
# See check_fonts: not cosmetic. Without these the visual tier goes red on a
# third of its routes and reads as a CSS regression.
apt_install "${FONT_PKGS[@]}"

# rustup, not the distro toolchain -- see check_rust. --no-modify-path because
# this script puts ~/.cargo/bin on PATH itself and a profile edit would only
# take effect in some future login shell, which a runner service never opens.
if ! command -v rustup >/dev/null 2>&1; then
    note "installing rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --default-toolchain none
fi
export PATH="$HOME/.cargo/bin:$PATH"

# Materialise the pinned toolchain now rather than letting the first `cargo
# test` do it mid-gate, where a download failure reads as a test failure.
if command -v rustup >/dev/null 2>&1 && [ -f rust-toolchain.toml ]; then
    note "installing the toolchain pinned in rust-toolchain.toml"
    rustup show active-toolchain >/dev/null 2>&1 || true
    rustup toolchain install --profile minimal --component rustfmt --component clippy \
        "$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' rust-toolchain.toml)" \
        || note "rustup could not preinstall the pinned toolchain -- cargo will fetch it on first use"
fi

# Node. The distro package is BELOW the floor (see check_node), so this takes
# NodeSource rather than apt's `nodejs`. Skipped entirely when a new enough
# node is already present, which is what makes it a no-op on a developer's
# machine and on a template that has been baked with one.
if ! check_node >/dev/null 2>&1; then
    note "installing Node ${NODE_MAJOR_INSTALL} from NodeSource"
    if [ -n "$SUDO" ]; then
        curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR_INSTALL}.x" \
            | $SUDO -E bash - >/dev/null 2>&1 \
            && apt_install nodejs \
            || note "NodeSource setup failed"
    else
        note "need root to install node"
    fi
fi

# See check_podman for why. A machine property, so it is set here rather than
# worked around in the suite; the durable home is the Proxmox template, and this
# keeps a hand-built box and a developer's Ubuntu working too.
AA="/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
if [ -w "$AA" ] || [ -n "$SUDO" ]; then
    if [ -r "$AA" ] && [ "$(cat "$AA" 2>/dev/null)" = "1" ]; then
        note "allowing unconfined unprivileged user namespaces (rootless bridge networks)"
        $SUDO sysctl -q -w kernel.apparmor_restrict_unprivileged_userns=0 2>/dev/null \
            || note "WARNING: could not clear kernel.apparmor_restrict_unprivileged_userns"
    fi
fi

# See check_podman for the measurement. Box-level rather than per-store, because
# the default store is affected too. A drop-in, not containers.conf itself, so
# nothing a person or another tool put there is overwritten.
DROPIN="${HOME}/.config/containers/containers.conf.d/10-rootless-network.conf"
if ! grep -qs 'default_rootless_network_cmd' "$DROPIN" 2>/dev/null; then
    note "selecting slirp4netns as the rootless network backend"
    mkdir -p "$(dirname "$DROPIN")"
    cat > "$DROPIN" <<'DROP'
# Written by deploy/runner-deps.sh.
#
# podman 5.x defaults to pasta. Ubuntu confines pasta with
# /etc/apparmor.d/usr.bin.pasta, which declares no `signal` rules, so podman
# cannot kill the process it starts and every container on a user-defined
# network fails cleanup with "rootless netns: kill network process: permission
# denied". Under $HOME it fails earlier still: the profile grants write-only
# access there and pasta_open_ns() needs read.
#
# slirp4netns carries no such profile. Remove this file if the profile is ever
# fixed upstream.
[network]
default_rootless_network_cmd = "slirp4netns"
DROP
fi

RUNUSER="$(id -un)"
if ! grep -q "^${RUNUSER}:" /etc/subuid 2>/dev/null || ! grep -q "^${RUNUSER}:" /etc/subgid 2>/dev/null; then
    note "adding subuid/subgid range for ${RUNUSER}"
    $SUDO usermod --add-subuids 100000-165535 --add-subgids 100000-165535 "$RUNUSER" || true
fi

# Enabling linger starts the user manager, which is what CREATES
# /run/user/<uid>. That is not instantaneous, and the re-check below runs
# immediately, so wait for the directory rather than for the setting — the
# directory is the thing every later caller needs.
if command -v loginctl >/dev/null 2>&1 && [ ! -d "/run/user/$(id -u)" ]; then
    note "enabling linger for ${RUNUSER}"
    $SUDO loginctl enable-linger "$RUNUSER" || true
    for _ in $(seq 1 20); do
        [ -d "/run/user/$(id -u)" ] && break
        sleep 0.5
    done
    [ -d "/run/user/$(id -u)" ] \
        && note "user manager up; /run/user/$(id -u) exists" \
        || note "WARNING: /run/user/$(id -u) still absent after enabling linger"
fi

# Re-check and report. A partial install is a failure HERE rather than a
# surprise three gates into ci.sh on a machine that no longer exists.
printf '\n'
run_checks
if [ ${#MISSING[@]} -gt 0 ]; then
    printf '\nrunner-deps: still missing after install: %s\n' "${MISSING[*]}" >&2
    exit 1
fi
note "all dependencies present"
