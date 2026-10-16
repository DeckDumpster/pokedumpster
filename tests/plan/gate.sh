#!/usr/bin/env bash
# gate.sh — the use-case coverage gate: a deletion that orphans a use case fails, naming it.
#
#   gate.sh            check
#   gate.sh --write    regenerate docs/test-plan/ui-coverage.{json,md}, then check
#
# The mechanism is Spira's (`test-plan`, the typed catalogue in docs/test-plan/*.toml, the
# `covers: UC-…` declaration) — ported, not re-invented; tests/plan/coverage-json.sh is the
# one repo-specific part, reading this repo's suites into the JSON test-plan expects.
# Four checks, each fatal:
#   1. every intent in tests/ui/intents has a catalogue row (a new intent needs its UC);
#   2. the catalogue is well-formed (`test-plan validate`);
#   3. no use case lacks a covering suite (`test-plan gaps`) — which is what a deleted
#      intent, implementation or runner turns into;
#   4. none lost its last cover since the base ref (`test-plan orphans`), and the committed
#      coverage report is the one the tree generates.
# A tool this gate cannot find is a refusal, never a pass: a gate that cannot check says so.
#
# tier: T0
# covers: tests/plan/gate.sh tests/plan/coverage-json.sh docs/test-plan/*.toml
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd -P)"
CAT="$REPO/docs/test-plan"

command -v test-plan >/dev/null 2>&1 || { [ -n "${SPIRA_RELEASE:-}" ] && PATH="$SPIRA_RELEASE/bin:$PATH"; }
command -v test-plan >/dev/null 2>&1 || {
    echo "plan gate: REFUSED — test-plan is on neither PATH nor \$SPIRA_RELEASE/bin; cannot check use-case coverage" >&2
    exit 1; }

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
bad=0
fail() { printf 'plan gate: %s\n' "$*" >&2; bad=1; }

# 1. every intent has a use case
for f in "$REPO"/tests/ui/intents/*.yaml; do
    slug="$(basename "$f" .yaml)"
    grep -qF "statement = \"$slug: " "$CAT/ui.toml" \
        || fail "intent $slug has no use case in docs/test-plan/ui.toml (add a [[use_case]] whose statement begins \"$slug: \")"
done

# 2. catalogue
test-plan validate --catalogue-dir "$CAT" >"$tmp/v" 2>&1 || { cat "$tmp/v" >&2; fail "catalogue invalid"; }

bash "$HERE/coverage-json.sh" >"$tmp/now.json" || fail "cannot read the suites"

# 3. gaps
test-plan gaps --catalogue-dir "$CAT" --suites "$tmp/now.json" >"$tmp/g" 2>&1 || { cat "$tmp/g" >&2; fail "use case(s) with no covering suite"; }

# 4. orphans against the base, and the report
base=""
for r in origin/master master; do git -C "$REPO" rev-parse -q --verify "$r^{commit}" >/dev/null 2>&1 && { base="$r"; break; }; done
if [ -n "$base" ]; then
    bash "$HERE/coverage-json.sh" --ref "$base" >"$tmp/prev.json" || fail "cannot read the suites at $base"
    test-plan orphans --catalogue-dir "$CAT" --suites "$tmp/now.json" --prev-suites "$tmp/prev.json" >"$tmp/o" 2>&1 \
        || { cat "$tmp/o" >&2; fail "use case(s) orphaned since $base"; }
else
    echo "plan gate: no base ref (origin/master, master) — orphans-since-base not checked; gaps still were" >&2
fi

test-plan write-matrix --catalogue-dir "$CAT" --suites "$tmp/now.json" \
    --out-json "$tmp/ui-coverage.json" --out-md "$tmp/ui-coverage.md" >"$tmp/m" 2>&1 || { cat "$tmp/m" >&2; fail "cannot render the coverage report"; }
if [ "${1:-}" = "--write" ]; then
    cp "$tmp/ui-coverage.json" "$tmp/ui-coverage.md" "$CAT/"
else
    for x in json md; do
        cmp -s "$tmp/ui-coverage.$x" "$CAT/ui-coverage.$x" \
            || fail "docs/test-plan/ui-coverage.$x is stale — run: bash tests/plan/gate.sh --write"
    done
fi

[ "$bad" = 0 ] && echo "plan gate: clean — every use case has a covering suite, none orphaned"
exit "$bad"
