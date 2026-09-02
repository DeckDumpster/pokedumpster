#!/usr/bin/env bash
# Unit test for tests/lib/objects.sh (pd-cxq4).
#
# Two halves, and the second is what keeps the fix from decaying.
#
#   §1-§5 the library does what it claims. The case that matters is §3: a
#         command that exits 0 and prints NOTHING must FAIL, because that is
#         the exact shape the bug took — an `mc ls` that died in a container
#         under parallel-gate load, its stderr thrown away by a `2>/dev/null`,
#         read by the gate as "the tenant zone holds 0 objects". §2 is the
#         opposite and matters just as much: a bucket whose tenant zone really
#         is empty must come back empty and SUCCEED, or the guard's first
#         contact with real work is a false positive and it earns itself an
#         exemption.
#   §6    THE RATCHET. Three gates had the same swallowing listing, copied one
#         from the next. The way it comes back is a fourth gate copying a
#         third. So the tree itself is asserted on: an `mc … ls` whose OUTPUT
#         is read must go through object_store_ls. A listing whose output is
#         discarded is a permission probe — failure is the answer it is asking
#         for — and is left alone.
#
# Deliberately hermetic — no podman, no network, no MinIO — so deploy/ci.sh can
# run it in the lint tier beside tests/lib/wait_test.sh.
#
#   bash tests/lib/objects_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=tests/lib/objects.sh
. "${SCRIPT_DIR}/objects.sh"

SENTINEL="raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-14/run=01GATE/part-0000.json"

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

# A stand-in for `mc_root ls`, driven by files so each case can say exactly how
# the object store is going to misbehave. It counts its own calls, which is how
# §4 can tell a retry from a single attempt.
STUB_CALLS="${WORK}/calls"
STUB_STATUS="${WORK}/status"
STUB_BODY="${WORK}/body"
STUB_ERR="${WORK}/stub-stderr"
stub_ls() {
	echo x >>"$STUB_CALLS"
	local n
	n=$(wc -l <"$STUB_CALLS")
	# `<n>:<status>` lines let a case fail the first call and pass the next.
	local status body
	status="$(sed -n "s/^${n}://p" "$STUB_STATUS")"
	[[ -n "$status" ]] || status="$(sed -n 's/^\*://p' "$STUB_STATUS")"
	body="$(cat "$STUB_BODY")"
	[[ -s "$STUB_ERR" ]] && cat "$STUB_ERR" >&2
	# The body first, THEN the status — because that is what a listing that
	# printed part of a bucket and then died looks like, and it is the case
	# where nothing but the exit status can tell.
	[[ -n "$body" ]] && printf '%s\n' "$body"
	return "$status"
}
arrange() { # arrange <status-spec> <body> [stderr]
	: >"$STUB_CALLS"
	printf '%s\n' "$1" >"$STUB_STATUS"
	printf '%s' "$2" >"$STUB_BODY"
	printf '%s' "${3:-}" >"$STUB_ERR"
}
calls() { wc -l <"$STUB_CALLS" | tr -d ' '; }

A_REAL_LISTING="[2026-08-26 07:53:35 UTC] 41B STANDARD ${SENTINEL}
[2026-08-26 07:53:36 UTC] 1.2KiB STANDARD tenant/database_id=01ALICE/dataset=holdings/as_of=2026-08-13/part-seq-000000000001-000000000005.parquet.enc
[2026-08-26 07:53:36 UTC] 1.1KiB STANDARD tenant/database_id=01BOB/dataset=holdings/as_of=2026-08-14/part-seq-000000000001-000000000003.parquet.enc"

# ---------------------------------------------------------------------------
log "1. a good listing comes back whole"
arrange '*:0' "$A_REAL_LISTING"
OUT="$(object_store_ls "$SENTINEL" stub_ls)"
RC=$?
check "it succeeds" "0" "$RC"
check "…asking exactly once" "1" "$(calls)"
check "…and hands back every line it was given" "3" "$(grep -c . <<<"$OUT")"
check "…so the caller can count the tenant zone" "2" \
	"$(awk '{print $NF}' <<<"$OUT" | grep -c '^tenant/')"

# ---------------------------------------------------------------------------
log "2. AN EMPTY ZONE IS NOT AN ERROR — the guard must not cry wolf"
# Every one of these gates asserts a prefix is empty at least twice: alice's
# partition after the drop, that nothing was put back under it, that tenant/
# holds nothing at all. If the sentinel rule could not tell that from a broken
# listing, it would be unusable and would get an exemption instead of a fix.
arrange '*:0' "[2026-08-26 07:53:35 UTC] 41B STANDARD ${SENTINEL}"
OUT="$(object_store_ls "$SENTINEL" stub_ls)"
RC=$?
check "a bucket with no tenant objects still succeeds" "0" "$RC"
check "…and the tenant zone reads as empty" "0" \
	"$(awk '{print $NF}' <<<"$OUT" | grep -c '^tenant/')"

# ---------------------------------------------------------------------------
log "3. THE BUG: exit 0 with nothing to show is a FAILED listing, not an empty store"
OBJECT_LS_TIMEOUT=0
arrange '*:0' ""
OUT="$(object_store_ls "$SENTINEL" stub_ls 2>"${WORK}/diag")"
RC=$?
check "it refuses" "1" "$RC"
check "…printing no listing at all, so a count cannot be taken from it" "" "$OUT"
check "…and says it printed nothing, rather than blaming the sentinel" "yes" \
	"$(grep -q 'exited 0 and printed NOTHING' "${WORK}/diag" && echo yes || echo no)"

# A caller that names no sentinel gets rule 1 alone, deliberately. That is for
# the gates where an empty store IS the observation — tests/litestream/run.sh
# lists a bucket whose contents are the thing under test, and an empty replica
# is a finding to report, not a listing to distrust. Claiming a sentinel there
# would turn a real failure into a misleading one.
OUT="$(object_store_ls "" stub_ls 2>/dev/null)"
check "…while a caller that promised nothing is told what the store said" "0" "$?"
check "…which is nothing, for its own assertion to judge" "" "$OUT"

# A listing that printed a plausible bucket and THEN died. Half a listing is
# not half an answer — every caller here counts lines — and it is the one shape
# the sentinel cannot catch, because the sentinel is in what came back.
arrange '*:7' "$A_REAL_LISTING"
OUT="$(object_store_ls "$SENTINEL" stub_ls 2>/dev/null)"
check "a listing that printed keys and then failed is refused" "1" "$?"
check "…and none of those keys is handed on to be counted" "" "$OUT"
OUT="$(object_store_ls "" stub_ls 2>/dev/null)"
check "…the same with no sentinel named, where the status is all there is" "1" "$?"

# One step further out: a listing that came back full, of something that is not
# this bucket. Only the sentinel can catch that one.
arrange '*:0' "[2026-08-26 07:53:35 UTC] 41B STANDARD raw/some/other/bucket.json"
OUT="$(object_store_ls "$SENTINEL" stub_ls 2>"${WORK}/diag")"
check "a full listing missing the sentinel is refused" "1" "$?"
check "…and the refusal names the key it looked for" "yes" \
	"$(grep -qF "$SENTINEL" "${WORK}/diag" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "4. a command that fails is retried, boundedly, and then refused"
OBJECT_LS_TIMEOUT=6
arrange '1:7
*:0' "$A_REAL_LISTING"
OUT="$(object_store_ls "$SENTINEL" stub_ls 2>/dev/null)"
RC=$?
check "a listing that fails once and then works is returned" "0" "$RC"
check "…having really asked twice" "2" "$(calls)"
check "…and it is the good listing" "3" "$(grep -c . <<<"$OUT")"

# And the bound. An unbounded retry would turn a broken object store into a
# hung CI job, which is strictly worse than the flake it replaced: the failure
# stops being reported at all. (How many attempts fit in the budget is the
# clock's business — the deterministic evidence that it retries at all is the
# case above.)
OBJECT_LS_TIMEOUT=3
arrange '*:7' "$A_REAL_LISTING" "mc: <ERROR> Unable to initialize new alias from the provided credentials."
START=$SECONDS
OUT="$(object_store_ls "$SENTINEL" stub_ls 2>"${WORK}/diag")"
RC=$?
ELAPSED=$((SECONDS - START))
check "one that never works is refused rather than returned empty" "1" "$RC"
check "…nothing on stdout for a caller to miscount" "" "$OUT"
check "…and it GAVE UP, instead of retrying until CI times out" "yes" \
	"$([[ "$ELAPSED" -le 10 ]] && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "5. the refusal is DIAGNOSABLE — pd-cxq4 was not"
# The listing that flaked discarded its stderr, so what actually went wrong
# that night is unknowable. Whatever the command said is reproduced.
check "it names the command that was run" "yes" \
	"$(grep -q 'stub_ls' "${WORK}/diag" && echo yes || echo no)"
check "it reports the exit status" "yes" \
	"$(grep -q 'last exit status 7' "${WORK}/diag" && echo yes || echo no)"
check "it reproduces what the command wrote to stderr" "yes" \
	"$(grep -q 'Unable to initialize new alias' "${WORK}/diag" && echo yes || echo no)"
check "it says an empty listing is not an empty store" "yes" \
	"$(grep -q 'an empty listing is not an empty store' "${WORK}/diag" && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "6. every listing whose OUTPUT is read goes through object_store_ls"
# The rule stated over the TREE, not over the three files that were wrong. A
# gate nobody has written yet is the failure mode: it copies a neighbour, and
# nothing says so until a listing dies under load and a deletion is reported
# proven having looked at nothing.
#
# Exempt by construction: a listing whose stdout is discarded is a permission
# probe (tenant_zone.sh's can_list), where a non-zero status IS the answer.
harnesses() {
	find "${REPO_DIR}/tests" "${REPO_DIR}/deploy" -name '*.sh' -type f \
		! -path "${REPO_DIR}/tests/lib/objects_test.sh" | sort
}
UNGUARDED=""
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	rest="${hit#*:}"
	lineno="${rest%%:*}"
	text="${rest#*:}"
	# A probe throws the LISTING away and branches on the status, so its
	# stdout goes to /dev/null. Discarding stderr is the opposite thing and
	# is the bug's own signature — strip the stderr redirects before asking,
	# or `2>/dev/null` reads as an exemption from the rule it violates.
	stdout="${text//2>\/dev\/null/}"
	stdout="${stdout//2>&1/}"
	[[ "$stdout" == *">/dev/null"* ]] && continue
	# Guarded on its own line, or on the one above it (a wrapped call).
	[[ "$text" == *"object_store_ls"* ]] && continue
	prev="$(sed -n "$((lineno - 1))p" "$file")"
	[[ "$prev" == *"object_store_ls"* ]] && continue
	UNGUARDED+="${file}:${lineno}:${text}"$'\n'
done < <(harnesses | xargs grep -nE '\bmc(_[a-z_]+)?[^|#]* ls( |$)' /dev/null |
	grep -vE '^[^:]*:[0-9]+:[[:space:]]*#')
none "no harness reads an object listing it cannot trust" "${UNGUARDED%$'\n'}"

# And the library is defined once, like every other harness library here.
DEFS="$(harnesses | xargs grep -ln '^object_store_ls() {' /dev/null)"
check "one definition" "${REPO_DIR}/tests/lib/objects.sh" "$DEFS"
CALLERS="$(harnesses | xargs grep -l 'object_store_ls ' /dev/null |
	grep -v '/tests/lib/objects.sh$')"
UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/objects.sh"' "$f" || UNSOURCED+="${f}"$'\n'
done <<<"$CALLERS"
none "every caller sources tests/lib/objects.sh" "${UNSOURCED%$'\n'}"

# ---------------------------------------------------------------------------
log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — a listing that failed can no longer pass for an empty store."
