#!/usr/bin/env bash
# Unit test for tests/lib/wait.sh (pd-86er).
#
# Two halves, and the second is the point of the bead.
#
#   §1-§4 the library does what it claims — it asks before it sleeps, it returns
#         the moment the condition holds, and it GIVES UP. The third is the one
#         that matters most: a poll with no bound converts a failing gate into a
#         hung CI job, which is worse than the fixed sleep it replaced, because
#         the failure stops being reported at all.
#   §5-§6 THE RATCHET. Fixed sleeps were removed from the harnesses once
#         (pd-86er, ~2 minutes of wall clock across tests/litestream/); the way
#         they come back is one line at a time, each one locally reasonable. So
#         the tree itself is asserted on: no harness under tests/ or deploy/ may
#         sleep longer than SLEEP_CEILING seconds in a single call, and
#         wait_until may exist in exactly one place. A relapse fails HERE, in
#         under a second, instead of costing eight seconds a run forever.
#
# Deliberately hermetic — no podman, no network — so deploy/ci.sh can run it
# early beside tests/lib/ports_test.sh.
#
#   bash tests/lib/wait_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=tests/lib/wait.sh
. "${SCRIPT_DIR}/wait.sh"

# The longest a single `sleep` may be in a harness. Three seconds is a poll
# interval whose body is itself expensive (a `podman run` costs the better part
# of a second, so polling faster than that just burns the runner); anything
# longer is waiting for the clock rather than for the condition.
SLEEP_CEILING=3

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
# An assertion that prints the offending lines, because "something in the tree
# sleeps too long" is useless without saying which line does.
none() { # none <label> <lines>
	if [[ -z "$2" ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		printf '          %s\n' "$2"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# Milliseconds, so §1 can tell "returned immediately" from "slept once".
now_ms() { echo $(($(date +%s%N) / 1000000)); }

# A condition that counts how many times it was asked, and succeeds on the Nth.
CALLS="$(mktemp "${TMPDIR:-/tmp}/pd-waittest.XXXXXX")"
trap 'rm -f "$CALLS"' EXIT
reset_calls() { : >"$CALLS"; }
call_count() { grep -c . "$CALLS"; }
succeeds_on() { # succeeds_on <n> — true once it has been asked <n> times
	echo x >>"$CALLS"
	[ "$(call_count)" -ge "$1" ]
}

log "1. it asks BEFORE it sleeps"
# The failure this rules out is a poll written as sleep-then-check, which is a
# fixed sleep wearing a loop.
reset_calls
T0=$(now_ms)
wait_until 30 5 succeeds_on 1
RC=$?
ELAPSED=$(( $(now_ms) - T0 ))
check "returns 0 when the condition already holds" "0" "$RC"
check "having asked exactly once" "1" "$(call_count)"
check "and without sleeping its interval first (< 5s interval, took ${ELAPSED}ms)" "yes" \
	"$([ "$ELAPSED" -lt 1000 ] && echo yes || echo no)"

log "2. it returns as soon as the condition holds, not a moment later"
reset_calls
T0=$(now_ms)
wait_until 30 0.2 succeeds_on 4
RC=$?
ELAPSED=$(( $(now_ms) - T0 ))
check "returns 0 once the condition comes true" "0" "$RC"
check "having asked exactly as many times as it took" "4" "$(call_count)"
# Three intervals of 0.2s is 600ms; the ceiling is loose because this box runs
# the suite under load, but it is nowhere near the 30s timeout.
check "in about three intervals, not the timeout (took ${ELAPSED}ms)" "yes" \
	"$([ "$ELAPSED" -lt 10000 ] && echo yes || echo no)"

log "3. IT GIVES UP — a poll that never does would hang CI instead of failing it"
reset_calls
T0=$(now_ms)
wait_until 2 0.25 false
RC=$?
ELAPSED=$(( $(now_ms) - T0 ))
check "a condition that never holds returns non-zero" "1" "$RC"
# The lower bound is 1s, not 2: SECONDS ticks on the wall clock, so a deadline
# of `SECONDS + 2` set a moment before a tick genuinely expires in just over a
# second. The claim is that it waited and then stopped, not that it counted.
check "within roughly the timeout it was given (took ${ELAPSED}ms)" "yes" \
	"$([ "$ELAPSED" -ge 1000 ] && [ "$ELAPSED" -lt 20000 ] && echo yes || echo no)"

log "4. the condition is asked at least once, whatever the timeout"
# A zero timeout is a legitimate "check it now"; asking nothing and reporting
# failure would be a different answer from the one the caller wanted.
reset_calls
wait_until 0 1 succeeds_on 1
RC=$?
check "timeout 0 still asks" "1" "$(call_count)"
check "and answers on what it found" "0" "$RC"

log "5. wait_until is defined exactly once, and every caller sources it"
# Same ratchet as tests/lib/ports_test.sh §8, for the same reason: a copied
# helper is how a fix stops reaching the next file.
harnesses() {
	find "${REPO_DIR}/tests" "${REPO_DIR}/deploy" -name '*.sh' -type f \
		! -path "${REPO_DIR}/tests/lib/wait_test.sh" | sort
}
DEFS="$(harnesses | xargs grep -ln '^wait_until() {' /dev/null)"
check "one definition" "${REPO_DIR}/tests/lib/wait.sh" "$DEFS"
CALLERS="$(harnesses | xargs grep -l 'wait_until ' /dev/null | grep -v '/tests/lib/wait.sh$')"
UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/wait.sh"' "$f" || UNSOURCED+="${f}"$'\n'
done <<<"$CALLERS"
none "every caller sources tests/lib/wait.sh" "${UNSOURCED%$'\n'}"

log "6. no harness sleeps longer than ${SLEEP_CEILING}s in one call (pd-86er)"
# `sleep 8` after starting a sidecar, `sleep 10` after a write phase, `sleep
# "${1:-8}"` inside a helper — measured at ~2 minutes per run across
# tests/litestream/ alone, none of it asserting anything. What replaced them
# says what it is waiting FOR.
#
# Excluded: `--entrypoint sleep <image> infinity`, which is not a delay at all —
# it is how a throwaway container is kept alive to be exec'd into. And comment
# lines, so a file may go on explaining the sleeps it no longer has.
LONG="$(
	harnesses | xargs grep -nE '(^|[^-[:alnum:]_])sleep[[:space:]]+[0-9.]+' /dev/null |
		grep -v 'entrypoint sleep' | grep -vE '^[^:]*:[0-9]+:[[:space:]]*#' |
		awk -v ceiling="$SLEEP_CEILING" '
			match($0, /(^|[^-[:alnum:]_])sleep[[:space:]]+[0-9.]+/) {
				s = substr($0, RSTART, RLENGTH)
				sub(/.*sleep[[:space:]]+/, "", s)
				if (s + 0 > ceiling) print
			}'
)"
none "every sleep is a poll interval, not a wait for the clock" "$LONG"
# And the variable form of the same thing: a helper whose duration is a
# parameter with a long default is a fixed sleep with a knob on it.
DEFAULTED="$(
	harnesses | xargs grep -nE 'sleep[[:space:]]+"?\$\{[^}]*:-[0-9.]+\}' /dev/null |
		grep -vE '^[^:]*:[0-9]+:[[:space:]]*#' |
		awk -v ceiling="$SLEEP_CEILING" '
			match($0, /:-[0-9.]+\}/) {
				s = substr($0, RSTART + 2, RLENGTH - 3)
				if (s + 0 > ceiling) print
			}'
)"
none "no sleep takes its duration from a long default" "$DEFAULTED"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — harnesses wait for conditions, boundedly, in one place."
