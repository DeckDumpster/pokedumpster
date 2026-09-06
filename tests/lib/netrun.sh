#!/usr/bin/env bash
# netrun.sh — a container that never RESOLVED its peer did not run (sp-pd-ci-green).
#
# WHY THIS FILE EXISTS
#
# The lake gates stand a MinIO and a Nessie up on a user-defined podman network
# and then launch job containers that reach them BY NAME. Under deploy/ci.sh
# seventeen gates run two at a time, each churning containers on its own
# network, and rootless podman serves every one of those names from a single
# per-user aardvark-dns process that is reconfigured and signalled every time a
# container appears or disappears. There is a window during that reload where a
# freshly-started container gets no answer at all.
#
# On 2026-09-05, run 33994136151, tests/lake/value_snapshots.sh hit it:
#
#   §3  Nessie up, warehouse s3://pdvalue-961d5c/lake        <- probe answered
#   §4  socket.gaierror: [Errno -3] Temporary failure in name resolution
#       ... host='pdvalue-nessie-961d5c', port=19120
#       !! the 2026-08-09 build failed
#
# §3's readiness probe is a container ON the network resolving that same name,
# and it had just succeeded. Seconds later the job could not resolve it. The box
# was not wedged — the `derive` gate stood up its own internal network and
# passed five seconds afterwards. The same commit had gone green 54 minutes
# earlier in run 33991135274, in the same slot, beside the same `prices` gate.
#
# EAI_AGAIN IS NOT NXDOMAIN, and the difference is the whole argument. `-3 /
# Temporary failure in name resolution` means the resolver got NO ANSWER from
# the DNS server. A name that had genuinely gone away — Nessie dead, the wrong
# network, a typo — answers `-2 / Name or service not known`. So this signature
# says something specific and checkable: the container never reached the
# catalog, which means it did no work and may be run again.
#
# WHAT THIS DOES NOT DO, because both directions have already cost a run here:
#
#   * It retries ONLY on that signature. Any other failure is the job's own
#     answer and is returned untouched, first time, unretried. That matters
#     more than it looks: these gates ASSERT non-zero statuses — a
#     value-snapshots run that skips a tenant must exit 2, and §5, §7 and §8
#     check exactly that. A wrapper that retried "a failure" would re-run those
#     deliberate partial runs and could turn an asserted 2 into something else.
#   * It never returns 0 having done nothing. When the budget is spent the
#     ORIGINAL exit status and the command's own output propagate, so an
#     existing `|| die` still fires and still says what it always said. This is
#     the trade crates/pkdump-ingest/src/retry.rs makes at the one place a
#     request is executed, and the one tests/lib/objects.sh makes for a bucket
#     listing: retrying transport is not fallback logic, because when the
#     budget is spent the error propagates rather than a default.
#   * It is LOUD. A retry that happened is printed, so a network that is really
#     failing shows up as repeated notices in the log instead of being quietly
#     absorbed into a passing run. A retry nobody can see is how a gate stops
#     being able to report the thing it exists to report.
#
# stdout and stderr are kept APART. Callers capture stdout (`TABLES=$(run_job
# python -c …)`) and some merge the streams themselves at the call site
# (`OUT=$(snapshot … 2>&1)`); a wrapper that merged them for everybody would put
# diagnostics inside a value the gate goes on to compare.
#
# Wired into tests/lake/value_snapshots.sh, which is where it was observed. The
# other lake gates launch job containers the same way and have the same
# exposure; adopting it there is a change with its own blast radius and is
# deliberately not made here.
#
# Sourced, not executed.

_NETRUN_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_NETRUN_REPO_DIR="$(cd "${_NETRUN_LIB_DIR}/../.." && pwd)"
# The bound is wait_until, so there is still one polling implementation.
# shellcheck source=tests/lib/wait.sh
. "${_NETRUN_REPO_DIR}/tests/lib/wait.sh"

# How long a peer name may go on being unresolvable before the gate gives up.
# Bounded for the reason every poll here is bounded: an unbounded retry turns a
# broken network into a hung CI job instead of a failing one. The window this
# exists for is the sub-second one around an aardvark-dns reload; the budget is
# generous because the box has been measured at load average 7-11 with two
# gates running.
NETRUN_TIMEOUT=${NETRUN_TIMEOUT:-45}
NETRUN_INTERVAL=${NETRUN_INTERVAL:-3}

# The signature of a container that never reached its peer. Deliberately narrow
# — every alternative spelling here is a resolver saying it could not get an
# answer, and none of them is something a job says about its own work.
NETRUN_UNRESOLVED_RE=${NETRUN_UNRESOLVED_RE:-'Temporary failure in name resolution|NameResolutionError|Failed to resolve|Name or service not known|no such host|Could not resolve host'}

# The last attempt's captured stdout and its exit status. Globals because
# wait_until runs its condition in THIS shell, which is what lets the retry
# report what the attempts actually said.
NETRUN_OUT=""
NETRUN_STATUS=0
NETRUN_TRIES=0

_netrun_unresolved() {
	if grep -qE "$NETRUN_UNRESOLVED_RE" "$_NETRUN_ERR"; then return 0; fi
	grep -qE "$NETRUN_UNRESOLVED_RE" <<<"$NETRUN_OUT"
}

# Returns 0 when there is a FINAL answer — the command succeeded, or failed for
# a reason of its own — and 1 only while the failure is "the peer's name did not
# resolve", which is the one thing worth asking again.
_netrun_attempt() {
	NETRUN_STATUS=0
	NETRUN_OUT="$("${_NETRUN_CMD[@]}" 2>"$_NETRUN_ERR")" || NETRUN_STATUS=$?
	NETRUN_TRIES=$((NETRUN_TRIES + 1))
	if [[ "$NETRUN_STATUS" -eq 0 ]]; then return 0; fi
	if _netrun_unresolved; then return 1; fi
	return 0
}

# netrun <command...>
#
# Runs <command>, returning its stdout on stdout, its stderr on stderr and its
# exit status as the status — unchanged, on the first attempt, for every
# outcome except one: a failure in which the command reported that it could not
# resolve a host name is retried until NETRUN_TIMEOUT is spent, after which the
# last attempt's status and output propagate exactly as if nothing had wrapped
# it.
netrun() {
	_NETRUN_CMD=("$@")
	_NETRUN_ERR="$(mktemp)"
	NETRUN_OUT=""
	NETRUN_STATUS=0
	NETRUN_TRIES=0

	local resolved=0
	if wait_until "$NETRUN_TIMEOUT" "$NETRUN_INTERVAL" _netrun_attempt; then
		resolved=1
	fi

	if [[ "$NETRUN_TRIES" -gt 1 ]]; then
		{
			echo "-- netrun: the peer name did not resolve; asked again."
			echo "   command  : ${_NETRUN_CMD[*]}"
			if [[ "$resolved" -eq 1 ]]; then
				echo "   outcome  : answered on attempt ${NETRUN_TRIES} (status ${NETRUN_STATUS})."
			else
				echo "   outcome  : still unresolved after ${NETRUN_TRIES} attempt(s) in ${NETRUN_TIMEOUT}s."
				echo "              Failing with the command's own status (${NETRUN_STATUS}) and output,"
				echo "              which follow. This is a network that is really broken, not the"
				echo "              aardvark-dns reload window this retry exists for."
			fi
		} >&2
	fi

	if [[ -n "$NETRUN_OUT" ]]; then printf '%s\n' "$NETRUN_OUT"; fi
	cat "$_NETRUN_ERR" >&2
	rm -f "$_NETRUN_ERR"
	return "$NETRUN_STATUS"
}
