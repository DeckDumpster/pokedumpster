#!/usr/bin/env bash
# Failure diagnostics for the shell harnesses. Sourced, never executed.
#
# ── WHY THIS EXISTS (pd-8gjs) ───────────────────────────────────────────────
# tests/litestream/run.sh died with exit 127 partway through a CI run and
# printed NOTHING about why: no error, no failing command, no line number. The
# log simply stopped after a section banner and the next thing in it was the
# teardown chatter of the EXIT trap. Two habits conspired to erase the whole
# evidence trail:
#
#   * `cmd 2>/dev/null` at the call site. Silencing the noise a command makes
#     when it SUCCEEDS also silences the reason it FAILED — and under
#     `set -e` + `pipefail` that failure is what takes the entire run down.
#   * No ERR trap. Nothing named the failing line, so the EXIT trap's cleanup
#     output was the last thing in the log and the failure was invisible
#     behind it.
#
# So diagnostics here go to a duplicate of stderr taken before anything can
# redirect it (`$PD_DIAG_FD`). A call site's own `2>/dev/null` cannot eat
# them — which matters, because the call sites that need this most are exactly
# the ones that silence themselves.
#
# Usage:
#   . "${REPO_DIR}/tests/lib/diagnostics.sh"
#   diag_init                       # open the fd, install the ERR trap
#   diag "something worth saying"   # unswallowable message
#   diag_run <label> cmd args...    # run cmd; on failure print what it said
#
# tests/lib/diagnostics_test.sh proves each of those behaviours. It needs no
# container and no network, so it runs in under a second as an early CI gate.

# An unswallowable line. Falls back to plain stderr if diag_init has not run.
diag() { printf '%s\n' "$*" >&"${PD_DIAG_FD:-2}"; }

# The exact source text of diag_run's final statement. `set -E` fires the ERR
# trap once per frame a non-zero status propagates through, so without this the
# report for a wrapped command is preceded by one whose "command" is that bare
# `return` — plumbing, in the place where the failing call site should be.
# Keep this string and diag_run's last line identical; case 8 of
# tests/lib/diagnostics_test.sh fails if they ever drift apart.
PD_DIAG_RETURN_STMT='return "$rc"'

# Reports whatever `set -e` is about to die on, before the EXIT trap speaks.
pd_diag_on_err() {
	# errtrace makes this reachable from inside itself; one report is enough.
	[[ -n ${PD_DIAG_IN_ERR:-} ]] && return 0
	local PD_DIAG_IN_ERR=1
	local rc=$1 src=$2 line=$3 cmd=$4 i
	# Say nothing about diag_run's own plumbing. The wrapper has already printed
	# the command, its status and everything it wrote to stderr; the statements
	# that carry that status back out are not a second failure, and a report
	# whose "command" reads `return "$rc"` buries the line the reader wants.
	[[ ${FUNCNAME[1]:-} == diag_run ]] && return 0
	[[ $cmd == "diag_run "* || $cmd == "$PD_DIAG_RETURN_STMT" ]] && return 0
	diag ''
	diag "!! FAILED  ${src}:${line}"
	diag "!!   command : ${cmd}"
	diag "!!   status  : ${rc}"
	for ((i = 1; i < ${#FUNCNAME[@]}; i++)); do
		diag "!!   called from ${FUNCNAME[i]}() at ${BASH_SOURCE[i]}:${BASH_LINENO[i - 1]}"
	done
	return 0
}

diag_init() {
	# Taken once, and before any redirection in the script proper, so that a
	# `2>/dev/null` further down cannot reach it.
	if [[ -z ${PD_DIAG_FD:-} ]]; then
		exec {PD_DIAG_FD}>&2
	fi
	# `set -E` (errtrace) is what makes the trap worth having: without it ERR
	# is not inherited by functions, command substitutions or subshells, which
	# is where these harnesses do nearly all of their work.
	set -E
	trap 'pd_diag_on_err "$?" "${BASH_SOURCE[0]}" "$LINENO" "$BASH_COMMAND"' ERR
}

# Run a command with its stderr captured. Silent when it succeeds — so no call
# site ever needs a `2>/dev/null` of its own — and on failure prints the
# command, its exit status and every line it wrote to stderr. Returns the
# command's status, so `set -e` still behaves exactly as it did before.
diag_run() {
	local label=$1
	shift
	local err rc=0 line
	err="$(mktemp "${TMPDIR:-/tmp}/pd-diag.XXXXXX")"
	"$@" 2>"$err" || rc=$?
	if [[ $rc -ne 0 ]]; then
		diag ''
		diag "!! ${label} failed (status ${rc}): $*"
		if [[ -s $err ]]; then
			while IFS= read -r line; do diag "!!   ${label} | ${line}"; done <"$err"
		else
			# The observed pd-8gjs failure looked exactly like this: a bare
			# 127 with nothing said. Saying so is still better than silence.
			diag "!!   ${label} wrote nothing to stderr"
		fi
	fi
	rm -f "$err"
	return "$rc"
}
