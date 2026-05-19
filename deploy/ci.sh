#!/usr/bin/env bash
#
# Local CI loop for PokeDumpster — the "super tight" inner dev cycle.
#
# Reproduces what a GitHub `ci.yml` workflow would do, as a plain script so
# it runs identically on a laptop and (eventually) on a CI runner. A future
# GitHub workflow should be a thin wrapper that just calls this file.
#
# Steps:
#   1. Tear down any stale `ci` container instance.
#   2. Rust gates:     cargo test, cargo clippy --all-targets, cargo fmt --check.
#   3. Frontend gate:  npm ci && npm run check && npm run build.
#   4. Container gate: build + start a `--test` instance, wait for the server
#                      to answer on its port, then tear it down.
#   5. Intents UI:     run the Playwright harness if browsers are installed;
#                      skip (without failing) if they are not.
#
# Exits non-zero on the first failure. Fast and re-runnable.
#
# Usage:
#   bash deploy/ci.sh
#
set -euo pipefail

# systemctl --user / podman need XDG_RUNTIME_DIR; CI runners and
# non-interactive shells often lack it.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="ci"
SERVICE_NAME="pkdump-${INSTANCE}"
CONTAINER="systemd-${SERVICE_NAME}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

START_TIME=$(date +%s)

step() { echo ""; echo "==> $*"; }

# --- 1. Clean up any stale ci instance --------------------------------------

step "Cleaning up stale '${INSTANCE}' instance..."
bash "$SCRIPT_DIR/teardown.sh" "$INSTANCE" --purge 2>/dev/null || true
podman image prune -f >/dev/null 2>&1 || true

# Tear the ci instance down again on exit, whatever happens.
cleanup() {
    bash "$SCRIPT_DIR/teardown.sh" "$INSTANCE" --purge 2>/dev/null || true
}
trap cleanup EXIT

# --- 2. Rust gates ----------------------------------------------------------

step "cargo test"
( cd "$REPO_DIR" && cargo test )

step "cargo clippy --all-targets"
( cd "$REPO_DIR" && cargo clippy --all-targets -- -D warnings )

step "cargo fmt --check"
( cd "$REPO_DIR" && cargo fmt --check )

# --- 3. Frontend gate -------------------------------------------------------

step "Frontend: npm ci && npm run check && npm run build"
(
    cd "$REPO_DIR/frontend"
    npm ci
    npm run check
    npm run build
)

# --- 4. Container gate ------------------------------------------------------

step "Building and starting '--test' container instance..."
bash "$SCRIPT_DIR/setup.sh" "$INSTANCE" --test
systemctl --user start "$SERVICE_NAME"

step "Waiting for the server to answer..."
PORT=""
for _ in $(seq 1 30); do
    PORT=$(podman port "$CONTAINER" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)
    if [ -n "$PORT" ] && curl -sf -o /dev/null "http://localhost:${PORT}/"; then
        echo "    Server is up on port ${PORT}."
        break
    fi
    PORT=""
    sleep 2
done
if [ -z "$PORT" ]; then
    echo "ERROR: server failed to start within timeout."
    journalctl --user -u "$SERVICE_NAME" --no-pager -n 40 2>/dev/null || true
    exit 1
fi

# --- 5. Intents UI harness (Playwright) -------------------------------------
# Needs browsers, and ANTHROPIC_API_KEY for the Claude-Vision modes. Skip
# cleanly when browsers are not installed — never fail CI on a missing browser.

step "Intents UI harness (tests/ui)..."
UI_DIR="$REPO_DIR/tests/ui"
if [ ! -d "$UI_DIR" ]; then
    echo "    tests/ui not present — skipping."
elif ! command -v npx >/dev/null 2>&1; then
    echo "    npx not found — skipping intents harness."
else
    (
        cd "$UI_DIR"
        npm ci
        # Playwright stores browsers under PLAYWRIGHT_BROWSERS_PATH, else
        # ~/.cache/ms-playwright. A `chromium-*` subdir means a browser is
        # installed; if none exists, skip — never fail CI on a missing browser.
        BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
        if compgen -G "${BROWSERS_PATH}/chromium-*" >/dev/null 2>&1; then
            if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
                echo "    NOTE: ANTHROPIC_API_KEY unset — Vision-mode scenarios may skip."
            fi
            npx playwright test
        else
            echo "    Playwright browsers not installed — skipping intents harness."
            echo "    Install with: (cd tests/ui && npx playwright install chromium)"
        fi
    )
fi

# --- Done -------------------------------------------------------------------

ELAPSED=$(( $(date +%s) - START_TIME ))
echo ""
echo "==> CI passed in ${ELAPSED}s."
