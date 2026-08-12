#!/usr/bin/env bash
#
# Run independent CI gates concurrently, capped, with the disk floor checked
# before every dispatch (pd-2nl9, item 4 of pd-6onp).
#
# Sourced by deploy/ci.sh. Defines functions and runs nothing.
#
# ── WHAT THIS IS FOR ────────────────────────────────────────────────────────
#
# Eleven of deploy/ci.sh's gates stand up their own containers and share
# nothing: litestream, drill, alarming, recreate, upgrade, tenant-header,
# schema-version, the three lake gates and refresh. Each already derives every
# name it uses — network, container, volume, image tag, unit prefix, temp dir —
# from its own prefix plus a per-checkout hash, because concurrent polecats
# already run whole CI suites beside each other. That isolation is what makes
# running them at the same time a scheduling change rather than a correctness
# one, and tests/ci/parallel_test.sh asserts the prefixes stay distinct.
#
# They are latency, not throughput: the box has four cores and the gates spend
# their time waiting on containers to come up, replicate, and stop. Sequentially
# they add up to most of a CI run's wall clock.
#
# ── THE CAP IS A RESOURCE DECISION, NOT A TUNING KNOB ───────────────────────
#
# Three at a time by default, four at the very most. This is a 15G box with
# four cores that also runs prod, and each of these gates stands up two or three
# containers — a MinIO, sometimes a JVM (Nessie), sometimes a whole pkdump
# instance with a Litestream sidecar. Above four the failures stop looking like
# resource exhaustion and start looking like flaky gates, which is the worst
# possible outcome for a suite whose job is to be believed.
#
# PKDUMP_CI_JOBS lowers it — 1 is a serial run, which is the first thing to try
# when a parallel run misbehaves. A value above the ceiling is clamped out loud
# rather than honoured silently.
#
# ── THE DISK FLOOR IS CHECKED BEFORE EVERY DISPATCH ─────────────────────────
#
# deploy/ci.sh checks the floor once at startup, which was enough when one gate
# ran at a time and the previous gate's teardown had already returned the disk.
# Three at a time can be three images, three volumes and three MinIO stores
# deep at once, so the check moves inside the loop: before every job is
# launched, never once per batch.
#
# Below the floor with gates still running, the runner HOLDS — it launches
# nothing new and waits for a running gate to finish and return its space, then
# asks again. That is the whole reason it is a hold and not an abort: the
# thing most likely to fix a low-disk moment is the gate that is about to tear
# itself down. Below the floor with NOTHING running, there is nothing left to
# wait for, and the run fails naming the gates that never started rather than
# filling the disk to find out.
#
# It is the same guard deploy/diskcheck.sh --floor installs everywhere else,
# invoked as a subprocess, so PKDUMP_DISK_FLOOR_GB moves both. Nothing here
# re-derives a threshold.
#
# ── A FAILURE IS NEVER MASKED ───────────────────────────────────────────────
#
# Every queued gate runs. A gate that fails does not cancel the others, and the
# others passing does not soften it: each gate's full output is printed under
# its own name, the summary table marks it FAIL, and pkdump_par_run returns
# non-zero, which under deploy/ci.sh's `set -e` ends the run red. Sequentially
# the first failure stopped the suite; in parallel the wave finishes, so one
# red run now reports every gate that was already broken instead of only the
# earliest one.
#
# ── OUTPUT ──────────────────────────────────────────────────────────────────
#
# Concurrent gates cannot share a terminal without shredding each other's
# output, and a shredded CI log is a gate nobody can diagnose. Each gate writes
# to its own file and the whole file is printed, verbatim and contiguous, at the
# moment that gate finishes. Nothing is summarised away, nothing is dropped, and
# a line printed by one gate can never land inside another's.

# The queue. pkdump_par_reset clears it; pkdump_par_add appends.
PKDUMP_PAR_LABELS=()
PKDUMP_PAR_CMDS=()

# Live children, keyed by pid, so a caller's EXIT trap can take them down with
# it — see pkdump_par_kill_all. Entries are removed as gates finish: a pid the
# kernel has already recycled belongs to somebody else, and signalling it would
# be the worst kind of bug to ship in a cleanup path.
declare -A PKDUMP_PAR_LIVE=()

_PKDUMP_PAR_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The filesystems the floor is measured on. deploy/ci.sh sets both of the ones
# it cares about ($HOME and the container store); the default is the one the
# rest of the tree defaults to.
PKDUMP_PAR_DISK_PATHS=("$HOME")

# The floor gate itself, as a path to a script taking `--floor <path>...`.
# Overridable so tests/ci/parallel_test.sh can drive the hold branch from a stub
# whose answer changes mid-run — a real disk cannot be made to do that on cue,
# and the branch that waits is the one worth proving. §4 of that test runs the
# REAL script against an impossible floor, so both halves are covered.
PKDUMP_PAR_DISKCHECK="${PKDUMP_PAR_DISKCHECK:-${_PKDUMP_PAR_LIB_DIR}/diskcheck.sh}"

# How many gates at once. See the header: 3 by default, 4 is the ceiling.
PKDUMP_PAR_JOBS_CEILING=4

pkdump_par_reset() {
	PKDUMP_PAR_LABELS=()
	PKDUMP_PAR_CMDS=()
	PKDUMP_PAR_LIVE=()
}

# pkdump_par_add <label> <command> [args...]
#
# The label names the gate in the log, the summary and its own output file, so
# it is restricted to what is safe in all three. argv is quoted here rather than
# left as a string for the caller to get right.
pkdump_par_add() {
	local label="$1"
	shift
	case "$label" in
	'' | *[!a-z0-9-]*)
		echo "pkdump_par_add: '${label}' is not a usable gate label ([a-z0-9-]+)" >&2
		return 2
		;;
	esac
	[ "$#" -gt 0 ] || {
		echo "pkdump_par_add: ${label} has no command" >&2
		return 2
	}
	PKDUMP_PAR_LABELS+=("$label")
	PKDUMP_PAR_CMDS+=("$(printf '%q ' "$@")")
}

pkdump_par_count() { printf '%s\n' "${#PKDUMP_PAR_LABELS[@]}"; }

# Take down anything still running. For a caller's EXIT trap: a cancelled CI run
# would otherwise leave gates behind still holding containers. SIGTERM, not
# SIGKILL, so each gate's own EXIT trap gets to remove what it created.
pkdump_par_kill_all() {
	local pid
	for pid in "${!PKDUMP_PAR_LIVE[@]}"; do
		kill -TERM "$pid" 2>/dev/null || true
	done
}

# Is there room on disk to start one more gate? Quiet when the answer is yes —
# the floor is reported once by deploy/ci.sh at startup and does not need
# repeating eleven times.
_pkdump_par_disk_ok() {
	local out rc=0
	out="$(bash "$PKDUMP_PAR_DISKCHECK" --floor "${PKDUMP_PAR_DISK_PATHS[@]}" 2>&1)" || rc=$?
	[ "$rc" -eq 0 ] && return 0
	printf '%s\n' "$out" >&2
	return 1
}

# Print one finished gate's whole log under its own name, and record the result.
# Reads and writes pkdump_par_run's locals — bash scopes dynamically, and
# keeping them local there is what makes the runner re-entrant.
_pkdump_par_finish() { # _pkdump_par_finish <pid> <exit status>
	local pid="$1" frc="$2"
	local label="${_PAR_LABEL_OF[$pid]}"
	local secs=$(($(date +%s) - ${_PAR_START_OF[$pid]}))
	local status=ok

	unset "PKDUMP_PAR_LIVE[$pid]"
	running=$((running - 1))
	if [ "$frc" -ne 0 ]; then
		status=FAIL
		failed+=("$label")
	fi

	echo ""
	echo "──────── ${label}: ${status} (${secs}s, exit ${frc}) ────────"
	cat "${logdir}/${label}.log" 2>/dev/null || true
	echo "──────── end ${label} ────────"
	results+=("$(printf '%-4s  %-16s %5ss' "$status" "$label" "$secs")")
}

# Block until at least one gate finishes, then harvest it.
_pkdump_par_reap() {
	local pid="" frc=0
	wait -n -p pid || frc=$?
	# `wait -n` without a pid to report means there was nothing to wait for,
	# which the caller's `running` counter says cannot happen. Treat it as the
	# bug it would be rather than looping forever on it.
	[ -n "$pid" ] || {
		echo "pkdump_par_run: wait -n returned no pid with ${running} gate(s) running" >&2
		return 1
	}
	_pkdump_par_finish "$pid" "$frc"
}

# Run the queue. Returns 0 only if every queued gate ran and passed.
pkdump_par_run() {
	local total="${#PKDUMP_PAR_LABELS[@]}"
	if [ "$total" -eq 0 ]; then
		echo "    (no gates queued — nothing to run in parallel)"
		return 0
	fi

	local cap="${PKDUMP_CI_JOBS:-3}"
	case "$cap" in
	'' | *[!0-9]*)
		echo "ERROR: PKDUMP_CI_JOBS='${cap}' is not a positive integer." >&2
		return 1
		;;
	esac
	[ "$cap" -ge 1 ] || {
		echo "ERROR: PKDUMP_CI_JOBS must be at least 1." >&2
		return 1
	}
	if [ "$cap" -gt "$PKDUMP_PAR_JOBS_CEILING" ]; then
		echo "    PKDUMP_CI_JOBS=${cap} is above the ${PKDUMP_PAR_JOBS_CEILING} this box is sized for — using ${PKDUMP_PAR_JOBS_CEILING}."
		cap="$PKDUMP_PAR_JOBS_CEILING"
	fi

	local logdir
	logdir="$(mktemp -d "${TMPDIR:-/tmp}/pkdump-par.XXXXXX")"

	local -A _PAR_LABEL_OF=() _PAR_START_OF=()
	local -a results=() failed=()
	local next=0 running=0 held=0
	local t0
	t0="$(date +%s)"

	echo "    ${total} gate(s), ${cap} at a time, disk floor checked before each one."

	while [ "$next" -lt "$total" ] || [ "$running" -gt 0 ]; do
		while [ "$next" -lt "$total" ] && [ "$running" -lt "$cap" ]; do
			if ! _pkdump_par_disk_ok; then
				if [ "$running" -gt 0 ]; then
					echo "    [hold]  ${PKDUMP_PAR_LABELS[$next]} — below the disk floor; waiting for a running gate to finish"
					held=$((held + 1))
					break
				fi
				# Nothing running means nothing is going to give the space
				# back. Say which gates never started; a run that stops here
				# is red, and the reason is a disk, not a gate.
				echo "" >&2
				echo "ERROR: below the disk floor with no gate running — refusing to start more." >&2
				echo "       ${total} queued, $((total - next)) never started:" >&2
				local i
				for ((i = next; i < total; i++)); do
					echo "         ${PKDUMP_PAR_LABELS[$i]}" >&2
				done
				rm -rf "$logdir"
				return 1
			fi

			local label="${PKDUMP_PAR_LABELS[$next]}"
			echo "    [start] ${label}"
			# </dev/null: a gate must never inherit the run's stdin and
			# block on it.
			(eval "${PKDUMP_PAR_CMDS[$next]}") >"${logdir}/${label}.log" 2>&1 </dev/null &
			local pid=$!
			_PAR_LABEL_OF[$pid]="$label"
			_PAR_START_OF[$pid]="$(date +%s)"
			PKDUMP_PAR_LIVE[$pid]=1
			next=$((next + 1))
			running=$((running + 1))
		done

		_pkdump_par_reap || {
			rm -rf "$logdir"
			return 1
		}
	done

	local elapsed=$(($(date +%s) - t0))
	local serial=0 line
	echo ""
	echo "    ── parallel gates: ${total} in ${elapsed}s (cap ${cap}) ──"
	for line in "${results[@]}"; do
		echo "       ${line}"
		serial=$((serial + $(printf '%s' "$line" | awk '{print $3+0}')))
	done
	echo "       ${serial}s of gate time in ${elapsed}s of wall clock."
	[ "$held" -eq 0 ] || echo "       ${held} dispatch(es) held by the disk floor."

	rm -rf "$logdir"

	if [ "${#failed[@]}" -gt 0 ]; then
		echo ""
		echo "    FAILED: ${failed[*]}" >&2
		# The caller wants the names for its own diagnostics line.
		PKDUMP_PAR_FAILED="${failed[*]}"
		return 1
	fi
	PKDUMP_PAR_FAILED=""
	return 0
}
