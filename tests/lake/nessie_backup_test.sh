#!/usr/bin/env bash
# sp-87nj2v: hermetic gate for the Nessie backup — the Python logic (stale /
# corrupt / drill-failure positive controls) and the shell wiring. No podman.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
fail=0
python3 -I "$ROOT/tests/lake/nessie_backup_test.py" || fail=1

bash -n "$ROOT/deploy/nessie-backup.sh" || fail=1
# Every unit installed, declared host-wide, and torn down.
for u in nessie-backup nessie-backup-check; do
    for ext in service timer; do
        [ -f "$ROOT/deploy/pkdump-$u.$ext" ] || { echo "missing pkdump-$u.$ext" >&2; fail=1; }
        grep -q "pkdump-$u@.$ext" "$ROOT/deploy/units-lib.sh" || { echo "units-lib lacks $u@.$ext" >&2; fail=1; }
    done
    grep -q "pkdump-$u@\${INSTANCE}" "$ROOT/deploy/teardown.sh" || { echo "teardown lacks $u" >&2; fail=1; }
done
# No default backup location: the wrapper must refuse without one.
HOME_T="$(mktemp -d)"; trap 'rm -rf "$HOME_T"' EXIT
: >"$HOME_T/lake.env"
out="$(HOME="$HOME_T" PKDUMP_LAKE_ENV="$HOME_T/lake.env" bash "$ROOT/deploy/nessie-backup.sh" x check 2>&1 || true)"
case "$out" in *"PKDUMP_NESSIE_BACKUP_URL is not set"*) ;; *) echo "no refusal without a backup URL: $out" >&2; fail=1 ;; esac
exit "$fail"
