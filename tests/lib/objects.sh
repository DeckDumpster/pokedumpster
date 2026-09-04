#!/usr/bin/env bash
# objects.sh — an object-store listing that cannot come back EMPTY by accident
# (pd-cxq4).
#
# WHY THIS FILE EXISTS
#
# Seven gates ask a real bucket what is in it by running `mc ls` in a throwaway
# container over a user-defined network. Every one of them wrote that as
#
#     mc_root ls --recursive "x/${BUCKET}" 2>/dev/null | ... | grep -c .
#
# which has no way to say "the listing failed". A podman run that dies under
# load produces the same thing an empty bucket does: no lines. On 2026-08-26,
# in a CI run with seventeen gates going two at a time, one such run came back
# with nothing and tests/lake/deletion.sh read it as "the tenant zone holds 0
# objects" — three lines before the identical call listed the zone perfectly.
# That was the flake. It cost a red run.
#
# THE DIRECTION THAT MATTERS IS THE OTHER ONE. Half of what these gates assert
# is that a prefix is EMPTY: alice's partition after the drop, that nothing was
# put back under it, that nothing was replicated to the bucket root, that no
# tenant-keyed object sits outside `tenant/`. A listing that dies satisfies
# every one of those, silently, and the gate goes GREEN having proved an erasure
# it never looked at. The flake and the false green are one bug; only the flake
# is loud, and eight of those assertions were live across six gates when this
# landed.
#
# So a listing is trusted only when these hold, checked on every call:
#
#   1. the command exited 0; and, WHEN THE CALLER NAMED A SENTINEL,
#   2. the sentinel key is among what it printed.
#
# Rule 1 is what the flake needed: a container that dies exits non-zero, and
# nothing was looking. It is also the rule that a partial answer breaks — a
# listing that printed half a bucket and then failed is not half an answer, and
# a caller counting lines cannot tell. Rule 2 catches what no exit status
# reports: a run that exits 0 having listed nothing, or having listed some other
# bucket.
#
# The sentinel is how a caller states an invariant it can actually promise: a
# key it seeded before listing anything and asserts is still there at teardown.
# The lake gates all have one, which is why they list from the BUCKET ROOT and
# filter rather than listing the prefix they care about — an empty PREFIX is a
# real answer and half of what they assert, while an empty BUCKET is not an
# answer at all, and naming the sentinel is what separates the two. (Listing
# from the root is also what those gates already did for their own reason: mc
# prints keys relative to the prefix it is given, and a key with its own prefix
# chopped off is not a key any of this code could be handed.)
#
# A caller with no such key passes "" and gets rule 1 alone. That is not an
# opt-out to reach for: it is for the gates where an empty store IS a legitimate
# observation — tests/litestream/run.sh listing a bucket whose contents are the
# very thing under test, where "the replica is empty" is a finding to report and
# not a listing to distrust. Claiming a sentinel there would turn a real failure
# into a misleading one.
#
# A transient failure is retried, bounded, and then FATAL. That is the same
# trade crates/pkdump-ingest/src/retry.rs makes at the one place a request is
# executed: retrying transport is not fallback logic, because when the budget is
# spent the original error propagates rather than a default. The one thing this
# function must never do is return successfully with nothing.
#
# It returns 1 rather than exiting, because it is meant to be called in a
# command substitution and an `exit` there would only kill the subshell — which
# is the exact shape of the bug. Callers assign and `|| die`, at the top level,
# where a failure is fatal:
#
#     LISTING="$(object_store_ls "$SENTINEL" mc_root ls --recursive "x/$BUCKET")" ||
#         die "the bucket listing could not be trusted — see above"
#
# Sourced, not executed.

_OBJECTS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_OBJECTS_REPO_DIR="$(cd "${_OBJECTS_LIB_DIR}/../.." && pwd)"
# shellcheck source=tests/lib/wait.sh
. "${_OBJECTS_REPO_DIR}/tests/lib/wait.sh"

# How long a listing may go on being untrustworthy before the gate gives up.
# Bounded for the reason every poll here is bounded: an unbounded retry turns a
# broken object store into a hung CI job instead of a failing one.
OBJECT_LS_TIMEOUT=${OBJECT_LS_TIMEOUT:-6}

# The last attempt's captured stdout, its exit status, and the file its stderr
# went to. Globals because wait_until runs its condition in THIS shell, which is
# the whole reason the retry can report what the failures actually said.
OBJECT_LS_OUT=""
OBJECT_LS_STATUS=0

_object_ls_attempt() {
	OBJECT_LS_STATUS=0
	OBJECT_LS_OUT="$("${_OBJECT_LS_CMD[@]}" 2>"$_OBJECT_LS_ERR")" || OBJECT_LS_STATUS=$?
	_OBJECT_LS_TRIES=$((_OBJECT_LS_TRIES + 1))
	[[ "$OBJECT_LS_STATUS" -eq 0 ]] || return 1
	[[ -n "$_OBJECT_LS_SENTINEL" ]] || return 0
	grep -qF -- "$_OBJECT_LS_SENTINEL" <<<"$OBJECT_LS_OUT"
}

# object_store_ls <sentinel-key> <command...>
#
# Runs <command>, which must print an object listing on stdout, and echoes that
# listing. Returns 0 only when the command exited 0 and — where <sentinel-key>
# is not empty — printed that key among what it returned. Otherwise it retries
# until the budget is spent and then returns 1, having explained on stderr what
# went wrong and what the command said: the diagnosis pd-cxq4 could not make,
# because the stderr had been thrown away.
#
# Pass "" for <sentinel-key> only where an empty store is a legitimate
# observation. The exit status is still checked.
object_store_ls() {
	local sentinel="$1"
	shift
	_OBJECT_LS_SENTINEL="$sentinel"
	_OBJECT_LS_CMD=("$@")
	_OBJECT_LS_TRIES=0
	_OBJECT_LS_ERR="$(mktemp)"

	if wait_until "$OBJECT_LS_TIMEOUT" 1 _object_ls_attempt; then
		printf '%s\n' "$OBJECT_LS_OUT"
		rm -f "$_OBJECT_LS_ERR"
		return 0
	fi

	{
		echo
		echo "!! THE OBJECT LISTING FAILED, and an empty listing is not an empty store (pd-cxq4)."
		echo "   command  : ${_OBJECT_LS_CMD[*]}"
		echo "   attempts : ${_OBJECT_LS_TRIES} in ${OBJECT_LS_TIMEOUT}s; last exit status ${OBJECT_LS_STATUS}"
		if [[ "$OBJECT_LS_STATUS" -eq 0 && -z "$OBJECT_LS_OUT" ]]; then
			echo "   why      : it exited 0 and printed NOTHING, and the caller promised the"
			echo "              sentinel below would be there. A bucket that cannot even show the"
			echo "              object seeded into it before any of this ran was not listed."
			echo "   sentinel : ${_OBJECT_LS_SENTINEL}"
		elif [[ "$OBJECT_LS_STATUS" -eq 0 ]]; then
			echo "   why      : it exited 0, but the sentinel key was not among the $(grep -c . <<<"$OBJECT_LS_OUT") line(s) it returned."
			echo "   sentinel : ${_OBJECT_LS_SENTINEL}"
			echo "              That key is seeded before anything is listed and is still there at"
			echo "              teardown, so a listing without it did not see the bucket."
		fi
		echo "   stderr   :"
		sed 's/^/     /' "$_OBJECT_LS_ERR"
	} >&2
	rm -f "$_OBJECT_LS_ERR"
	return 1
}
