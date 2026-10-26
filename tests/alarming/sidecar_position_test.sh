#!/usr/bin/env bash
# sidecar_position — the tenant's replica position, measured and not read from a
# log (sp-ksml54).
#
# It used to parse the sidecar's `msg="replica sync"` line, which forced the
# sidecar to DEBUG and ~2M syslog lines/day onto the prod host's root filesystem.
# It now compares the replica's furthest TXID (stubbed here: `ltx_max_txid`) with
# the furthest TXID in Litestream's local LTX directory (real files in a temp
# dir). No podman, no logs: there is no log input anywhere in this test, which is
# the claim. Stale replica -> red, fresh replica -> green, each seen.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/../../deploy/backup-check.sh"
for fn in sidecar_position judge_correspondence judge_no_position txid_lt txid_zero; do
    eval "$(awk -v f="^${fn}\\\\(\\\\) \\\\{" '$0 ~ f, /^\}/' "$SRC")"
    declare -F "$fn" >/dev/null || { echo "could not extract ${fn}" >&2; exit 1; }
done

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
MOUNTPOINT="$TMP"; NOW=$(date +%s)
SIDECAR_GRACE_SECONDS=1800; SIDECAR_CTR=x
CORRESPONDENCE_GRACE_SECONDS=0; CORRESPONDENCE_POLL_SECONDS=1
stale() { echo "STALE:$1"; exit 9; }
tenant_replica_url() { echo "s3://b/$1"; }
sidecar_uptime() { echo 7200; }
REPLICA=""
ltx_max_txid() { printf '%s\n' "$REPLICA"; }

mkdir -p "$TMP/tenants"; : >"$TMP/tenants/t.sqlite"
local_ltx() { # local_ltx <max-txid>... — Litestream's local LTX files for t.sqlite
    rm -rf "$TMP/tenants/.t.sqlite-litestream"
    mkdir -p "$TMP/tenants/.t.sqlite-litestream/ltx/0"
    local m; for m in "$@"; do : >"$TMP/tenants/.t.sqlite-litestream/ltx/0/0000000000000001-$m.ltx"; done
}

fails=0
verdict() { # verdict <label> <ok|stale> <replica>
    local out got=ok
    out="$( (judge_correspondence "tenant 't'" t.sqlite "2026-01-01T00:00:00Z" "$TMP/tenants/t.sqlite" s3://b/t) 2>&1)"
    grep -q '^STALE:' <<<"$out" && got=stale
    if [ "$got" = "$2" ]; then printf '  ok    %-52s %s\n' "$1" "$got"
    else printf '  FAIL  %-52s got=%s want=%s\n%s\n' "$1" "$got" "$2" "$out"; fails=$((fails+1)); fi
}

local_ltx 0000000000000009 000000000000000a
REPLICA=000000000000000a; verdict "a fresh replica (equal to the local position)" ok
REPLICA=0000000000000005; verdict "a STALE replica (behind the local position)" stale
REPLICA="";               verdict "an empty replica under a local position" stale
local_ltx 000000000000000a 0000000000000005
REPLICA=000000000000000a; verdict "level files out of order: the max decides" ok
rm -rf "$TMP/tenants/.t.sqlite-litestream"
REPLICA=000000000000000a
out="$( (sidecar_position t.sqlite "tenant 't'"; echo "rc=$?") 2>&1)"
if grep -q 'rc=1' <<<"$out"; then echo "  ok    no local state is 'never picked up' (rc 1)"
else echo "  FAIL  no local state should return 1: $out"; fails=$((fails+1)); fi

[ "$fails" -eq 0 ] && { echo "=== all passed ==="; exit 0; }
echo "=== $fails FAILED ==="; exit 1
