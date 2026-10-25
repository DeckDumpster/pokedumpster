#!/usr/bin/env bash
# tests/ci/spill_market_test.sh — every use of the ephemeral-ci provision action
# passes `spill-market: ${{ vars.SPILL_MARKET }}` (the org-wide spot switch), or
# carries a `# spill-market pinned:` comment saying why it pins a market. Hermetic.
set -euo pipefail
cd "$(dirname "$0")/../.."
fail=0
uses=0
for wf in .github/workflows/*.yml .github/workflows/*.yaml; do
  [ -f "$wf" ] || continue
  while IFS= read -r start; do
    uses=$((uses + 1))
    # the `with:` block runs until the next step (a line starting `- `) at or above the uses line's indent
    block="$(awk -v s="$start" 'NR>s && /^ *- / {exit} NR>s {print}' "$wf")"
    if grep -qE '^ +spill-market: +\$\{\{ *vars\.SPILL_MARKET *\}\}' <<<"$block"; then continue; fi
    if grep -qE '^ +spill-market: +[a-z-]+' <<<"$block" && grep -q '# spill-market pinned:' <<<"$block"; then continue; fi
    echo "FAIL: $wf:$start: provision call has no spill-market (or commented pin)" >&2
    fail=1
  done < <(grep -nE 'uses: *DeckDumpster/ephemeral-ci/provision@' "$wf" | cut -d: -f1)
done
[ "$uses" -gt 0 ] || { echo "FAIL: no provision call found" >&2; fail=1; }
[ "$fail" = 0 ] && echo "ok: $uses provision call(s) carry spill-market"
exit "$fail"
