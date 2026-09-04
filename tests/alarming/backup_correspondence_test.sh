#!/usr/bin/env bash
# judge_correspondence — the tenant backup test, in isolation.
#
# Its verdict turns on the SIDECAR, and the case this file exists for is the one
# that was silent (pd-30yy): a tenant database the sidecar has never named, on a
# sidecar that has been up for hours, PASSED — because the grace it inherited was
# computed from the file's own timestamp, and the file was new.
#
# Every timestamp proxy tried for that grace has failed in the same direction, and
# each failure was invisible in a way a container gate could not show cheaply:
#
#   * replica age — paged three times for a collection nobody had edited (pd-me6h)
#   * file mtime  — moves on checkpoint and on open, so an old database reads new
#   * birth time  — does not move, but says nothing about whether anything is
#                   replicating the file, and is 0 on filesystems without one
#
# So the matrix below drives the function against a STUBBED sidecar: the position
# it reports, how long its container has been up, and whether it is running at all
# are the inputs, and the tenant file's age is deliberately varied underneath them
# to prove it changes no verdict.
#
# The function is extracted rather than the script sourced: backup-check.sh
# resolves an instance and enumerates a data volume at load time, so sourcing it
# needs a live deployment. The unit under test needs neither. The container tier
# (tests/alarming/run.sh §5, tests/litestream/drill.sh §6) drives the same
# function against a real sidecar and a real S3; this is the cheap half that can
# enumerate the whole matrix.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/../../deploy/backup-check.sh"
[ -f "$SRC" ] || { echo "missing $SRC" >&2; exit 1; }

for fn in judge_correspondence judge_no_position txid_lt txid_zero; do
    eval "$(awk -v f="^${fn}\\\\(\\\\) \\\\{" '$0 ~ f, /^\}/' "$SRC")"
    declare -F "$fn" >/dev/null || { echo "could not extract ${fn}" >&2; exit 1; }
done

NOW=$(date +%s)
SIDECAR_GRACE_SECONDS=1800
SIDECAR_LOOKBACK=6h
SIDECAR_CTR=pkdump-litestream-test

# `exit`, not `return`: the real stale() exits, so anything after a trip must not
# run. Every call below is inside $( ), so this ends the subshell and nothing else.
stale() { echo "STALE:$1"; exit 9; }

# The sidecar, stubbed. POSITION is what its journal says ("" for silence), and
# UPTIME how long its container has been up ("" for not running at all) — the two
# facts the verdict is allowed to turn on.
POSITION=""
UPTIME=""
SIDE_REPLICA=""; SIDE_DB=""; SIDE_AGE=""
sidecar_position() { # sets SIDE_* from POSITION="<replica> <db> <age-seconds>"
    SIDE_REPLICA=""; SIDE_DB=""; SIDE_AGE=""
    [ -n "$POSITION" ] || return 1
    read -r SIDE_REPLICA SIDE_DB SIDE_AGE <<<"$POSITION"
}
sidecar_uptime() { [ -n "$UPTIME" ] || return 1; printf '%s\n' "$UPTIME"; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
fails=0 ran=0

mk() { # mk <name> <age-seconds> -> a tenant file that old by mtime
    : >"$TMP/$1"
    touch -d "@$(( NOW - $2 ))" "$TMP/$1"
}

case_() { # case_ <label> <ok|stale> <output> [must-contain]
    local label="$1" want="$2" out="$3" needle="${4:-}" got=ok
    ran=$((ran + 1))
    grep -q '^STALE:' <<<"$out" && got=stale
    if [ "$got" != "$want" ]; then
        printf '  FAIL  %-58s got=%s want=%s\n' "$label" "$got" "$want"
        printf '%s\n' "$out" | sed 's/^/          /'
        fails=$((fails + 1))
        return
    fi
    if [ -n "$needle" ] && ! grep -qF "$needle" <<<"$out"; then
        printf '  FAIL  %-58s says the wrong thing (want: %s)\n' "$label" "$needle"
        printf '%s\n' "$out" | sed 's/^/          /'
        fails=$((fails + 1))
        return
    fi
    printf '  ok    %-58s %s\n' "$label" "$got"
}

# A healthy tenant: the sidecar named it a second ago, its two txids agree, and
# S3 holds LTX files. Everything below is this with one fact changed.
OK_POS="00000000000000ff 00000000000000ff 1"
NEWEST="2026-08-16T03:00:00Z"

# ── the file's age decides nothing ──────────────────────────────────────────
mk old.sqlite $(( 72*3600 ))
POSITION="$OK_POS" UPTIME=7200
case_ "a 3-day-old file with a healthy sidecar" ok \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/old.sqlite" s3://x 2>&1)" \
  "OK — replica in correspondence"

mk new.sqlite 0
POSITION="$OK_POS" UPTIME=7200
case_ "a brand-new file with a healthy sidecar" ok \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/new.sqlite" s3://x 2>&1)" \
  "OK — replica in correspondence"

# THE pd-30yy CASE. Sidecar up two hours, has never named this database, file
# created moments ago. Judged on the file's age — by mtime or by birth time — this
# is a newborn taking its grace, exit 0, an unreplicated tenant passing in silence.
mk orphan-new.sqlite 0
POSITION="" UPTIME=7200
case_ "a NEW file the long-up sidecar has never named" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/orphan-new.sqlite" s3://x 2>&1)" \
  "has not reported a replication position"

# The same fault with an aged file — what tests/litestream/drill.sh §6 sets up
# with `touch -d '3 days ago'`, and what the birth-time reading scored as newborn
# because touching a file does not change when it was created.
mk orphan-old.sqlite $(( 72*3600 ))
POSITION="" UPTIME=7200
case_ "an OLD file the long-up sidecar has never named" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/orphan-old.sqlite" s3://x 2>&1)" \
  "has not reported a replication position"

# ── what the sidecar's own state decides ────────────────────────────────────
mk restart.sqlite $(( 72*3600 ))
POSITION="" UPTIME=10
case_ "a sidecar that came up 10s ago has judged nothing yet" ok \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/restart.sqlite" s3://x 2>&1)" \
  "not judged"

mk down.sqlite $(( 72*3600 ))
POSITION="" UPTIME=""
case_ "a sidecar that is not running at all is the fault itself" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/down.sqlite" s3://x 2>&1)" \
  "is not running"

# It said something recently and then died. `podman logs` outlives the process,
# so its last words would otherwise carry the check green.
mk died.sqlite $(( 72*3600 ))
POSITION="$OK_POS" UPTIME=""
case_ "a sidecar that reported 1s ago and has since died" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/died.sqlite" s3://x 2>&1)" \
  "NOT RUNNING"

# A position of all zeros is not a position: picked the file up, shipped nothing.
mk zero.sqlite $(( 72*3600 ))
POSITION="0000000000000000 0000000000000000 1" UPTIME=7200
case_ "a zero position on a long-up sidecar" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/zero.sqlite" s3://x 2>&1)" \
  "has not reported a replication position"

POSITION="0000000000000000 0000000000000000 1" UPTIME=10
case_ "a zero position on a sidecar that just came up" ok \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/zero.sqlite" s3://x 2>&1)" \
  "not judged"

# ── the faults the fix must not have quietened ──────────────────────────────
mk s3empty.sqlite $(( 72*3600 ))
POSITION="$OK_POS" UPTIME=7200
case_ "the sidecar claims correspondence, S3 holds nothing" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "" "$TMP/s3empty.sqlite" s3://x 2>&1)" \
  "NOT backed up"

mk stalled.sqlite $(( 72*3600 ))
POSITION="00000000000000ff 00000000000000ff 7200" UPTIME=86400
case_ "a position older than the grace — replication stopped" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/stalled.sqlite" s3://x 2>&1)" \
  "Replication has stopped"

mk behind.sqlite $(( 72*3600 ))
POSITION="0000000000000001 00000000000000ff 1" UPTIME=86400
case_ "a replica BEHIND the local database" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/behind.sqlite" s3://x 2>&1)" \
  "S3 holds up to txid 0000000000000001"

# Ahead, not behind — a database restored to an earlier point. Nothing local is
# missing from S3, which is the only question this check answers.
mk ahead.sqlite $(( 72*3600 ))
POSITION="00000000000000ff 0000000000000001 1" UPTIME=86400
case_ "a replica AHEAD of the local database is not a fault" ok \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/ahead.sqlite" s3://x 2>&1)" \
  "OK — replica in correspondence"

# A named tenant with no file is never a pass — it used to fall through the
# newborn door, an absent file reading as "created 0m ago".
POSITION="$OK_POS" UPTIME=7200
case_ "a named tenant with no database at all" stale \
  "$(judge_correspondence "tenant 'x'" x.sqlite "$NEWEST" "$TMP/absent.sqlite" s3://x 2>&1)" \
  "nothing to check"

[ "$fails" -eq 0 ] && { echo "=== ${ran} passed, 0 failed ==="; exit 0; }
echo "=== $fails FAILED (of ${ran}) ==="; exit 1
