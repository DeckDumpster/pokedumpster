#!/usr/bin/env bash
#
# Run the visual-regression suite against a throwaway container instance.
#
# Stands up an isolated `--test` instance seeded from the committed fixture,
# screenshots every route in routes.json at both viewports, compares against
# the committed baselines, and tears the instance down again.
#
# Usage:
#   bash tests/visual/run.sh                     # check against the baselines
#   bash tests/visual/run.sh --update            # APPROVE: rewrite them
#   bash tests/visual/run.sh --keep              # leave the instance running
#   bash tests/visual/run.sh --instance vis2     # a different throwaway name
#
# Reviewing a failure:
#   bash tests/visual/run.sh
#   (cd tests/visual && npm run report)          # side-by-side actual/diff
# and then, once the change is intended and reviewed, `--update` and commit
# the new PNGs with the CSS that moved them, in a commit that says which
# change moved which baselines. An approval covers every viewport; narrowing
# one to a single `--project` is refused — see approval-guard.sh.
#
# Already have an instance? Skip this script:
#   PKDUMP_BASE_URL=http://localhost:8099 npx playwright test
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

INSTANCE="visual"
KEEP=false
PW_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --instance) INSTANCE="$2"; shift 2 ;;
        --keep)     KEEP=true; shift ;;
        --update)   PW_ARGS+=(--update-snapshots); shift ;;
        *)          PW_ARGS+=("$1"); shift ;;
    esac
done

# An approval covers every viewport (pd-tf4h, pd-4tce). playwright.sh refuses it
# too — it is the one place any harness here executes Playwright — but that is
# three minutes and a container image downstream of here, and a refusal an
# operator waits for a build to receive is one they learn to route around.
# Same rule, one implementation, asked at the earliest point on this path.
# shellcheck source=tests/visual/approval-guard.sh
. "$SCRIPT_DIR/approval-guard.sh"
pkdump_visual_approval_guard ${PW_ARGS[@]+"${PW_ARGS[@]}"}

# PokeDumpster prod runs on this box. Instances are isolated by name, so the
# only way to hurt it is to name one 'prod'.
if [ "$INSTANCE" = "prod" ]; then
    echo "ERROR: refusing to run the visual suite against 'prod'." >&2
    exit 1
fi

SERVICE_NAME="pkdump-${INSTANCE}"
CONTAINER="systemd-${SERVICE_NAME}"

# Same container store deploy/ci.sh uses, resolved the same way: the host config
# names it, an explicit PKDUMP_STORE_ROOT beats that, unconfigured means Podman's
# default. Without this, `setup.sh` below would honour PKDUMP_STORE_ROOT while
# the bare `podman port` further down asked the DEFAULT store, find nothing, and
# time out waiting for a server that was already answering (pd-66hq).
# `pkdump_store_activate` puts a flag-carrying `podman` shim on PATH, so every
# podman call in this script — and in setup.sh/teardown.sh — lands in one store.
# shellcheck source=deploy/store-lib.sh
. "$REPO_DIR/deploy/store-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Bounded condition polling, in one place for every harness (pd-86er).
# shellcheck source=tests/lib/wait.sh
. "$REPO_DIR/tests/lib/wait.sh"

cleanup() {
    if [ "$KEEP" = true ]; then
        echo "==> Leaving '${INSTANCE}' running (--keep). Tear down with:"
        echo "    bash deploy/teardown.sh ${INSTANCE} --purge"
    else
        bash "$REPO_DIR/deploy/teardown.sh" "$INSTANCE" --purge >/dev/null 2>&1 || true
    fi
}

echo "==> Removing any stale '${INSTANCE}' instance..."
bash "$REPO_DIR/deploy/teardown.sh" "$INSTANCE" --purge >/dev/null 2>&1 || true
trap cleanup EXIT

echo "==> Building and starting the fixture instance '${INSTANCE}'..."
bash "$REPO_DIR/deploy/setup.sh" "$INSTANCE" --test
systemctl --user start "$SERVICE_NAME"

echo "==> Waiting for the server..."
PORT=""
# Four times a second, not once every two: `podman port` plus a loopback curl
# costs nothing, and the interval was most of what this step spent on a server
# that was already answering (pd-86er). Same 60-second bound as before.
server_answering() {
    PORT=$(podman port "$CONTAINER" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)
    if [ -n "$PORT" ] && curl -sf -o /dev/null "http://localhost:${PORT}/"; then
        echo "    Up on port ${PORT}."
        return 0
    fi
    PORT=""
    return 1
}
wait_until 60 0.25 server_answering || true
if [ -z "$PORT" ]; then
    echo "ERROR: server failed to start within timeout." >&2
    journalctl --user -u "$SERVICE_NAME" --no-pager -n 40 2>/dev/null || true
    exit 1
fi

PKDUMP_BASE_URL="http://localhost:${PORT}" \
    bash "$SCRIPT_DIR/playwright.sh" "${PW_ARGS[@]}"
