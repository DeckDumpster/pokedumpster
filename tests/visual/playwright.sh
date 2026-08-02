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
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

: "${PKDUMP_BASE_URL:?PKDUMP_BASE_URL must point at a running instance}"

if [ ! -d node_modules ]; then
    echo "==> Installing visual-suite dependencies..."
    if [ -f package-lock.json ]; then npm ci; else npm install; fi
fi

# Chromium is cached in ~/.cache/ms-playwright and shared across projects;
# this is a no-op after the first run on a box.
npx playwright install chromium >/dev/null

echo "==> Screenshotting ${PKDUMP_BASE_URL}..."
exec npx playwright test "$@"
