#!/usr/bin/env bash
# Unit test for tests/lib/netrun.sh (sp-pd-ci-green).
#
# The library retries one thing and must retry nothing else, so both halves are
# asserted and the second is the one that matters:
#
#   §2  a command that fails because it could not resolve its peer IS asked
#       again, and the answer comes from the attempt that worked.
#   §3  a command that fails for ANY OTHER reason is NOT retried, and its
#       status and stderr arrive unchanged on the first attempt. §4 is the
#       specific case that made this worth writing down: the lake gates assert
#       that a partial run exits 2, so a 2 must pass through untouched and
#       uncounted. A wrapper that retried "a failure" would re-run the very
#       runs those sections are asserting about.
#   §5  the retry is BOUNDED, and when the budget is spent the command's own
#       status and output propagate. Returning 0 there would be the flake with
#       more code in it.
#   §6  stdout and stderr stay apart, because callers capture stdout and some
#       merge the streams themselves at the call site.
#   §7  THE RATCHET: one definition, and every caller sources it.
#
# Deliberately hermetic — no podman, no network — so deploy/ci.sh can run it in
# the lint tier beside tests/lib/objects_test.sh.
#
#   bash tests/lib/netrun_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=tests/lib/netrun.sh
. "${SCRIPT_DIR}/netrun.sh"

# Sub-second: the window this library exists for is an aardvark-dns reload, and
# the test has no interest in waiting out the real budget.
NETRUN_TIMEOUT=2
NETRUN_INTERVAL=0

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

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

CALLS="${WORK}/calls"

# A stand-in for a job container, driven by a per-call script so each case can
# say exactly how the network is going to misbehave and on which attempt. It
# counts its own calls, which is how a retry is told from a single attempt.
#
# `<n>:<status>:<stream>:<text>` — on call n, write <text> to <stream> and exit
# <status>. A call with no line of its own succeeds silently.
stub() {
	echo x >>"$CALLS"
	local n line status stream text
	n=$(wc -l <"$CALLS")
	line="$(grep "^${n}:" "${WORK}/script" 2>/dev/null | head -1)"
	# `*:` is "on every call", for the case where the network never comes back.
	[[ -n "$line" ]] || line="$(grep '^\*:' "${WORK}/script" 2>/dev/null | head -1)"
	if [[ -z "$line" ]]; then
		echo "ok-${n}"
		return 0
	fi
	status="$(cut -d: -f2 <<<"$line")"
	stream="$(cut -d: -f3 <<<"$line")"
	text="$(cut -d: -f4- <<<"$line")"
	if [[ "$stream" == "err" ]]; then echo "$text" >&2; else echo "$text"; fi
	return "$status"
}

# script <line>... — how the stub behaves, call by call.
script() {
	: >"$CALLS"
	printf '%s\n' "$@" >"${WORK}/script"
}

GAIERROR='socket.gaierror: [Errno -3] Temporary failure in name resolution'

# ---------------------------------------------------------------------------
log "1. an ordinary success is passed straight through, once"
script
OUT="$(netrun stub 2>"${WORK}/e")"
check "the status is the command's" "0" "$?"
check "stdout is the command's" "ok-1" "$OUT"
check "and it was run exactly once" "1" "$(wc -l <"$CALLS")"

# ---------------------------------------------------------------------------
log "2. a peer name that did not resolve IS asked again"
script "1:1:err:${GAIERROR}"
OUT="$(netrun stub 2>"${WORK}/e")"
RC=$?
check "the second attempt's status is the answer" "0" "$RC"
check "…and its stdout is what the caller gets" "ok-2" "$OUT"
check "…having really been retried" "2" "$(wc -l <"$CALLS")"
RETRY_SAID="$(grep -c 'netrun: the peer name did not resolve' "${WORK}/e")"
check "…loudly, so a real outage is still visible" "1" "$RETRY_SAID"

# The other spellings a resolver uses, since the signature is the whole trigger.
for phrase in \
	'NameResolutionError: HTTPConnection(host=x)' \
	'Failed to resolve '"'"'pdvalue-nessie-961d5c'"'" \
	'dial tcp: lookup minio: no such host' \
	'curl: (6) Could not resolve host: nessie'; do
	script "1:1:err:${phrase}"
	netrun stub >/dev/null 2>&1
	check "retried on: ${phrase:0:38}" "2" "$(wc -l <"$CALLS")"
done

# ---------------------------------------------------------------------------
log "3. any OTHER failure is the job's own answer — returned, not retried"
script "1:1:err:ValueError: the fixture is wrong"
OUT="$(netrun stub 2>"${WORK}/e")"
RC=$?
check "the status is the command's own" "1" "$RC"
check "…on the FIRST attempt" "1" "$(wc -l <"$CALLS")"
check "…with the command's stderr intact" "ValueError: the fixture is wrong" "$(cat "${WORK}/e")"
none "…and nothing claims a retry happened" "$(grep 'netrun:' "${WORK}/e")"

# ---------------------------------------------------------------------------
log "4. exit 2 — a partial run — passes through untouched"
# tests/lake/value_snapshots.sh §5, §7 and §8 assert this exact status: a
# transform that skipped a tenant exits 2 having written the rest. Retrying it
# would re-run a deliberate partial run and could change what it exits.
script "1:2:err:==> exiting 2 (partial): a run that half-completes says so"
netrun stub >/dev/null 2>"${WORK}/e"
check "exit 2 survives the wrapper" "2" "$?"
check "…and was not asked twice" "1" "$(wc -l <"$CALLS")"

# ---------------------------------------------------------------------------
log "5. the retry is BOUNDED, then the command's own failure propagates"
script "*:1:err:${GAIERROR}"
netrun stub >/dev/null 2>"${WORK}/e"
RC=$?
check "a name that never resolves FAILS" "1" "$RC"
GAVE_UP="$(grep -c 'still unresolved after' "${WORK}/e")"
check "…saying it gave up rather than absorbing it" "1" "$GAVE_UP"
KEPT="$(grep -c 'Temporary failure in name resolution' "${WORK}/e")"
check "…and reproducing what the command said" "1" "$KEPT"
TRIES=$(wc -l <"$CALLS")
if [[ "$TRIES" -ge 2 ]]; then
	echo "  PASS  …after more than one attempt (= ${TRIES})"
	pass=$((pass + 1))
else
	echo "  FAIL  …after more than one attempt (= ${TRIES})"
	fail=$((fail + 1))
fi

# ---------------------------------------------------------------------------
log "6. stdout and stderr are kept apart"
script "1:0:err:a diagnostic nobody should capture"
OUT="$(netrun stub 2>"${WORK}/e")"
check "the captured value is stdout alone" "" "$OUT"
check "…and the diagnostic went to stderr" "a diagnostic nobody should capture" "$(cat "${WORK}/e")"

# Multi-line stdout survives the round trip through the capture.
script
printf 'one\ntwo\nthree\n' >"${WORK}/multi"
multi() { cat "${WORK}/multi"; }
check "multi-line stdout is unchanged" "one two three" "$(netrun multi 2>/dev/null | tr '\n' ' ' | sed 's/ $//')"

# ---------------------------------------------------------------------------
log "7. THE RATCHET: one definition, and every caller sources it"
DEFS="$(grep -rln '^netrun() {' "${REPO_DIR}/tests" "${REPO_DIR}/deploy" 2>/dev/null)"
check "one definition" "${REPO_DIR}/tests/lib/netrun.sh" "$DEFS"

CALLERS="$(grep -rl '^[^#]*\bnetrun ' "${REPO_DIR}/tests" "${REPO_DIR}/deploy" 2>/dev/null |
	grep -v '/tests/lib/netrun\(_test\)\?\.sh$')"
UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/netrun.sh"' "$f" || UNSOURCED+="${f}"$'\n'
done <<<"$CALLERS"
none "every caller sources tests/lib/netrun.sh" "${UNSOURCED%$'\n'}"

# The gate this was written for must actually be using it — a library wired
# into nothing is a fix that runs never.
grep -q 'netrun podman run' "${REPO_DIR}/tests/lake/value_snapshots.sh"
check "value_snapshots.sh launches its jobs through it" "0" "$?"

# ---------------------------------------------------------------------------
log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — a container that never resolved its peer is asked again, and"
echo "         nothing else is."
