#!/usr/bin/env bash
# judge_freshness — the tenant backup test, in isolation.
#
# It had NO unit coverage, and its one stated assumption — "a collection changes
# daily, so a replica that stopped advancing is a replica that stopped" — is false
# for a collection nobody edits. On 2026-08-14 that sent three Pushover pages for a
# tenant whose database had not been touched in 24 hours, while Litestream was
# syncing every two seconds with zero errors in 48 hours. The backup was perfect.
#
# Case 1 below is that night, reproduced. The rest are the failures the check must
# still catch, so fixing the false alarm cannot quietly disarm the real one.
#
# The function is extracted rather than the script sourced: backup-check.sh resolves
# an instance and enumerates a data volume at load time, so sourcing it needs a live
# deployment. The unit under test needs neither.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/../../deploy/backup-check.sh"
[ -f "$SRC" ] || { echo "missing $SRC" >&2; exit 1; }

eval "$(awk '/^judge_freshness\(\) \{/,/^\}/' "$SRC")"
declare -F judge_freshness >/dev/null || { echo "could not extract judge_freshness" >&2; exit 1; }

NOW=$(date +%s)
MAX_AGE_HOURS=36
MAX_AGE_SECONDS=$(( MAX_AGE_HOURS * 3600 ))
stale() { echo "STALE:$1"; return 0; }   # capture rather than trip the dead-man

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
fails=0
mk()  { : > "$TMP/$1"; touch -d "@$(( NOW - $2 ))" "$TMP/$1"; }
iso() { date -d "@$(( NOW - $1 ))" -Is; }
case_() { # case_ <label> <ok|stale> <output>
    local got=ok
    printf '%s' "$3" | grep -q '^STALE:' && got=stale
    if [ "$got" = "$2" ]; then printf '  ok    %-56s %s\n' "$1" "$got"
    else printf '  FAIL  %-56s got=%s want=%s\n' "$1" "$got" "$2"; fails=$((fails+1)); fi
}

mk quiet.sqlite $(( 24*3600 ))
case_ "quiescent db, 42h-old replica, caught up" ok \
  "$(judge_freshness quiet "$(iso $((42*3600)))" "$TMP/quiet.sqlite" s3://x 2>&1)"

mk moving.sqlite 60
case_ "db written 1m ago, replica 40h old" stale \
  "$(judge_freshness moving "$(iso $((40*3600)))" "$TMP/moving.sqlite" s3://x 2>&1)"

mk lagging.sqlite 60
case_ "db 1m ago, replica 2h old (inside threshold)" ok \
  "$(judge_freshness lagging "$(iso $((2*3600)))" "$TMP/lagging.sqlite" s3://x 2>&1)"

mk walonly.sqlite $(( 90*3600 )); mk walonly.sqlite-wal 60
case_ "main file old, WAL 1m ago, replica 40h old" stale \
  "$(judge_freshness walonly "$(iso $((40*3600)))" "$TMP/walonly.sqlite" s3://x 2>&1)"

mk shmonly.sqlite $(( 90*3600 )); mk shmonly.sqlite-shm 60
case_ "-shm touched by a mere OPEN must not count as a write" ok \
  "$(judge_freshness shmonly "$(iso $((80*3600)))" "$TMP/shmonly.sqlite" s3://x 2>&1)"

mk never.sqlite $(( 90*3600 ))
case_ "no replica at all, db 90h old" stale \
  "$(judge_freshness never "" "$TMP/never.sqlite" s3://x 2>&1)"

mk fresh.sqlite 60
case_ "no replica, db 1m old (not judged yet)" ok \
  "$(judge_freshness fresh "" "$TMP/fresh.sqlite" s3://x 2>&1)"

[ "$fails" -eq 0 ] && { echo "=== $(( 7 )) passed, 0 failed ==="; exit 0; }
echo "=== $fails FAILED ==="; exit 1
