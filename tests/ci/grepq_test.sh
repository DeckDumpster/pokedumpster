#!/usr/bin/env bash
# tests/ci/grepq_test.sh — `cmd | grep -q PAT` inverts under `pipefail`.
#
# grep -q exits the moment it matches. That closes the read end of the pipe, the
# writer upstream takes SIGPIPE, and `set -o pipefail` makes the PIPELINE report
# that writer's 141 — so a SUCCESSFUL match takes the failure branch.
#
# The reason this needs a gate rather than a review is that it is invisible
# until it is not. Nothing inverts while the writer's whole output fits in a
# pipe buffer, because the writer has already finished writing by the time grep
# exits; the same line starts failing the day the log, the container, the
# awk range or the ci.sh run behind it outgrows ~64KB. tests/ci/treewatch_test.sh
# passed on master and failed on a branch carrying seven merges, and the only
# thing that had changed was how much deploy/ci.sh printed.
#
# THE RULE, and it is a whole-repo rule rather than a per-file one: do not pipe
# into `grep -q` anywhere in this repo. `pipefail` is a property of the SHELL
# that ends up running the code, not of the file the code sits in — tests/lib/*
# is sourced by suites that all set it — so "this file does not set pipefail" is
# not a defence the file gets to make. Capture and match with a herestring:
#
#     out="$(cmd)"; grep -q PAT <<<"$out"      # or  grep -q PAT <<<"$(cmd)"
#     grep -q PAT <(cmd)                       # when the bytes must not be
#                                              # round-tripped through $( ),
#                                              # which strips NULs — see
#                                              # tests/lake/shipper.sh
#
# A herestring is not a pipeline, so there is nothing for `pipefail` to inherit
# and nothing to SIGPIPE. Note that the writer's exit status is DISCARDED by the
# rewrite; at every site converted here that was the intent already, because a
# writer that fails prints nothing and the match fails on its own.
#
# §1 is the SEEN RED arm, and it is the reason this file is worth its second:
# it demonstrates the inversion in a live shell rather than asserting that a
# document says it happens. §2 proves the scanner can find the idiom and does
# not fire on the cure. §3 runs it over the real tree.
#
# Hermetic: no podman, no network, no compilation. Sub-second, so it runs in the
# lint tier beside the other harness self-tests.
#
#   bash tests/ci/grepq_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

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
log() { printf '\n=== %s ===\n' "$*"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
log "§1 SEEN RED — the inversion, in a live shell"
# Not a claim about SIGPIPE: the two arms below differ in one thing only, the
# size of the payload, and that is enough to flip the verdict of a matching
# grep. If a future bash or grep stops doing this, this section goes red and
# the rest of the file can be retired rather than believed.

# Small enough to fit the pipe buffer: the writer finishes before grep exits, so
# the bug hides. This is why the idiom survives review and survives master.
( set -o pipefail; seq 1 10 | grep -q '^1$' ) >/dev/null 2>&1
check "a pipeline into grep -q reports success while the payload is small" "0" "$?"

# Same pipeline, same match, ~1.1MB of payload. grep -q exits on line 1 of
# 200000 and seq dies of SIGPIPE behind it.
( set -o pipefail; seq 1 200000 | grep -q '^1$' ) >/dev/null 2>&1
rc_big=$?
check "…and reports FAILURE on the same successful match once it is large" "1" \
	"$([ "$rc_big" -ne 0 ] && echo 1 || echo 0)"
check "…with the writer's SIGPIPE status, not grep's" "141" "$rc_big"

# The cure. Same payload, same match, no pipeline.
( set -o pipefail; grep -q '^1$' <<<"$(seq 1 200000)" ) >/dev/null 2>&1
check "a herestring reports success at the same size" "0" "$?"

# And the process-substitution form, for the sites that carry binary bytes.
( set -o pipefail; grep -q '^1$' <(seq 1 200000) ) >/dev/null 2>&1
check "…and so does process substitution" "0" "$?"

# ---------------------------------------------------------------------------
log "§2 the scanner finds it, and does not fire on the cure"
# scan <dir> -> one "path:line" per offending line, empty when clean.
#
# Full-line comments are skipped, because this rule is DOCUMENTED in about seven
# places in this repo — deploy/ci.sh, CLAUDE.md, tests/lib/litestream.sh — and a
# gate that cannot tell an explanation of a bug from the bug would make writing
# the explanation down the thing that breaks the build. Inline comments are not
# stripped: there is no such site, and a scanner that parsed shell quoting well
# enough to strip them safely would be a bigger thing to trust than the rule.
#
# THIS FILE IS THE ONE EXCLUSION, and it is not a loophole being carved for
# convenience: §1 has to RUN the broken idiom to show it inverting, so the
# literal is here on purpose and a scan that flagged it would make the gate
# permanently red at itself. What that costs is that a genuine offender written
# into this file is not caught by §3 — which is exactly why §2 proves the
# scanner on fixtures it does not exclude, rather than on itself.
scan() {
	grep -rnI --binary-files=without-match \
		--include='*.sh' --include='*.bash' --exclude=grepq_test.sh \
		--exclude-dir=node_modules --exclude-dir=target --exclude-dir=.git \
		--exclude-dir=.svelte-kit --exclude-dir=build --exclude-dir=dist \
		-E '\|[[:space:]]*grep[[:space:]]+-[a-zA-Z]*q' "$1" \
		| grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' \
		| cut -d: -f1,2
}

mkdir -p "$TMP/fixture"
cat > "$TMP/fixture/offender.sh" <<'FIX'
#!/usr/bin/env bash
set -euo pipefail
podman logs "$c" | grep -q 'READY'
FIX
cat > "$TMP/fixture/cured.sh" <<'FIX'
#!/usr/bin/env bash
set -euo pipefail
grep -q 'READY' <<<"$(podman logs "$c")"
grep -q PAR1 <(head -c 4 "$f")
FIX
cat > "$TMP/fixture/explained.sh" <<'FIX'
#!/usr/bin/env bash
# Never write `podman logs "$c" | grep -q READY` — it inverts under pipefail.
	# and not this either: foo | grep -qE bar
set -euo pipefail
FIX

FOUND="$(scan "$TMP/fixture")"
check "the scanner flags a real pipeline into grep -q" "yes" \
	"$(grep -qF 'offender.sh:3' <<<"$FOUND" && echo yes || echo no)"
check "…and does not flag the herestring cure" "no" \
	"$(grep -qF 'cured.sh' <<<"$FOUND" && echo yes || echo no)"
check "…and does not flag a comment explaining the rule" "no" \
	"$(grep -qF 'explained.sh' <<<"$FOUND" && echo yes || echo no)"
check "exactly one offending line in the fixture" "1" "$(grep -c . <<<"$FOUND")"

# ---------------------------------------------------------------------------
log "§3 the real tree is clean"
OFFENDERS="$(scan "$REPO_DIR")"
if [ -n "$OFFENDERS" ]; then
	echo "  the following pipe into grep -q; capture and use a herestring instead:"
	sed 's|^|          |' <<<"$OFFENDERS"
fi
check "no shell script in this repo pipes into grep -q" "" "$OFFENDERS"

# ---------------------------------------------------------------------------
echo ""
echo "grep -q gate: ${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
