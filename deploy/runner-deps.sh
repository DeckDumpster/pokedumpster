#!/usr/bin/env bash
#
# The system dependencies deploy/ci.sh needs, as a script rather than a
# paragraph.
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
        $SUDO apt-get update -qq || true
        APT_UPDATED=1
    fi
    note "installing ${want[*]}"
    if DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq "${want[@]}"; then
        return 0
    fi
    note "batch install failed -- retrying individually"
    for p in "${want[@]}"; do
        DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq "$p" \
            || note "could not install $p"
    done
}

# ---------------------------------------------------------------------------
# Plain command-line tools ci.sh and the deploy scripts shell out to.
# ---------------------------------------------------------------------------
BASE_PKGS=(git curl jq sqlite3 xz-utils ca-certificates)
BASE_CMDS=(git curl jq sqlite3 python3)

check_base() {
    local c missing=()
    for c in "${BASE_CMDS[@]}"; do command -v "$c" >/dev/null 2>&1 || missing+=("$c"); done
    [ ${#missing[@]} -eq 0 ] && return 0
    lack "base tools (${missing[*]})" "deploy/ci.sh and the deploy scripts call these directly"
    return 1
}

# ---------------------------------------------------------------------------
# Podman, rootless-capable.
#
# uidmap          newuidmap/newgidmap; without it `podman run` fails with
#                 "newuidmap not found".
# slirp4netns     rootless networking. Podman 4.x prefers pasta when present.
# fuse-overlayfs  rootless overlay storage. Without it podman falls back to
#                 vfs, where the builder stage takes minutes instead of seconds.
# ---------------------------------------------------------------------------
PODMAN_MIN_MAJOR=4
PODMAN_MIN_MINOR=4

check_podman() {
    command -v podman >/dev/null 2>&1 || { lack podman "deploy/ci.sh builds the image and starts a --test instance"; return 1; }
    local v major minor
    v="$(podman --version 2>/dev/null | awk '{print $3}')"
    major="${v%%.*}"; minor="${v#*.}"; minor="${minor%%.*}"
    case "${major:-x}${minor:-x}" in *[!0-9]*) note "cannot parse podman version [$v] -- not enforcing the floor"; return 0 ;; esac
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
}

check_subid() {
    local u; u="$(id -un)"
    grep -q "^${u}:" /etc/subuid 2>/dev/null && grep -q "^${u}:" /etc/subgid 2>/dev/null && return 0
    lack "subuid/subgid for ${u}" "rootless podman cannot map users without them"
    return 1
}

check_linger() {
    command -v loginctl >/dev/null 2>&1 || return 0
    local u; u="$(id -un)"
    [ "$(loginctl show-user "$u" -p Linger --value 2>/dev/null)" = "yes" ] && return 0
    # The deploy scripts write Quadlet units under ~/.config/containers/systemd,
    # which systemd only reads inside a live user session. Without lingering the
    # failure surfaces as a systemctl --user error that says nothing about
    # lingering.
    lack "linger for ${u}" "systemctl --user has no user manager to talk to; Quadlet units are never generated"
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

run_checks() {
    MISSING=()
    check_base;   check_podman
    check_subid;  check_linger
    check_rust;   check_node
    check_chromium_libs
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
command -v podman >/dev/null 2>&1 || apt_install podman uidmap slirp4netns fuse-overlayfs
apt_install "${CHROMIUM_LIBS[@]}"

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

RUNUSER="$(id -un)"
if ! grep -q "^${RUNUSER}:" /etc/subuid 2>/dev/null || ! grep -q "^${RUNUSER}:" /etc/subgid 2>/dev/null; then
    note "adding subuid/subgid range for ${RUNUSER}"
    $SUDO usermod --add-subuids 100000-165535 --add-subgids 100000-165535 "$RUNUSER" || true
fi

if command -v loginctl >/dev/null 2>&1 \
   && [ "$(loginctl show-user "$RUNUSER" -p Linger --value 2>/dev/null)" != "yes" ]; then
    note "enabling linger for ${RUNUSER}"
    $SUDO loginctl enable-linger "$RUNUSER" || true
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
