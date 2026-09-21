#!/usr/bin/env bash
# tests/ci/unit_diagnostics_test.sh — deploy/ci.sh captures unit evidence before teardown.
#
# When systemctl --user start fails, the CI runner is about to be destroyed.
# Every diagnostic pointer that appears in the failure output — "see journalctl
# --user -xeu ..." — points at a journal that no longer exists by the time
# somebody reads the log.  deploy/ci.sh must capture the evidence ITSELF before
# exiting, the way the wait-loop timeout path already did.
#
# The fix is a single helper, dump_unit_diagnostics, called from every abort
# path.  This gate asserts the structural properties:
#
#   §1  The helper is defined in deploy/ci.sh and contains both
#       `systemctl status` and `journalctl` calls.
#
#   §2  The helper is wired to the systemctl --user start failure path, so a
#       failed start does not exit silently.
#
#   §3  The helper is wired to the server-wait timeout path, so both failure
#       shapes produce the same evidence.
#
#   §4  The standalone journalctl call that predated the helper is gone — no
#       bare `journalctl` outside the helper definition, so the two paths
#       cannot drift apart again.
#
# Hermetic: grep over ci.sh, no podman, no network. Sub-second, lint tier.
#
#   bash tests/ci/unit_diagnostics_test.sh
set -uo pipefail  # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CI_SH="${REPO_DIR}/deploy/ci.sh"

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

# Extract the body of dump_unit_diagnostics from ci.sh so the later assertions
# can be scoped to the function, not the whole file.
# We find the function definition and collect lines until the closing brace.
helper_body() {
    awk '/^dump_unit_diagnostics\(\)/{found=1} found{print} found && /^\}/{found=0}' "$CI_SH"
}

# ---------------------------------------------------------------------------
log "§1 dump_unit_diagnostics is defined and contains both diagnostic commands"

BODY="$(helper_body)"

check "dump_unit_diagnostics is defined in ci.sh" "yes" \
    "$(grep -q 'dump_unit_diagnostics()' "$CI_SH" && echo yes || echo no)"

check "helper body contains systemctl status" "yes" \
    "$(grep -q 'systemctl.*status' <<<"$BODY" && echo yes || echo no)"

check "helper body contains journalctl" "yes" \
    "$(grep -q 'journalctl' <<<"$BODY" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "§2 helper is called on systemctl --user start failure"

# The start line must be immediately followed (on the same logical line) by
# an || that calls dump_unit_diagnostics, so a failed start does not silently
# exit because of set -e.
check "systemctl start failure calls dump_unit_diagnostics" "yes" \
    "$(grep -q 'systemctl.*start.*dump_unit_diagnostics\|dump_unit_diagnostics.*systemctl.*start' "$CI_SH" && echo yes || echo no)"

# More precisely: the start call uses || { dump_unit_diagnostics ...; exit 1; }
check "start failure path exits after dumping diagnostics" "yes" \
    "$(grep -qE 'systemctl --user start.*\|\|.*dump_unit_diagnostics' "$CI_SH" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "§3 helper is called on the server-wait timeout"

# The timeout path must call dump_unit_diagnostics rather than an inline
# journalctl.
check "timeout path calls dump_unit_diagnostics" "yes" \
    "$(AWK_OUT="$(awk '/server failed to start within timeout/{found=1} found && /dump_unit_diagnostics/{print "yes"; found=0}' "$CI_SH")"; grep -q yes <<<"$AWK_OUT" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "§4 no bare journalctl outside the helper definition"

# The only journalctl calls in ci.sh must be inside dump_unit_diagnostics.
# Extract lines that are NOT inside the helper body.
outside_body() {
    awk '
        /^dump_unit_diagnostics\(\)/ { skip=1 }
        skip && /^\}/ { skip=0; next }
        !skip { print }
    ' "$CI_SH"
}

BARE_JOURNAL="$(outside_body | grep 'journalctl' | grep -v '^\s*#' || true)"
check "no bare journalctl call outside dump_unit_diagnostics" "" "$BARE_JOURNAL"

# ---------------------------------------------------------------------------
echo ""
echo "unit diagnostics gate: ${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
