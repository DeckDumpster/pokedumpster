#!/usr/bin/env bash
#
# Run the visual suite against an instance that is already up.
#
# Installs the node deps and the Chromium build on first use, then runs
# Playwright. `run.sh` calls this after standing up a throwaway instance;
# `deploy/ci.sh` calls it against the container it already started, so CI
# does not pay for a second one.
#
# Usage:
#   PKDUMP_BASE_URL=http://localhost:8099 bash tests/visual/playwright.sh [args...]
#
# An approval (`--update-snapshots`) may not be narrowed to a single viewport:
# see approval-guard.sh.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

: "${PKDUMP_BASE_URL:?PKDUMP_BASE_URL must point at a running instance}"

# An approval covers every viewport (pd-tf4h, pd-4tce). Refused BEFORE the npm
# install and the Chromium fetch below, so the refusal costs nothing, and here
# rather than in run.sh because this is the one place any harness in this repo
# executes Playwright — run.sh, deploy/ci.sh and the README's own recipes all
# arrive through it, so a caller cannot forget to ask.
# shellcheck source=tests/visual/approval-guard.sh
. "$SCRIPT_DIR/approval-guard.sh"
pkdump_visual_approval_guard "$@"

if [ ! -d node_modules ]; then
    echo "==> Installing visual-suite dependencies..."
    if [ -f package-lock.json ]; then npm ci; else npm install; fi
fi

# Chromium is cached in ~/.cache/ms-playwright and shared across projects;
# this is a no-op after the first run on a box.
npx playwright install chromium >/dev/null

echo "==> Screenshotting ${PKDUMP_BASE_URL}..."
exec npx playwright test "$@"
