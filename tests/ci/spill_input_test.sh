#!/usr/bin/env bash
# tests/ci/spill_input_test.sh — ci.yml's provision `spill:` is `ec2-only`
# unconditionally: no allow_proxmox input, no Proxmox fallback. Hermetic.
set -euo pipefail
cd "$(dirname "$0")/../.."
wf=.github/workflows/ci.yml
fail=0
check() { if ! "$@"; then echo "FAIL: $*" >&2; fail=1; fi; }

check test "$(grep -rc allow_proxmox .github | awk -F: '{s+=$2} END{print s+0}')" = 0
spill="$(grep -E '^ +spill: ' "$wf")"
check test "$(grep -c . <<<"$spill")" = 1
check test "$(sed -E 's/^ +spill: +//' <<<"$spill")" = "ec2-only"
[ "$fail" = 0 ] && echo "ok: spill is ec2-only"
exit "$fail"
