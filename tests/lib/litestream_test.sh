#!/usr/bin/env bash
# Self-test + ratchet: "does this replica hold data?" has THREE answers, and no
# harness may go back to asking it with two (pd-reyy, pd-nt1k).
#
# `litestream ltx -level all <url>` prints a COLUMN HEADER and exits 0 for a
# prefix that has never been written to, and prints NOTHING and exits non-zero
# when it cannot reach S3 at all. So the obvious predicate — `ltx … | grep -q .`
# — is inverted at both ends: an empty replica reads as full, an unreachable
# bucket reads as empty. Both halves have cost a gate; tests/lib/litestream.sh
# has the full account.
#
# §1-§4 drive the real functions against RECORDED `ltx` output, captured from
# litestream v0.5.16 against a throwaway MinIO. Recorded rather than live
# because the three outcomes are what is being asserted, and two of them (an
# empty prefix, an unreachable endpoint) are states a live bucket has to be
# manoeuvred into — the recorded form states all three in the same shape and
# runs in a second.
#
# §5-§6 assert on the TREE. That is the half that matters: pd-nt1k fixed this
# predicate in tests/alarming/run.sh and the identical line stayed in two other
# harnesses for months, which is exactly what a per-file fix does.
#
# Hermetic — no podman, no network, no litestream — so deploy/ci.sh runs it in
# the lint tier beside tests/lib/ports_test.sh and tests/lib/wait_test.sh.
#
#   bash tests/lib/litestream_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/litestream.sh
. "${SCRIPT_DIR}/litestream.sh"

# No real query is made, so nothing has to wait for one.
LTX_QUERY_RETRY_SECONDS=0

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}
none() { # none <label> <lines> — the assertion is that there are none
	if [[ -z "$2" ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		printf '%s\n' "$2" | sed 's/^/          /'
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# ── the recorded output, verbatim from litestream v0.5.16 ───────────────────
# A replica holding two LTX files (level 0 and the level-9 snapshot).
LTX_WITH_DATA='level  min_txid          max_txid          size  created
0      0000000000000001  0000000000000001  567   2026-09-03T22:47:54Z
9      0000000000000001  0000000000000001  567   2026-09-03T22:47:54Z'
# A prefix nothing has ever written to. THE HEADER IS THE WHOLE OUTPUT, and the
# exit status is 0 — this line is the bug.
LTX_EMPTY='level  min_txid  max_txid  size  created'
# A query that could not be made. Nothing on stdout; litestream's own message on
# stderr, wrapped over two lines the way it really arrives.
LTX_ERROR='Error: operation error S3: ListObjectsV2, exceeded maximum number of attempts, 10,
https response error StatusCode: 0, request send failed, dial tcp 127.0.0.1:1: connect: connection refused'

# A stub runner, in the shape the library calls one: `<runner> ltx -level all <url>`.
# What it does is driven by STUB_MODE, and every call is counted so §4 can prove
# the retry happened rather than assuming it.
#
# The count lives in a FILE, not a variable. Every caller of this reaches it
# through `$(replica_state …)`, and a command substitution is a subshell — a
# counter incremented in one is gone by the time the assertion reads it, which
# is a stub that silently measures nothing.
STUB_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pd-ltxtest.XXXXXX")"
trap 'rm -rf "$STUB_DIR"' EXIT
STUB_COUNT="${STUB_DIR}/calls"
stub_calls() { cat "$STUB_COUNT"; }
stub_ls() {
	local n
	n=$(( $(cat "$STUB_COUNT") + 1 ))
	printf '%s' "$n" >"$STUB_COUNT"
	if [[ $n -le $STUB_FAIL_FIRST ]]; then
		printf '%s\n' "$LTX_ERROR" >&2
		return 1
	fi
	case "$STUB_MODE" in
		data) printf '%s\n' "$LTX_WITH_DATA" ;;
		empty) printf '%s\n' "$LTX_EMPTY" ;;
		error) printf '%s\n' "$LTX_ERROR" >&2; return 1 ;;
	esac
}
reset_stub() { printf '0' >"$STUB_COUNT"; STUB_MODE="$1"; STUB_FAIL_FIRST="${2:-0}"; }
reset_stub data

log "1. the recorded fixtures are what the old predicate got wrong"
# Stated over the fixtures themselves, so §2's expectations are anchored to a
# real litestream and not to this file's opinion of one. If either line stops
# being true of the tool, these two fail and say so.
check "an EMPTY replica still prints output — 'any output' means full" "yes" \
	"$([[ -n "$LTX_EMPTY" ]] && echo yes || echo no)"
check "and it carries no TXID, which is what distinguishes it" "0" \
	"$(printf '%s\n' "$LTX_EMPTY" | grep -cE '\b[0-9a-fA-F]{16}\b')"
check "a replica holding data carries TXIDs" "2" \
	"$(printf '%s\n' "$LTX_WITH_DATA" | grep -cE '\b[0-9a-fA-F]{16}\b')"

log "2. replica_state gives all three answers"
reset_stub data
check "a replica holding LTX files" "data" "$(replica_state stub_ls s3://b/x)"
reset_stub empty
check "a prefix never written to is EMPTY, not data" "empty" "$(replica_state stub_ls s3://b/x)"
reset_stub error
STATE="$(replica_state stub_ls s3://b/x)"
check "a failed query is an ERROR, not an absence" "error" "${STATE%%:*}"
# ...and it carries litestream's own words on the same line, so the FAIL line a
# caller prints says WHY. pd-reyy's six occurrences each reported `got no` and
# nothing else, which is why each one was re-run instead of read.
check "and it quotes litestream" "1" \
	"$(printf '%s' "$STATE" | grep -c 'connection refused' || true)"

log "3. replica_holds_data is true for exactly one of them"
reset_stub data
check "data" "yes" "$(replica_holds_data stub_ls s3://b/x && echo yes || echo no)"
reset_stub empty
check "empty" "no" "$(replica_holds_data stub_ls s3://b/x && echo yes || echo no)"
# The half pd-reyy is: an unreachable bucket must not be reported as a replica
# that is gone. It is not `data`, so the predicate is false — but a CALLER that
# needs to tell the two apart has replica_state, and §2 proves it can.
reset_stub error
check "error is not data either" "no" "$(replica_holds_data stub_ls s3://b/x && echo yes || echo no)"

log "4. the QUERY is retried; the ANSWER is not"
# The fix for pd-reyy in one assertion: a query that fails and then succeeds
# answers with what it found, not with the failure.
reset_stub data 2
check "two failures then an answer still reports the data" "data" \
	"$(replica_state stub_ls s3://b/x)"
reset_stub data 2
replica_state stub_ls s3://b/x >/dev/null
check "and it took three attempts to get there" "3" "$(stub_calls)"
# The other direction, so the retry cannot be an unbounded one: an unreachable
# bucket is reported after a bounded number of attempts rather than polled
# forever. Against a LITERAL, and against an overridden budget rather than the
# default — comparing the count to $LTX_QUERY_ATTEMPTS would be the variable
# measured against itself, which passes for any value including a wrong one.
check "the shipped budget is three attempts" "3" "$LTX_QUERY_ATTEMPTS"
reset_stub error
( LTX_QUERY_ATTEMPTS=2; replica_state stub_ls s3://b/x >/dev/null )
check "a query that never answers stops after the budget, whatever it is" "2" "$(stub_calls)"
# And the thing the retry must NOT do. An empty replica is an ANSWER — re-asking
# it would spend the budget on every genuinely-empty prefix and, worse, would
# turn "the data is gone" into something a slow bucket could recover from.
reset_stub empty
replica_state stub_ls s3://b/x >/dev/null
check "an EMPTY answer is asked for exactly once" "1" "$(stub_calls)"

# The shell harnesses, minus this file and the library — both are written to
# talk about the thing they detect.
harnesses() {
	find "${REPO_DIR}/tests" "${REPO_DIR}/deploy" -name '*.sh' -type f \
		! -path "${REPO_DIR}/tests/lib/litestream_test.sh" \
		! -path "${REPO_DIR}/tests/lib/litestream.sh" | sort
}

log "5. no harness reads 'ltx printed something' as 'the replica holds data'"
# The exact shape that failed, three times over:
#   ls_run ltx -level all "$1" 2>/dev/null | grep -q .
# Any `ltx` listing whose result is tested for mere non-emptiness. `grep -q .`,
# `[ -n "$(…)" ]`, `-z`, `wc -l` — the test is on the pipeline, not on the
# spelling, so it catches the next one too. A listing greped for a TXID or an
# RFC3339 timestamp (what tests/alarming/run.sh and deploy/backup-check.sh do)
# is the CORRECT question and passes.
# Comment lines are excluded, so a file may go on explaining the predicate it no
# longer uses — the same exemption tests/lib/wait_test.sh §6 makes for `sleep`.
EMPTINESS="$(
	{
		harnesses | xargs grep -nE 'ltx .*\|[[:space:]]*grep -q[[:space:]]+\.' /dev/null
		harnesses | xargs grep -nE '(-n|-z)[[:space:]]+"?\$\(.*ltx .*\)' /dev/null
		harnesses | xargs grep -nE 'ltx .*\|[[:space:]]*wc -l' /dev/null
	} | grep -vE '^[^:]*:[0-9]+:[[:space:]]*#'
)"
none "every 'holds data' question looks at the CONTENT of the listing" "$EMPTINESS"

log "6. replica_state is defined once, and every caller sources it"
# Same ratchet as tests/lib/ports_test.sh §8 and tests/lib/wait_test.sh §5, for
# the reason this file exists at all: pd-nt1k's fix reached one harness of three
# because it was written INTO that harness. A copy is how a fix stops travelling.
DEFS="$(harnesses | xargs grep -ln '^replica_state() {' /dev/null; \
	grep -ln '^replica_state() {' "${REPO_DIR}/tests/lib/litestream.sh")"
check "one definition" "${REPO_DIR}/tests/lib/litestream.sh" "$DEFS"
CALLERS="$(harnesses | xargs grep -lE 'replica_state |replica_holds_data ' /dev/null)"
UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/litestream.sh"' "$f" || UNSOURCED+="${f}"$'\n'
done <<<"$CALLERS"
none "every caller sources tests/lib/litestream.sh" "${UNSOURCED%$'\n'}"

log "7. a point-in-time marker is named through pitr_marker, never by hand"
# THE BUG: tests/litestream/run.sh §4b took its registry marker with a bare
# `date -u`, two lines after a wait that had just moved it onto the instant the
# replica became restorable. `date` truncates DOWN to the start of its second,
# so the marker landed BEFORE the oldest instant covered and Litestream answered
# `timestamp does not exist` — surfacing as `expected 1, got <no restored db>`.
# Immune to retrying, and reachable only when replication is fast, so a local
# run passed 61/0 and CI went red on the same commit.
#
# §3 of that same file had the derivation, and the `sleep 1` that follows from
# it, in a 20-line comment — which protected §3 and nothing else. This is that
# rule as the only way to spell a marker.

# The behaviour, before the tree assertion: it must LEAD, or it is decorative.
T_BEFORE=$(date -u +%s)
STAMP="$(pitr_marker 1)"
T_AFTER=$(date -u +%s)
check "pitr_marker leaves at least a second before naming an instant" "yes" \
	"$([ "$((T_AFTER - T_BEFORE))" -ge 1 ] && echo yes || echo no)"
check "and emits one RFC3339 second-resolution UTC timestamp" "yes" \
	"$(grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' <<<"$STAMP" \
		&& echo yes || echo no)"
# The lead is a parameter, because recreate.sh's markers use 2s and run.sh's
# use 1s, and a helper that forced one of them would have to be worked around.
T_BEFORE=$(date -u +%s)
pitr_marker 2 >/dev/null
check "the lead is the caller's to choose" "yes" \
	"$([ "$(($(date -u +%s) - T_BEFORE))" -ge 2 ] && echo yes || echo no)"

# THE TREE. Any harness that does a `-timestamp` restore must name that instant
# through the helper. Comment lines are excluded, as in §5.
PITR_FILES="$(harnesses | xargs grep -lE '[-]timestamp' /dev/null || true)"
BYHAND=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	hits="$(grep -nE '=\$\(date -u \+%Y-%m-%dT%H:%M:%SZ\)' "$f" \
		| grep -vE '^[0-9]+:[[:space:]]*#' || true)"
	[[ -n "$hits" ]] && BYHAND+="${f}:${hits}"$'\n'
done <<<"$PITR_FILES"
none "no PITR harness names a marker with a bare date" "${BYHAND%$'\n'}"

# And the same "defined once, every caller sources it" ratchet §6 makes, because
# a second copy of pitr_marker is how this fix stops travelling.
DEFS="$(harnesses | xargs grep -ln '^pitr_marker() {' /dev/null; \
	grep -ln '^pitr_marker() {' "${REPO_DIR}/tests/lib/litestream.sh")"
check "one definition of pitr_marker" "${REPO_DIR}/tests/lib/litestream.sh" "$DEFS"
CALLERS="$(harnesses | xargs grep -lE 'pitr_marker' /dev/null || true)"
UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/litestream.sh"' "$f" || UNSOURCED+="${f}"$'\n'
done <<<"$CALLERS"
none "every pitr_marker caller sources tests/lib/litestream.sh" "${UNSOURCED%$'\n'}"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — a replica has three states, not two, and no harness asks with two."
