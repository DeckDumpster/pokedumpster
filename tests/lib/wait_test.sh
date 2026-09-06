#!/usr/bin/env bash
# Unit test for tests/lib/wait.sh (pd-86er).
#
# Three parts, and the ratchets are the point.
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
#   §7    THE SAME RATCHET, ONE TRAP OVER (pd-pfxf). `podman logs | grep -q`
#         returns 141 under pipefail when the pattern IS found, so a poll built
#         on it never goes true on a box busy enough to still be writing. Four
#         loops had it. `logs_match` counts instead, and no harness may go back.
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

log "7. logs_match answers, and nothing pipes podman logs into grep -q (pd-pfxf)"
# THE TRAP. `podman logs "$c" | grep -q PATTERN` under `set -o pipefail` returns
# 141 when the pattern IS found: grep -q exits on the first match, the pipe
# closes, and podman dies of SIGPIPE. So the condition reads as false exactly
# when it is true — and only once the log is long enough that the writer has not
# already finished, which is to say only on a busy box. Four polling loops in
# tests/{alarming,litestream,lake} had this form; inside `wait_until` it does not
# fail loudly, it silently never goes true and the gate burns its whole budget.
#
# §7a is the behaviour, over a real pipeline of the same shape rather than over a
# container (this file is hermetic): counting to EOF is immune, -q is not.
log "7a. the two forms, under pipefail, on a stream long enough to matter"
noisy() { seq 1 200000; echo MARKER; seq 1 200000; }
Q_RC=0
( set -o pipefail; noisy | grep -q MARKER ) || Q_RC=$?
C_RC=0
( set -o pipefail; [ "$(noisy | grep -c MARKER || true)" -gt 0 ] ) || C_RC=$?
# Not asserted as "-q returns 141": whether the writer is still writing is a
# race, and a test that demanded the bug reproduce would itself be flaky. What
# IS asserted is the half that must never be racy.
check "counting finds the marker, always" "0" "$C_RC"
check "…and logs_match is the counting form" "yes" \
	"$(grep -q 'grep -c' "${SCRIPT_DIR}/wait.sh" && echo yes || echo no)"
if [[ "$Q_RC" != 0 ]]; then
	echo "  note  the -q form returned ${Q_RC} on this run — the trap, reproduced"
fi

log "7b. the ratchet: one definition, and no harness rolls its own"
DEFS="$(harnesses | xargs grep -ln '^logs_match() {' /dev/null)"
check "one definition" "${REPO_DIR}/tests/lib/wait.sh" "$DEFS"
# And every caller sources it, for the reason §5 checks the same thing about
# wait_until: an undefined function is 127, `wait_until` reads 127 as "condition
# not met yet", and the gate spins to its deadline instead of saying
# "logs_match: command not found". Unsourced is silent here, not loud.
LM_CALLERS="$(harnesses | xargs grep -l 'logs_match ' /dev/null | grep -v '/tests/lib/wait.sh$')"
LM_UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/wait.sh"' "$f" || LM_UNSOURCED+="${f}"$'\n'
done <<<"$LM_CALLERS"
none "every logs_match caller sources tests/lib/wait.sh" "${LM_UNSOURCED%$'\n'}"
# Stated over the TREE, like §5 and §6, because the way this comes back is a new
# harness copying a neighbour that predates the helper.
#
# Over LOGICAL lines, not physical ones. A `grep -n` reads one line at a time,
# and the trap survives a backslash continuation perfectly well — which is not a
# hypothetical evasion, it is how the last one got through: tests/litestream/
# run.sh had `podman logs … | grep db=… \` on one line and `| grep -qE …` on the
# next, so this ratchet passed the day it was written while the offending
# pipeline sat two files away. Continuations are joined first, and the reported
# line number is where the logical line STARTS, which is where a reader has to
# go to fix it.
logical_lines() { # <file> -> "<file>:<first-lineno>:<line, continuations joined>"
	awk '{
		if (buf == "") { start = NR; buf = $0 } else { buf = buf " " $0 }
		if (buf ~ /\\$/) { sub(/\\$/, "", buf); next }
		print FILENAME ":" start ":" buf; buf = ""
	} END { if (buf != "") print FILENAME ":" start ":" buf }' "$1"
}
RAW="$(harnesses | while IFS= read -r f; do logical_lines "$f"; done |
	grep -E 'podman logs .*\|.*grep .*-[a-zA-Z]*q' |
	grep -vE '^[^:]*:[0-9]+:[[:space:]]*#')"
none "no harness pipes podman logs into grep -q" "$RAW"
# And the ratchet has been seen catching the two-line form, so it cannot quietly
# go back to reading physical lines: a fixture spelling the trap across a
# continuation must be found.
SPLIT_FIXTURE="$(mktemp)"
printf '%s\n' 'if podman logs "$c" 2>&1 | grep "db=x" \' '  | grep -qE "txid"; then :; fi' \
	>"$SPLIT_FIXTURE"
check "the two-line spelling of the trap is caught" "1" \
	"$(logical_lines "$SPLIT_FIXTURE" | grep -cE 'podman logs .*\|.*grep .*-[a-zA-Z]*q' || true)"
rm -f "$SPLIT_FIXTURE"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — harnesses wait for conditions, boundedly, in one place."
