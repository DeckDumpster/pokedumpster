#!/usr/bin/env bash
# wait.sh — wait for a CONDITION, never for a duration (pd-86er).
#
# WHY THIS FILE EXISTS
#
# A `sleep 8` in a test is two statements welded together, and only one of them
# is a claim. "The write reaches the replica" is the claim; "within 8 seconds on
# whatever machine this is" is a guess about the box. The guess is what makes a
# gate flaky (too short on a loaded runner) and what makes CI slow (too long on
# an idle one) — and it is the half nobody meant to assert. Every fixed sleep in
# this repo's harnesses paid the worst case on every run, whether it needed to or
# not.
#
# Polling keeps the claim and drops the guess: state the condition, bound how
# long you will wait for it, and return the instant it holds. Under deploy/ci.sh
# several polecats run the suite at once — this box has been measured at load
# average 7-11 — so the worst case has to stay generous while the common case
# stops paying for it.
#
# TWO PROPERTIES THAT ARE NOT NEGOTIABLE, and tests/lib/wait_test.sh asserts both:
#
#   * The condition is evaluated BEFORE the first sleep. A poll that sleeps
#     first has a fixed sleep hiding inside it.
#   * The wait is BOUNDED. A poll that never gives up turns a failing gate into
#     a hung CI job, which is strictly worse than the fixed sleep it replaced —
#     the failure stops being reported at all.
#
# On timeout `wait_until` returns 1 and says nothing. That is deliberate: the
# caller's own `check` line is what turns "the condition never held" into a FAIL
# with a name attached, and a diagnostic printed here would arrive before the
# section it belongs to. Callers that want a timeout to abort say so with `||`.
#
# Sourced, not executed.

# wait_until <timeout-seconds> <interval-seconds> <command...>
#
# Runs <command> until it exits 0, or until <timeout-seconds> have passed since
# the call. Returns 0 the moment the command succeeds, 1 if it never does.
# <command> may be a function, and its own output is left alone — redirect it at
# the call site if it is chatty.
#
# The order inside the loop is condition, then deadline, then sleep: the
# condition is always tried at least once (so a timeout of 0 still asks), and it
# is tried once more at the deadline rather than being abandoned a sleep short
# of it.
wait_until() {
	local timeout=$1 interval=$2
	shift 2
	local deadline=$(( SECONDS + timeout ))
	while :; do
		if "$@"; then return 0; fi
		[ "$SECONDS" -lt "$deadline" ] || return 1
		sleep "$interval"
	done
}

# logs_match <container> <grep args...> — does this container's log carry a line
# matching <pattern>? 0 if it does, 1 if it does not. The natural spelling of
# this is a one-liner, and the natural spelling is WRONG (pd-pfxf):
#
#     podman logs "$ctr" 2>&1 | grep -q 'msg="replica sync"'   # returns 141
#
# `grep -q` exits the instant it matches, which closes the pipe; `podman logs`
# is then killed by SIGPIPE and exits 141; and every harness here runs under
# `set -o pipefail`, so the PIPELINE reports 141 — a failure — precisely when the
# pattern was FOUND. Measured 2026-09-02 against a real sidecar whose log held 96
# matching lines. It is not deterministic either: with few enough lines the
# writer finishes before grep exits and the same code returns 0, so it passes on
# a quiet box and fails on a busy one, which is the signature this repo keeps
# mistaking for infrastructure flakiness.
#
# Counting reads the stream to EOF, so nothing is ever killed and the status is
# grep's alone. Four polling loops in tests/{alarming,litestream,lake} had the
# -q form; inside `wait_until` it does not fail loudly, it just never goes true,
# and the gate spends its whole budget before failing for some later reason.
#
# Everything is passed through to grep, so -F/-E/-i work as usual.
logs_match() {
	local ctr="$1"
	shift
	[ "$(podman logs "$ctr" 2>&1 | grep -c "$@" || true)" -gt 0 ]
}
