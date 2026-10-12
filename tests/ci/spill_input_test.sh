#!/usr/bin/env bash
# tests/ci/spill_input_test.sh — the force_spill dispatch input on ci.yml.
# Exists, boolean, defaults false; provision's `spill:` is ec2-only only when it
# is true and falls back to the unchanged `ec2` otherwise. Hermetic.
set -euo pipefail
cd "$(dirname "$0")/../.."
wf=.github/workflows/ci.yml
fail=0
check() { if ! "$@"; then echo "FAIL: $*" >&2; fail=1; fi; }

in_block="$(awk '/^  workflow_dispatch:/{f=1;next} f&&/^  [a-z]/{f=0} f' "$wf")"
check grep -q '^      force_spill:' <<<"$in_block"
check grep -q 'type: boolean' <<<"$in_block"
check grep -q 'default: false' <<<"$in_block"

spill="$(grep -E '^ +spill: ' "$wf")"
check test "$(grep -c . <<<"$spill")" = 1
expr="$(sed -E 's/^ +spill: +//' <<<"$spill")"
check test "$expr" = "\${{ inputs.force_spill && 'ec2-only' || 'ec2' }}"
check test "$(grep -c "'ec2-only'" "$wf")" = 1
[ "$fail" = 0 ] && echo "ok: force_spill shape"
exit "$fail"
