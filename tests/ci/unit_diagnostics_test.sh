#!/usr/bin/env bash
# tests/ci/unit_diagnostics_test.sh — deploy scripts capture unit evidence before teardown.
#
# When systemctl --user start/restart fails, the caller is about to exit or
# destroy its environment. Every diagnostic pointer that appears in the failure
# output — "see journalctl --user -xeu ..." — points at a journal that no
# longer exists by the time somebody reads the log. The scripts must capture
# the evidence THEMSELVES before exiting.
#
# The fix is a single helper, dump_unit_diagnostics, in deploy/diagnostics-lib.sh
# and sourced by both deploy/ci.sh and deploy/deploy.sh — one definition, not
# two (db-g6ku). This gate asserts the structural properties:
#
#   §1  The helper is defined in deploy/diagnostics-lib.sh and contains both
#       `systemctl status` and `journalctl` calls; ci.sh and deploy.sh source
#       the lib rather than redefining it.
#
#   §2  The helper is wired to the systemctl --user start failure path in
#       ci.sh, so a failed start does not exit silently.
#
#   §3  The helper is wired to the server-wait timeout path in ci.sh, so
#       both failure shapes produce the same evidence.
#
#   §4  The standalone journalctl call that predated the helper is gone — no
#       bare `journalctl` outside the helper definition in either file, so the
#       two paths cannot drift apart again.
#
#   §5  deploy/deploy.sh also calls dump_unit_diagnostics on restart failure
#       (db-g6ku: the production outage whose cause was a guess because the
#       log said "see journalctl" about a box the reader could not reach).
#
# Hermetic: grep over source files, no podman, no network. Sub-second, lint tier.
#
#   bash tests/ci/unit_diagnostics_test.sh
set -uo pipefail  # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CI_SH="${REPO_DIR}/deploy/ci.sh"
DEPLOY_SH="${REPO_DIR}/deploy/deploy.sh"
DIAG_LIB="${REPO_DIR}/deploy/diagnostics-lib.sh"

pass=0
fail=0
check() {  # check <label> <expected> <actual>
    if [[ "$2" == "$3" ]]; then
        echo "  PASS  $1"
        pass=$((pass + 1))
    else
        echo "  FAIL  $1"
        echo "          expected: $2"
        echo "          actual:   $3"
        fail=$((fail + 1))
    fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# Extract the body of dump_unit_diagnostics from the lib so the later
# assertions can be scoped to the function, not the whole file.
helper_body() {
    awk '/^dump_unit_diagnostics\(\)/{found=1} found{print} found && /^\}/{found=0}' "$DIAG_LIB"
}

# ---------------------------------------------------------------------------
log "§1 dump_unit_diagnostics is defined in diagnostics-lib.sh and sourced"

BODY="$(helper_body)"

check "dump_unit_diagnostics is defined in diagnostics-lib.sh" "yes" \
    "$(grep -q 'dump_unit_diagnostics()' "$DIAG_LIB" && echo yes || echo no)"

check "ci.sh sources diagnostics-lib.sh rather than redefining the function" "yes" \
    "$(grep -q 'diagnostics-lib.sh' "$CI_SH" && echo yes || echo no)"

check "deploy.sh sources diagnostics-lib.sh rather than redefining the function" "yes" \
    "$(grep -q 'diagnostics-lib.sh' "$DEPLOY_SH" && echo yes || echo no)"

check "helper body contains systemctl status" "yes" \
    "$(grep -q 'systemctl.*status' <<<"$BODY" && echo yes || echo no)"

check "helper body contains journalctl" "yes" \
    "$(grep -q 'journalctl' <<<"$BODY" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "§2 helper is called on systemctl --user start failure in ci.sh"

# The start line must be immediately followed (on the same logical line) by
# an || that calls dump_unit_diagnostics, so a failed start does not silently
# exit because of set -e.
check "systemctl start failure calls dump_unit_diagnostics" "yes" \
    "$(grep -q 'systemctl.*start.*dump_unit_diagnostics\|dump_unit_diagnostics.*systemctl.*start' "$CI_SH" && echo yes || echo no)"

# More precisely: the start call uses || { dump_unit_diagnostics ...; exit 1; }
check "start failure path exits after dumping diagnostics" "yes" \
    "$(grep -qE 'systemctl --user start.*\|\|.*dump_unit_diagnostics' "$CI_SH" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "§3 helper is called on the server-wait timeout in ci.sh"

# The timeout path must call dump_unit_diagnostics rather than an inline
# journalctl.
check "timeout path calls dump_unit_diagnostics" "yes" \
    "$(AWK_OUT="$(awk '/server failed to start within timeout/{found=1} found && /dump_unit_diagnostics/{print "yes"; found=0}' "$CI_SH")"; grep -q yes <<<"$AWK_OUT" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "§4 no bare journalctl outside the helper definition"

# The only journalctl calls in ci.sh and deploy.sh must be inside
# dump_unit_diagnostics (which now lives in the lib). Since the lib is sourced,
# neither script should contain any inline journalctl at all.
BARE_JOURNAL_CI="$(grep 'journalctl' "$CI_SH" | grep -v '^\s*#' || true)"
check "no bare journalctl in ci.sh" "" "$BARE_JOURNAL_CI"

BARE_JOURNAL_DEPLOY="$(grep 'journalctl' "$DEPLOY_SH" | grep -v '^\s*#' || true)"
check "no bare journalctl in deploy.sh" "" "$BARE_JOURNAL_DEPLOY"

# ---------------------------------------------------------------------------
log "§5 deploy.sh calls dump_unit_diagnostics on restart failure (db-g6ku)"

check "deploy.sh restart failure calls dump_unit_diagnostics" "yes" \
    "$(grep -qE 'systemctl --user restart.*\|\|.*dump_unit_diagnostics' "$DEPLOY_SH" && echo yes || echo no)"

# ---------------------------------------------------------------------------
echo ""
echo "unit diagnostics gate: ${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
