#!/usr/bin/env bash
# Unit test for tests/lib/diagnostics.sh (pd-8gjs).
#
# The thing under test is a harness's ability to describe its own death, so
# every case here KILLS a fixture script and asserts on what reached the real
# stderr. Deliberately hermetic — no podman, no network, no MinIO, nothing that
# can be slow or flaky — because a test that guards against an intermittent
# must not be intermittent itself. Runs in well under a second.
#
#   bash tests/lib/diagnostics_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LIB="${SCRIPT_DIR}/diagnostics.sh"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/pd-diagtest.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
check() { # check <label> <expected> <actual>
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
has() { # has <label> <needle> <haystack>
	case "$3" in
	*"$2"*) check "$1" "found" "found" ;;
	*) check "$1" "found: $2" "not found in:
$3" ;;
	esac
}
log() { printf '\n=== %s ===\n' "$*"; }

# Writes a fixture script that sources the library, runs it, and leaves its
# combined output in $OUT and its exit status in $RC. Combined, not split,
# because the ORDER of the failure report against the teardown chatter is
# itself one of the things under test.
FIXTURE="" OUT="" RC=0
fixture() { # fixture <name> <<'EOF' ... EOF
	FIXTURE="${WORK}/$1.sh"
	{
		echo '#!/usr/bin/env bash'
		echo 'set -euo pipefail'
		echo ". \"${LIB}\""
		cat
	} >"$FIXTURE"
	OUT="$(bash "$FIXTURE" 2>&1)"
	RC=$?
}
# The line number of a marked command in the fixture, so the assertion about
# "it names the failing line" cannot rot when this file is edited.
marked_line() { grep -n "$1" "$FIXTURE" | head -1 | cut -d: -f1; }

log "1. a failure silenced by 2>/dev/null still says what happened"
# The pd-8gjs shape exactly: a command that exits 127 and writes nothing, at a
# call site that redirects stderr away. Before this library that produced a
# completely empty log.
fixture silenced <<'EOF'
diag_init
probe() { diag_run probe bash -c 'exit 127'; }
probe 2>/dev/null
EOF
check "the fixture still dies (set -e is not neutered)" "127" "$RC"
has "the status is named" "probe failed (status 127)" "$OUT"
has "and so is the fact that it said nothing" "wrote nothing to stderr" "$OUT"

log "2. whatever the command DID write to stderr is reproduced"
fixture stderr_kept <<'EOF'
diag_init
probe() { diag_run probe bash -c 'echo "s3: connection refused" >&2; exit 3'; }
probe 2>/dev/null
EOF
check "the fixture dies with the command's status" "3" "$RC"
has "the command's own stderr survives the /dev/null" "s3: connection refused" "$OUT"

log "3. silent when the command succeeds"
fixture quiet <<'EOF'
diag_init
diag_run probe true
diag_run probe bash -c 'echo noise-on-stderr >&2; exit 0'
echo DONE
EOF
check "a passing command exits 0" "0" "$RC"
check "and prints nothing but its own stdout" "DONE" "$OUT"

log "4. the ERR trap names file, line, command and status"
fixture err_trap <<'EOF'
diag_init
run_section() {
	false # MARKED-FAILURE
}
run_section
EOF
check "the fixture dies" "1" "$RC"
has "the source file is named" "$(basename "$FIXTURE")" "$OUT"
has "the failing line is named" ":$(marked_line MARKED-FAILURE)" "$OUT"
has "the failing command is named" "command : false" "$OUT"
has "the exit status is named" "status  : 1" "$OUT"
has "and the call site it came from" "called from run_section()" "$OUT"

log "5. the failure is reported BEFORE the EXIT trap's teardown chatter"
# This is the half that made the original log unreadable: cleanup output was
# the last thing on screen, so the failure looked like it had no cause.
fixture ordering <<'EOF'
diag_init
cleanup() { echo "==> Stopping the test bed..."; }
trap cleanup EXIT
false # MARKED-FAILURE
EOF
FAIL_AT=$(printf '%s\n' "$OUT" | grep -n '!! FAILED' | head -1 | cut -d: -f1)
CLEAN_AT=$(printf '%s\n' "$OUT" | grep -n 'Stopping the test bed' | head -1 | cut -d: -f1)
check "the failure report precedes the teardown output" "yes" \
	"$([[ -n $FAIL_AT && -n $CLEAN_AT && $FAIL_AT -lt $CLEAN_AT ]] && echo yes || echo no)"

log "6. a failure inside a command substitution is reported too"
# errtrace, the reason `set -E` is in diag_init: harnesses compute nearly every
# assertion inside $( ), and without it the trap would fire for almost nothing.
fixture substitution <<'EOF'
diag_init
VALUE="$(bash -c 'echo why >&2; exit 42' # MARKED-FAILURE
)"
echo "$VALUE"
EOF
check "the fixture dies with that status" "42" "$RC"
has "the substitution's failure is reported" "status  : 42" "$OUT"

log "7. a pipeline failure is reported (pipefail + set -e)"
fixture pipeline <<'EOF'
diag_init
probe() { diag_run probe bash -c 'echo "network not found" >&2; exit 125'; }
probe 2>/dev/null | sort >/dev/null
EOF
check "the fixture dies with the pipeline's status" "125" "$RC"
has "the silenced stage's stderr is still reported" "network not found" "$OUT"

log "8. the report is the caller's line, not the wrapper's plumbing"
# diag_run has already said what failed and why; a second report naming the
# `return` inside the wrapper is noise that buries the line the reader wants.
fixture no_wrapper_noise <<'EOF'
diag_init
probe() { diag_run probe bash -c 'exit 9'; }
probe 2>/dev/null | cat >/dev/null # MARKED-FAILURE
EOF
has "the harness line that died is named" ":$(marked_line MARKED-FAILURE)" "$OUT"
check "and it is reported exactly once" "1" \
	"$(printf '%s\n' "$OUT" | grep -c '!! FAILED')"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — a harness sourcing tests/lib/diagnostics.sh names its own failure."
