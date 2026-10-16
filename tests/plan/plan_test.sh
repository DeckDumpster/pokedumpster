#!/usr/bin/env bash
# The use-case coverage gate, seen to fail first (the bead's acceptance):
#   - a clean copy passes;
#   - removing an intent (the only thing the runner covers a use case by) fails, naming its UC;
#   - removing the runner fails, naming use cases;
#   - an intent with no catalogue row fails;
#   - a stale report fails.
# Runs against a scratch copy of the relevant files; never touches the tree.
# tier: T1
# covers: tests/plan/gate.sh tests/plan/coverage-json.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd -P)"
fails=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (expected $2, got $3)"; fails=1; fi; }
has() { if grep -qF -- "$2" <<<"$3"; then echo "ok   $1"; else echo "FAIL $1: output lacks '$2'"; fails=1; fi; }

fresh() {
    W="$(mktemp -d)"
    mkdir -p "$W/tests/ui"
    cp -r "$REPO/tests/plan" "$W/tests/"
    cp -r "$REPO/tests/ui/intents" "$REPO/tests/ui/implementations" "$REPO/tests/ui/test_scenarios.ts" "$W/tests/ui/"
    cp -r "$REPO/docs" "$W/docs"
    git -C "$W" init -q -b master
    git -C "$W" add -A
    git -C "$W" -c user.email=t@t -c user.name=t commit -qm base
}
run() { out="$(bash "$W/tests/plan/gate.sh" 2>&1)"; rc=$?; }

fresh; run
check "clean tree passes" 0 "$rc"; rm -rf "$W"

fresh
slug=batches_filter_empty_result
rm "$W/tests/ui/intents/$slug.yaml"; run
check "deleted intent fails" 1 "$rc"
has "…naming its use case" "UC-ui-007" "$out"; rm -rf "$W"

fresh; rm "$W/tests/ui/test_scenarios.ts"; run
check "deleted runner fails" 1 "$rc"
has "…naming a use case" "UC-ui-001" "$out"; rm -rf "$W"

fresh; touch "$W/tests/ui/intents/brand_new_intent.yaml"; run
check "an intent without a use case fails" 1 "$rc"
has "…naming the intent" "brand_new_intent" "$out"; rm -rf "$W"

fresh; echo "x" >> "$W/docs/test-plan/ui-coverage.md"; run
check "a stale report fails" 1 "$rc"
has "…saying so" "stale" "$out"; rm -rf "$W"

exit "$fails"
