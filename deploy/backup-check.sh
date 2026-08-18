#!/usr/bin/env bash
#
# Layer 1 (PRIMARY) — backup-freshness dead-man's switch (pokedumpster-ivq.2,
# re-scoped for S3-only Litestream, multi-tenant since pd-fof4).
#
# The old design pinged a monitor from inside backup.sh on a successful nightly
# run. There is no backup.sh anymore: backups are a CONTINUOUS Litestream sidecar
# replicating every tenants/*.sqlite to S3. The canonical silent failure (observed
# during a Jun 2026 key rotation) is the sidecar showing systemd `active` while
# error-looping on AccessDenied and NOT replicating — liveness is NOT freshness.
#
# So this checker VERIFIES REPLICATION against S3, then pings the off-box
# monitor (healthchecks.io) only when every replica passes:
#   0. Check the USER REGISTRY's replica (pd-nd6w). It is checked separately
#      because it IS separate: its own file at the data root, its own `path:`
#      replica prefix. And it is checked at all because its silent loss is the
#      one this script's tenant loop cannot see — every tenant would still be
#      fresh, and the table saying whose database is whose would be gone.
#      It is checked by a DIFFERENT TEST — correspondence, not freshness
#      (pd-me6h); see the registry section for why age says nothing about a
#      database that is static by design.
#   1. Enumerate EVERY tenant database on the data volume — the same glob the
#      sidecar's `dir:` entry replicates. A tenant whose replica is dead is
#      exactly as unbacked-up as the only tenant used to be, and in directory
#      mode nothing else would ever notice: a tenant that never reached S3
#      produces no error anywhere, just an absent prefix.
#   2. List each tenant's replica LTX files via the litestream image (same creds
#      / secret / addressing as deploy/restore-litestream.sh).
#   3. If any list FAILS (broken creds, network, missing replica) -> NOT fresh.
#   4. If any tenant's newest replica write is older than the threshold -> NOT fresh.
#   5. All fresh -> ping the monitor (it expects a ping every run; a miss alerts).
#      Any stale -> ping <url>/fail (trip immediately) + push Pushover with detail.
#
# Because the monitor lives OFF the box, a dead checker / dead box / disabled
# timer ALSO stops the pings and trips the alert — this is the layer that would
# have caught the 11-day outage. The Pushover push is the fast, detailed signal
# while the box is up; the monitor is the backstop for box-down.
#
# ── NO CHECK MAY PASS BY SKIPPING (pd-1717, then pd-7f46) ───────────────────
# This script used to print "skipping" and exit 0 without asking S3 anything
# when PKDUMP_BACKUP_PING_URL was unset. That is a pass — indistinguishable, to
# a caller, a CI tier or an operator reading a green unit, from "every replica
# is fresh". A check that cannot fail is not evidence, and this project already
# owns the scar: prod ran ACTIVE and replicating nothing while every
# backup-shaped signal was green (pd-1717).
#
# So the VERIFICATION always runs. What the ping URL controls is the PING, and
# nothing else: with no monitor configured, freshness is still checked against
# S3 and a stale replica still fails, it just cannot arm the off-box dead-man.
# The absence of that URL is reported on the way past, because an unarmed Layer
# 1 is worth saying out loud on a box that has real backups.
#
# Note that "is this instance armed?" is a different question from "are the
# backups fresh?", and it has its own truthful answer in deploy/alarm-status.sh
# — which reports NOT ARMED and exits non-zero for exactly this configuration.
# Answering it here as well, by failing, would cost the freshness verification
# its own exit status.
#
# No new runtime deps beyond curl + the litestream image.
#
# Usage: backup-check.sh <instance> [tenant ...]   (default: every tenant on the volume)
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: backup-check.sh <instance> [tenant ...]}"
shift || true

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# Overridable so a gate can drive the shipped script with fixture config while
# leaving $HOME alone. Overriding HOME instead does not work: podman's container
# storage lives under it, so the script loses the data volume and every check
# reports "volume not found" — which reads as a fault and is really a broken test.
CONF_DIR="${PKDUMP_CONF_DIR:-${HOME}/.config/pkdump/${INSTANCE}}"
LS_IMG="docker.io/litestream/litestream:latest"
VOLUME="pkdump-${INSTANCE}-data"

# shellcheck source=deploy/litestream-lib.sh
. "${SCRIPT_DIR}/litestream-lib.sh"

# Host-wide Pushover creds, then per-instance ping URL / threshold / S3 target.
# PKDUMP_ALERTS_ENV names the host-wide file; production never sets it. Only
# tests point it elsewhere (tests/alarming/run.sh), for the same reason
# LITESTREAM_S3_ENDPOINT exists — so a gate can exercise the shipped script
# without writing to the operator's real credentials.
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"
[ -f "$ALERTS_ENV" ]                && { set -a; . "$ALERTS_ENV";                set +a; }
[ -f "${CONF_DIR}/alerts.env" ]     && { set -a; . "${CONF_DIR}/alerts.env";     set +a; }
[ -f "${CONF_DIR}/litestream.env" ] && { set -a; . "${CONF_DIR}/litestream.env"; set +a; }

PING="${PKDUMP_BACKUP_PING_URL:-}"
# Snapshots are daily (litestream.yml interval=24h); the threshold must clear one
# full interval plus margin, so a single late snapshot doesn't false-alarm.
MAX_AGE_HOURS="${PKDUMP_BACKUP_MAX_AGE_HOURS:-36}"

# No monitor configured. The check still runs — see the header. Only the ping at
# the end is skipped, and the operator is told which half they are getting, and
# how to arm the other one.
if [ -z "$PING" ]; then
    echo "backup-check: PKDUMP_BACKUP_PING_URL unset — verifying freshness anyway;" \
         "the off-box dead-man's switch is NOT armed (instance: ${INSTANCE})." \
         "To arm it: put the healthchecks.io ping URL in ${CONF_DIR}/alerts.env," \
         "then bash ${SCRIPT_DIR}/alarm-status.sh ${INSTANCE}"
fi

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# Mark the latest confirmed-fresh time on the data volume so the app can surface
# staleness in-app (Layer 3 / ivq.5) without needing S3 creds of its own.
MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME" 2>/dev/null || true)"
mark_fresh() {
    [ -n "$MOUNTPOINT" ] || return 0
    date +%s > "${MOUNTPOINT}/.backup-last-ok" 2>/dev/null || true
}

stale() {
    local reason="$1"
    echo "backup-check: STALE — ${reason}" >&2
    # Trip the off-box dead-man immediately rather than waiting for the grace
    # window to expire on a missed ping. Only if there is one to trip: an
    # unarmed monitor changes what this failure can NOTIFY, never whether it
    # is a failure.
    if [ -n "$PING" ]; then
        curl -fsS -m 10 "${PING}/fail" >/dev/null 2>&1 || true
    fi
    # Fast, detailed push (only reaches you while the box is up; the monitor is
    # the backstop for box-down).
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster backup STALE (${INSTANCE})" \
        "Litestream S3 freshness check failed: ${reason}" || true
    exit 1
}

# --- Which tenants to check ------------------------------------------------
# Default: every tenant database on the volume. Explicit names are accepted so a
# restore drill can check one without waiting for the timer.
TENANTS=("$@")
if [ ${#TENANTS[@]} -eq 0 ]; then
    [ -n "$MOUNTPOINT" ] || stale "data volume '${VOLUME}' not found — cannot enumerate tenants"
    mapfile -t TENANTS < <(tenants_on_volume "$MOUNTPOINT")
fi
[ ${#TENANTS[@]} -gt 0 ] || stale "no tenant databases found under ${MOUNTPOINT}/tenants/"

NOW="$(date +%s)"
MAX_AGE_SECONDS=$(( MAX_AGE_HOURS * 3600 ))

# How far back to look for the sidecar's own replication-position lines, and how
# long a silence is a fault. Measured on prod 2026-08-15: the sidecar emits one
# `replica sync` line per database PER SECOND, so 30 minutes of silence is not a
# quiet period, it is a stopped process. The lookback is wider than the grace so
# the check can tell "stopped 40 minutes ago" (reportable, with an age) apart from
# "nothing here at all" (reportable, as a glob miss) instead of collapsing both.
# A Go duration, because that is what `podman logs --since` parses — it rejects
# "6 hours ago" outright, unlike journalctl.
SIDECAR_LOOKBACK="${PKDUMP_BACKUP_SIDECAR_LOOKBACK:-6h}"
SIDECAR_GRACE_SECONDS="${PKDUMP_BACKUP_SIDECAR_GRACE_SECONDS:-1800}"
# Which container to read them from. `podman logs`, not `journalctl`, because the
# sidecar is a container in every deployment of it and only SOME of those are
# systemd units: prod's is a Quadlet (so journald has it too), the alarming gate
# runs a bare detached container (so journald does not). Reading the container
# directly is one mechanism that covers both, and it drops the coupling between
# this check and a unit-naming convention that has nothing to do with replication.
SIDECAR_CTR="${PKDUMP_BACKUP_SIDECAR_CTR:-systemd-pkdump-litestream-${INSTANCE}}"

# ltx_list <replica-url> <what> — every LTX file a replica holds, as litestream
# prints them. Returns NON-ZERO if the query itself failed, because "we could
# not ask" and "the answer is fine" must never be the same outcome — and every
# caller substitutes this, so it cannot trip the switch itself: `exit` inside
# `$(...)` leaves only the subshell, and the check would sail on with an empty
# answer. The caller does the tripping.
#
# A READ/LIST op, with the same creds / secret / addressing as
# deploy/restore-litestream.sh. Nothing here writes to S3, deliberately: a
# checker that can damage what it watches is a liability.
#
# `-level all` is load-bearing: `ltx` defaults to level 0, and level 0 gets
# compacted away into higher levels. A database nobody has written to today
# would list nothing at level 0 and read as dead when it is merely idle.
ltx_list() {
    local url="$1" what="$2" out
    out="$(podman run --rm --user 0:0 \
        -v "${CONF_DIR}/aws/config:/aws/config:ro" \
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
        -e AWS_CONFIG_FILE=/aws/config \
        -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
        -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
        "$LS_IMG" ltx -level all "$url" 2>&1)" || {
        echo "backup-check: ${what}: litestream ltx failed (creds/network/S3): $(printf '%s' "$out" | tail -n1)" >&2
        return 1
    }
    printf '%s\n' "$out"
}

# Both parsers below read ltx_list's output format-agnostically: the column
# order has shifted across litestream versions, so each greps for the SHAPE of
# the value it wants rather than a position. `|| true` because "no rows at all"
# is a case the callers handle, not a reason to abort: grep exits 1 on no match
# and pipefail would kill the script before it could report — silently, with the
# dead-man's switch neither pinged nor tripped.

# ltx_newest <replica-url> <what> — the newest RFC3339 'created' timestamp in a
# replica, or empty if it has none. Zulu RFC3339 sorts lexicographically ==
# chronologically.
ltx_newest() {
    local out
    out="$(ltx_list "$1" "$2")" || return 1
    printf '%s\n' "$out" \
        | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z' \
        | sort | tail -n1 || true
}

# ltx_max_txid <replica-url> <what> — the furthest transaction the replica has
# received, across every compaction level, or empty if it holds nothing.
#
# A TXID is 16 lowercase hex characters, zero-padded, which makes it both
# recognisable on sight and sortable as a string — no arithmetic, so a 64-bit
# value cannot overflow bash's signed integers on the way to a comparison.
# Every row carries min_txid AND max_txid and max >= min by construction, so
# the largest token anywhere in the listing IS the highest max_txid. Nothing
# else in the output can be mistaken for one: levels are single digits, sizes
# are decimal, timestamps carry punctuation.
ltx_max_txid() {
    local out
    out="$(ltx_list "$1" "$2")" || return 1
    printf '%s\n' "$out" \
        | grep -oE '\b[0-9a-fA-F]{16}\b' | tr 'A-F' 'a-f' \
        | LC_ALL=C sort | tail -n1 || true
}

# local_position <db-path-inside-the-container> <what> — where Litestream stands
# LOCALLY on a database: its own view of the file (LOCAL_STATE) and the last
# transaction it has ingested from it (LOCAL_TXID, empty when there is none).
#
# Asked of Litestream, through the SHIPPED deploy/litestream.yml, rather than
# re-derived here: the sidecar's local state directory is Litestream's own
# bookkeeping and the version that wrote it is the only thing that should be
# reading it. `status` is the documented command for exactly this question and
# `local txid` is its documented column.
#
# The data volume is mounted READ-ONLY, and that flag is the guarantee rather
# than a comment: this checker watches the one set of files on the box that
# cannot be regenerated, so it is denied the ability to write to them by the
# kernel and not by good intentions. (Measured, v0.5.16: `status` reads a
# read-only volume fine.)
#
# `status -json` prints an EMPTY ARRAY, exit 0, for a path the config does not
# name — a pass-by-skipping shape this script does not accept anywhere. An
# absent status field is therefore a hard failure below, not a quiet default.
LOCAL_STATE=""
LOCAL_TXID=""
local_position() {
    local db="$1" what="$2" out
    LOCAL_STATE=""
    LOCAL_TXID=""
    out="$(podman run --rm --user 0:0 \
        -v "${MOUNTPOINT}:/data:ro" \
        -v "${SCRIPT_DIR}/litestream.yml:/etc/litestream.yml:ro" \
        -e LITESTREAM_TENANTS_DIR -e LITESTREAM_REGISTRY_DB \
        -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_PATH \
        -e LITESTREAM_S3_REGISTRY_PATH -e LITESTREAM_S3_REGION \
        -e LITESTREAM_S3_ENDPOINT \
        "$LS_IMG" status -json "$db" 2>&1)" || {
        echo "backup-check: ${what}: litestream status failed: $(printf '%s' "$out" | tail -n1)" >&2
        return 1
    }

    LOCAL_STATE="$(printf '%s\n' "$out" \
        | grep -oE '"status"[[:space:]]*:[[:space:]]*"[^"]*"' \
        | head -n1 | sed 's/.*"\([^"]*\)"$/\1/' || true)"
    LOCAL_TXID="$(printf '%s\n' "$out" \
        | grep -oE '\b[0-9a-fA-F]{16}\b' | tr 'A-F' 'a-f' | head -n1 || true)"

    if [ -z "$LOCAL_STATE" ]; then
        echo "backup-check: ${what}: litestream status named no database — deploy/litestream.yml does not cover ${db}: $(printf '%s' "$out" | tr -d '\n')" >&2
        return 1
    fi
}

# judge_freshness <what> <newest> <db-file> <replica-url> — the TENANT test.
# Either it returns quietly or it trips the switch.
#
# LAG, NOT AGE. The question is whether the replica is BEHIND THE DATABASE, not
# whether it was written recently.
#
# This used to alarm when the newest S3 write was older than MAX_AGE_HOURS, on the
# stated assumption that "a collection changes daily, so a replica that stopped
# advancing is a replica that stopped". That assumption is false: a collection
# nobody edits produces no writes, Litestream correctly uploads nothing, and the
# check pages anyway.
#
# It did exactly that on 2026-08-14 — three Pushover pages for a tenant whose
# database had not been touched in 24h, while Litestream was syncing every two
# seconds with ZERO errors in 48 hours. The backup was perfect; the heuristic was
# measuring the wrong thing, and it woke Ryan to say so.
#
# A replica at or past the database's newest data write is CAUGHT UP, and its
# absolute age is irrelevant — restoring it reproduces the current database. The
# registry has been judged on correspondence rather than recency since pd-me6h;
# this brings tenants into line with it.
#
# The DB's own timestamp is max(mtime of the .sqlite, mtime of its -wal), because
# in WAL mode a write lands in the -wal first and the main file's mtime can lag a
# checkpoint. The -shm is deliberately NOT considered: it is touched whenever the
# database is merely OPENED, so including it would make every restart look like a
# write and mask a genuinely lagging replica.
# sidecar_position <db-basename> <what> — where the RUNNING Litestream stands on
# one tenant: SIDE_REPLICA (what has reached S3) and SIDE_DB (what it has ingested
# locally), both 16-hex TXIDs, plus SIDE_AGE seconds since it last said so.
#
# WHY THE JOURNAL AND NOT `litestream status`. Tenants are declared as ONE
# `dir:` + `pattern: "*.sqlite"` entry (litestream.yml) — deliberately, so a new
# tenant needs no config rewrite. The v0.5.16 CLI does not expand that entry:
# `status -json /data/tenants/<id>.sqlite` returns `[]`, and `status` with no path
# lists the dir as `"database": "/"`, `"status": "not initialized"`. Measured
# 2026-08-15 against the shipped config, both inside the live sidecar and in a
# throwaway container. So the CLI cannot answer this question for a tenant at all,
# while the RUNNING process answers it once per second per database:
#
#   msg="replica sync" db=<id>.sqlite replica=s3 txid.replica=…9 txid.db=…9
#
# That is not a proxy and not an inference. It is the replicating process naming
# its own two positions — the same pair pd-me6h judges the registry on, from the
# component that would know first. `status` reads config; this reads behaviour.
#
# An empty result is NOT a pass. A tenant file that exists on disk while the
# sidecar has never mentioned it means the glob did not pick it up and that
# database is being backed up by nothing — the worst outcome this check exists to
# find, and the one an age test scored as "fine, just quiet".
SIDE_REPLICA=""
SIDE_DB=""
SIDE_AGE=""
sidecar_position() {
    local base="$1" line ts_iso ts_epoch
    SIDE_REPLICA=""; SIDE_DB=""; SIDE_AGE=""

    # Bounded lookback so a rolled journal cannot resurrect a stale position, and
    # so the query stays cheap at ~86k lines/day/db.
    line="$(podman logs --since "${SIDECAR_LOOKBACK}" "$SIDECAR_CTR" 2>&1 \
            | grep -F "msg=\"replica sync\"" | grep -F "db=${base}" \
            | tail -n1 || true)"
    [ -n "$line" ] || return 1

    SIDE_REPLICA="$(printf '%s' "$line" | sed -n 's/.*txid\.replica=\([0-9a-fA-F]\{16\}\).*/\1/p' | tr 'A-F' 'a-f')"
    SIDE_DB="$(printf '%s' "$line"      | sed -n 's/.*txid\.db=\([0-9a-fA-F]\{16\}\).*/\1/p'      | tr 'A-F' 'a-f')"
    ts_iso="$(printf '%s' "$line" | sed -n 's/^time=\([^ ]*\).*/\1/p')"
    if [ -n "$ts_iso" ]; then
        ts_epoch="$(date -d "$ts_iso" +%s 2>/dev/null || true)"
        # Clamp at zero. The sidecar's clock and this script's differ by a hair,
        # and NOW is stamped at startup while the line is read seconds later, so
        # a healthy check routinely computes a small negative age. Reporting
        # "-1s ago" reads as a bug in the checker and invites someone to distrust
        # a correct result.
        if [ -n "$ts_epoch" ]; then
            SIDE_AGE=$(( NOW - ts_epoch ))
            # `if`, not `[ … ] && …`: this script runs under `set -e` and a bare
            # false test has taken a run down here before (see judge_freshness).
            if [ "$SIDE_AGE" -lt 0 ]; then SIDE_AGE=0; fi
        fi
    fi
    [ -n "$SIDE_REPLICA" ] && [ -n "$SIDE_DB" ]
}

# sidecar_uptime — how many seconds the Litestream sidecar CONTAINER has been up,
# on stdout. Non-zero, printing nothing, when it is not running or podman cannot
# say: "we could not ask" is never the same answer as "everything is fine", and
# every caller treats it as the fault it is.
#
# This is the honest source for the one question a tenant's file timestamps were
# standing in for — has this database had a FAIR CHANCE to be replicated yet.
# Replication is not a property of the file; it is a property of the process
# watching it. A container that came up ten seconds ago has not scanned its
# tenants glob yet, whatever the files' ages; one that has been up for hours and
# has never named a database is not replicating that database, however new it is.
# `.StartedAt.Unix`, not `.StartedAt`: podman renders the bare field as a Go
# time.Time — `2026-08-16 15:13:38.171340689 +0000 UTC` — which `date -d` refuses
# for the trailing zone NAME. Asking the template for the epoch removes the
# parsing step rather than working around it, and a non-numeric answer is
# treated as "could not ask" below.
sidecar_uptime() {
    local out state epoch up
    out="$(podman inspect -f '{{.State.Running}} {{.State.StartedAt.Unix}}' "$SIDECAR_CTR" 2>/dev/null)" \
        || return 1
    state="${out%% *}"
    epoch="${out##* }"
    [ "$state" = "true" ] || return 1
    case "$epoch" in ''|*[!0-9]*) return 1 ;; esac
    up=$(( NOW - epoch ))
    # Clamped for the same reason SIDE_AGE is: NOW is stamped at startup and the
    # clocks differ by a hair, so a container started moments ago can read as
    # negative — which looks like a bug in the checker rather than a young sidecar.
    if [ "$up" -lt 0 ]; then up=0; fi
    printf '%s\n' "$up"
}

# await_sidecar_correspondence <db-basename> <what> — the TENANT half of the
# bounded re-ask the registry has had since pd-me6h, and the reasoning is
# identical (it is written out in full at CORRESPONDENCE_GRACE_SECONDS below):
# a replica reading behind is not news YET, because Litestream holds an
# un-uploaded checkpoint across a transient S3 error and clears it at its next
# compaction tick. A replica that has genuinely stopped never catches up, so the
# window costs nothing on the path that matters.
#
# It re-asks the SIDECAR rather than S3, because the sidecar's own `replica sync`
# pair is where a tenant's positions come from at all — the registry's leg reads
# `litestream status` and can therefore re-query S3 directly, and this one cannot
# (see the note on sidecar_position above). Same question, same window, asked of
# whichever component can answer it for that database.
#
# Without this the two legs judged the same fault by different rules, and the
# difference was invisible on an unloaded box: tests/alarming/run.sh §4b takes
# the object store away, gives it back, and re-runs the checker, and the registry
# waited the resulting lag out while the tenant beside it — behind for exactly
# the same reason, by exactly the same one transaction — paged on the first
# reading (pd-yglw, seen on CI where the catch-up did not beat the run).
await_sidecar_correspondence() {
    local base="$1" what="$2" waited=0
    echo "backup-check: ${what}: its replica reads behind the database" \
         "(ingested ${SIDE_DB}, replica ${SIDE_REPLICA}) — re-asking the sidecar for up to" \
         "${CORRESPONDENCE_GRACE_SECONDS}s in case a sync is still in flight"
    while [ "$waited" -lt "$CORRESPONDENCE_GRACE_SECONDS" ]; do
        sleep "$CORRESPONDENCE_POLL_SECONDS"
        waited=$(( waited + CORRESPONDENCE_POLL_SECONDS ))
        # A sidecar that goes silent mid-window is a worse fault than the lag, and
        # it is not this function's to report: keep the diverged pair we already
        # have and let the verdict below name it.
        sidecar_position "$base" "$what" || return 0
        if ! txid_lt "$SIDE_REPLICA" "$SIDE_DB"; then
            echo "backup-check: ${what}: its replica caught up after ${waited}s"
            return 0
        fi
    done
}

# judge_correspondence <what> <db-basename> <newest> <db-file> <url> — the TENANT
# test, and the one Ryan asked for on 2026-08-16: page only when something is
# actually wrong.
#
# A handful of questions, each of which can only be answered NO by a real fault:
#
#   1. Is the sidecar watching this database at all?  (a position, non-zero)
#   2. Did bytes actually land off-box?               (the replica lists LTX files)
#   3. Is it still speaking?                          (that position, recently)
#   4. Has everything it ingested reached S3?         (txid.replica >= txid.db,
#      re-asked over the correspondence window before it counts)
#   5. Is the process still alive right now?          (the container is running)
#
# Question 2 is asked separately and of S3 itself because every other one is the
# sidecar's own account of its work. A backup check that only asks the backing-up
# process how it is getting on is a check that agrees with a liar.
#
# What is deliberately NOT asked: how OLD anything is. That was the bug, three
# times over — first raw replica age, then mtime lag, both of which page for a
# collection nobody edited. A quiescent tenant in correspondence is a PERFECT
# backup: restore it and you get today's database, byte for byte. Age is a fact
# about Ryan's card-buying habits, not about whether his data is safe.
#
# AND NEITHER IS THE TENANT FILE'S OWN AGE (pd-30yy). The one thing a file
# timestamp was still deciding was the GRACE — "is this database too newly
# created to judge yet" — and there is no honest timestamp for that question.
# mtime moves forward on every write, so an old database touched an hour ago
# reads as an hour old and inherits a newborn's grace. Birth time (`stat -c %W`)
# does not move, and reading it looked like the fix — but it is zero on
# filesystems that do not record one, it cannot be aged in a test without
# creating a file for real, and it is still answering the wrong question: WHEN A
# FILE WAS CREATED SAYS NOTHING ABOUT WHETHER ANYTHING IS REPLICATING IT.
#
# The grace question is "has this tenant had a fair chance to replicate yet",
# and the honest source for it is the SIDECAR — the process whose job that is
# (`sidecar_uptime`). It has been up longer than the grace and has never named
# this database: orphaned, whatever the file's age. It came up moments ago:
# nothing has had a chance yet, whatever the file's age. It is not running at
# all: that is itself the fault, and no grace applies to it.
judge_correspondence() {
    local what="$1" base="$2" newest="$3" db_file="$4" url="$5"

    # A tenant with no database on the volume is never a pass. The enumeration
    # path derives names FROM the files, so this only happens when a tenant was
    # named explicitly — and then the right answer is to say the name is wrong,
    # not to fall through the "brand new, not judged" door. It fell through
    # exactly that door on the first run of this function: an absent file left
    # the age at 0, which read as "created 0m ago" and returned OK.
    if [ ! -e "$db_file" ]; then
        stale "${what}: no database at ${db_file} — nothing to check. Either the name is wrong or the tenant's database is gone."
    fi

    # 1. IS THE SIDECAR WATCHING THIS DATABASE AT ALL?
    #
    # A position of all zeros is not a position. Litestream prints one for a file
    # it has picked up but ingested and shipped nothing from, and treating that
    # as an answer would let "the sidecar has seen it" stand in for "the sidecar
    # has replicated it". It goes down the same road as no position at all.
    if ! sidecar_position "$base" "$what" \
        || { txid_zero "$SIDE_DB" && txid_zero "$SIDE_REPLICA"; }; then
        judge_no_position "$what" "$base"
        return 0
    fi

    # 2. DID ANYTHING ACTUALLY LAND OFF-BOX?  Asked of S3 itself, because
    # everything else here is the sidecar's own account of its work and a backup
    # check that only asks the backing-up process how it is getting on is a check
    # that agrees with a liar. This is the pd-fof4 silent mode: nothing errors
    # when a tenant is simply absent from the bucket.
    #
    # There is no grace left on this branch and it needs none: step 1 has already
    # established that the sidecar is up, watching this database, and claims to
    # have shipped a real transaction from it. An empty replica under those
    # conditions is a contradiction, not a slow start.
    if [ -z "$newest" ]; then
        stale "${what}: no replica data at ${url%%\?*} — it is NOT backed up. The sidecar reports it replicated up to txid ${SIDE_REPLICA}, but that prefix holds no LTX files at all."
    fi

    # 3. IS THE SIDECAR STILL SPEAKING?  It reports about once a second per
    # database, so a long silence is not a quiet period, it is a stopped process.
    if [ -n "$SIDE_AGE" ] && [ "$SIDE_AGE" -gt "$SIDECAR_GRACE_SECONDS" ]; then
        stale "${what}: the sidecar last reported on ${base} $(( SIDE_AGE / 60 ))m ago (it reports about once a second). Replication has stopped."
    fi

    # 4. HAS EVERYTHING IT INGESTED REACHED S3?
    #
    # Behind is a VERDICT only after the re-ask, exactly as it is for the
    # registry. A lag of one transaction across a transient S3 error is the normal
    # shape of a blip, not of a lost backup, and paging over it is how an operator
    # learns to ignore the channel.
    if txid_lt "$SIDE_REPLICA" "$SIDE_DB"; then
        await_sidecar_correspondence "$base" "$what"
    fi

    if txid_lt "$SIDE_REPLICA" "$SIDE_DB"; then
        stale "${what}: its replica is BEHIND the local database — S3 holds up to txid ${SIDE_REPLICA}, Litestream has ingested ${SIDE_DB}. The newest changes are not backed up."
    fi

    # 5. AND IS IT STILL RUNNING NOW?  Everything above is read from what the
    # sidecar SAID, which outlives the process that said it — `podman logs` reads
    # a dead container's output just as well as a live one's. A sidecar that died
    # inside the grace window would otherwise pass on its own last words.
    if ! sidecar_uptime >/dev/null; then
        stale "${what}: the Litestream sidecar container '${SIDECAR_CTR}' is NOT RUNNING — it last reported on ${base} ${SIDE_AGE:-?}s ago, but nothing is replicating this database now."
    fi

    echo "backup-check: ${what} OK — replica in correspondence at txid ${SIDE_DB} (sidecar reported ${SIDE_AGE:-?}s ago)"
}

# judge_no_position <what> <base> — the verdict when the sidecar has said nothing
# about a database. Returns (not judged) only while the sidecar has genuinely not
# had time to; otherwise it trips the switch and never returns.
#
# The three answers are three different facts about the SIDECAR, and none of them
# is a fact about the file:
#
#   * not running        -> fault. Nothing is replicating anything, and the
#                           newest thing it ever said about this database is not
#                           evidence that it still is.
#   * up < grace         -> not judged. It has not finished scanning its tenants
#                           glob yet; a brand-new instance and a just-restarted
#                           sidecar both land here, briefly, and the next run
#                           judges them for real.
#   * up >= grace        -> fault. It has been watching for longer than it takes
#                           to notice a database and has never named this one:
#                           the glob is not matching the file, and every write to
#                           it is reaching NOTHING. This is the case that was
#                           SILENT while the grace came from the file's age — a
#                           newly created database on a long-running sidecar read
#                           as "too new to judge" and passed.
judge_no_position() {
    local what="$1" base="$2" up=""

    if ! up="$(sidecar_uptime)"; then
        stale "${what}: the Litestream sidecar has not reported a replication position for ${base}, and its container '${SIDECAR_CTR}' is not running — nothing is replicating this database, so every write to it is reaching NOTHING."
    fi

    if [ "$up" -lt "$SIDECAR_GRACE_SECONDS" ]; then
        echo "backup-check: ${what}: the sidecar has only been up ${up}s (grace ${SIDECAR_GRACE_SECONDS}s) and has not reported on ${base} yet — not judged"
        return 0
    fi

    stale "${what}: the Litestream sidecar has not reported a replication position for ${base} within the last ${SIDECAR_LOOKBACK}, and it has been up $(( up / 60 ))m (container '${SIDECAR_CTR}'). Its tenants glob is not matching this file — new writes are reaching NOTHING."
}

# Retained for the alarming gates, which drive it directly with synthetic
# timestamps to prove the LAG arithmetic. No longer on the live tenant path —
# judge_correspondence replaced it — but the reasoning below is why age alone was
# never the right question, and deleting it would delete the fixture that proves it.
judge_freshness() {
    local what="$1" newest="$2" db_file="$3" url="$4" age_h db_age=0 newest_epoch

    if [ -z "$newest" ]; then
        # Nothing has ever been replicated. That is the alarm condition —
        # unless the database itself is younger than the threshold, in which
        # case it was only just created and has not had a full window to reach
        # S3 yet.
        [ -e "$db_file" ] && db_age=$(( NOW - $(stat -c %Y "$db_file") ))
        if [ "$db_age" -gt "$MAX_AGE_SECONDS" ]; then
            stale "${what}: no replica data at ${url%%\?*} — it is NOT backed up"
        fi
        echo "backup-check: ${what} has no replica yet but was created $(( db_age / 60 ))m ago — not judged"
        return 0
    fi

    newest_epoch="$(date -d "$newest" +%s 2>/dev/null)" \
        || stale "${what}: could not parse replica timestamp: ${newest}"
    age_h=$(( ( NOW - newest_epoch ) / 3600 ))

    # The database's newest DATA write: the main file or its WAL, whichever is
    # later. Not the -shm, which merely opening the database touches.
    # `if` rather than `[ … ] && …`: this script runs under `set -e`, and a bare
    # test that evaluates FALSE as the last command of a line takes the whole run
    # down. A tenant with no -wal did exactly that — the check aborted before it
    # queried S3 for any tenant, and the integration gates caught it as "asked S3
    # about every tenant anyway (expected 2, got 0)".
    local db_epoch=0 wal_epoch=0
    if [ -e "$db_file" ]; then db_epoch=$(stat -c %Y "$db_file"); fi
    if [ -e "${db_file}-wal" ]; then wal_epoch=$(stat -c %Y "${db_file}-wal"); fi
    if [ "$wal_epoch" -gt "$db_epoch" ]; then db_epoch=$wal_epoch; fi

    if [ "$db_epoch" -gt "$newest_epoch" ]; then
        local lag_h=$(( ( db_epoch - newest_epoch ) / 3600 ))
        if [ "$lag_h" -gt "$MAX_AGE_HOURS" ]; then
            stale "${what}: the replica is ${lag_h}h BEHIND the database — writes are not reaching S3"
        fi
        echo "backup-check: ${what} OK — replica ${lag_h}h behind the database (<= ${MAX_AGE_HOURS}h)"
        return 0
    fi

    # Replica at or past the database: caught up. Age is not a fault — a collection
    # nobody edited has nothing to replicate, and restoring this replica reproduces
    # the current database exactly.
    echo "backup-check: ${what} OK — replica is caught up with the database (newest write ${age_h}h old, no changes since)"
}

# --- The user registry (pd-nd6w) -------------------------------------------
# Checked FIRST, and checked at all, because a registry that quietly stopped
# replicating is the one failure the tenant loop below cannot see: every tenant
# would still be fresh, and the thing that says whose database is whose would be
# gone. That is the DR gap this project rejected libSQL/sqld over.
#
# It is NOT checked as a tenant. The registry lives at the data root with its own
# `path:` replica prefix, so it needs its own URL — and passing "registry" to
# tenant_replica_url would silently address a tenant prefix that does not exist.
#
# A box that has never had a registry file (nothing writes one until the resolver
# lands) is not a failure: absent file AND absent replica is the pre-registry
# state, and it is judged only once a registry exists to be backed up.
#
# ── AND IT IS NOT JUDGED ON FRESHNESS (pd-me6h) ─────────────────────────────
# The registry is STATIC BY DESIGN. It maps handle -> database_id and changes
# only when a tenant is added, removed or renamed — legitimately months apart.
# Litestream writes S3 objects when there are WAL frames to ship, so a database
# nobody has written to produces no new objects however diligently the sidecar
# syncs. "Newest replica write is 65h old" is then TRUE, and means nothing is
# wrong.
#
# Judged on freshness it therefore cried wolf on prod's very first armed run,
# and would have gone on doing it every 36h forever — which is how an operator
# learns to ignore an alarm, and is strictly worse than having no alarm at all.
#
# A BIGGER THRESHOLD IS NOT THE FIX. It moves the false positive further out and
# re-breaks the moment the registry sits untouched for longer. The registry needs
# a DIFFERENT TEST: CORRESPONDENCE. Does the replica still hold what the local
# database holds? That question is age-independent by construction — it passes
# while the two agree no matter how long ago they last moved, and fails the
# moment the replica falls behind or is not there at all.
#
# The pair it turns on is Litestream's own: `txid.db` (what Litestream has
# ingested locally) against `txid.replica` (what has reached S3), the two values
# the sidecar prints on every single sync line. Nothing is re-derived here.
#
# The TENANT databases keep freshness, below, and must. They change daily, so a
# replica that stopped advancing IS the alarm there.
[ -n "$MOUNTPOINT" ] || stale "data volume '${VOLUME}' not found — cannot check the user registry"
REGISTRY_FILE="${MOUNTPOINT}/registry.sqlite"
REGISTRY_URL="$(registry_replica_url)" \
    || stale "could not derive the registry replica URL — run deploy/setup.sh ${INSTANCE} to backfill litestream.env"
REGISTRY_REPLICA_TXID="$(ltx_max_txid "$REGISTRY_URL" "the user registry")" \
    || stale "the user registry: could not read its replica at ${REGISTRY_URL%%\?*}"
local_position "$(registry_db_path)" "the user registry" \
    || stale "the user registry: could not read Litestream's local position for it"

# A replica that reads behind is not news YET. Two lags are normal and both
# resolve themselves:
#   * the ~1s window between Litestream ingesting a write and uploading it, and
#   * a transient S3 error, after which the un-uploaded checkpoint waits for the
#     next compaction tick — 30s, measured against a real sidecar and a real
#     S3 outage in tests/alarming/run.sh §4b, on a database nobody wrote to in
#     between.
# So a lag is RE-ASKED over a bounded window rather than judged on the first
# reading. A replica that has genuinely stopped never catches up, so the window
# costs nothing on the path that matters — only on the path that would otherwise
# have paged an operator over a blip.
#
# ONE WINDOW, BOTH LEGS. The same outage lags the tenants beside the registry —
# it is the same sidecar and the same un-uploaded checkpoint — so the tenant leg
# re-asks over this window too (await_sidecar_correspondence). It asks the
# sidecar rather than S3 because that is where a tenant's pair comes from; the
# window, and the reason for it, are these. Only the registry had one until
# pd-yglw, and the asymmetry showed up as a §4b that passed on a fast box and
# paged on a loaded one.
#
# Overridable so a gate provoking a permanent divergence on purpose need not sit
# through it. The poll interval follows the window down, so a short window is
# one immediate re-ask rather than none.
CORRESPONDENCE_GRACE_SECONDS="${PKDUMP_BACKUP_CORRESPONDENCE_GRACE_SECONDS:-90}"
CORRESPONDENCE_POLL_SECONDS=15
[ "$CORRESPONDENCE_GRACE_SECONDS" -lt "$CORRESPONDENCE_POLL_SECONDS" ] \
    && CORRESPONDENCE_POLL_SECONDS="$CORRESPONDENCE_GRACE_SECONDS"

# txid_lt <a> <b> — TXID a is strictly earlier than TXID b. Both are 16 hex
# characters wide, so ordering them is a string comparison and no arithmetic is
# involved — a 64-bit TXID would overflow bash's signed integers. `LC_ALL=C`
# because the collation of [0-9a-f] is the locale's business otherwise, and this
# comparison decides whether an operator gets paged.
txid_lt() {
    [ "$1" != "$2" ] \
        && [ "$(printf '%s\n%s\n' "$1" "$2" | LC_ALL=C sort | head -n1)" = "$1" ]
}

# txid_zero <a> — TXID a is the zero TXID: Litestream stands nowhere on this
# database. An empty string counts, because "it said nothing" and "it said
# nothing has happened" are the same amount of evidence that a backup exists.
txid_zero() {
    case "$1" in
        *[!0]*) return 1 ;;
        *) return 0 ;;
    esac
}
registry_behind() { # registry_behind — true while the replica has less than local
    [ -z "$REGISTRY_REPLICA_TXID" ] && return 0
    txid_lt "$REGISTRY_REPLICA_TXID" "$LOCAL_TXID"
}
await_registry_replica() {
    local waited=0
    echo "backup-check: the user registry: replica reads behind the local database" \
         "(local ${LOCAL_TXID}, replica ${REGISTRY_REPLICA_TXID:-none}) —" \
         "re-asking S3 for up to ${CORRESPONDENCE_GRACE_SECONDS}s in case a sync is still in flight"
    while registry_behind && [ "$waited" -lt "$CORRESPONDENCE_GRACE_SECONDS" ]; do
        sleep "$CORRESPONDENCE_POLL_SECONDS"
        waited=$(( waited + CORRESPONDENCE_POLL_SECONDS ))
        REGISTRY_REPLICA_TXID="$(ltx_max_txid "$REGISTRY_URL" "the user registry")" \
            || stale "the user registry: could not read its replica at ${REGISTRY_URL%%\?*}"
    done
    registry_behind \
        || echo "backup-check: the user registry: its replica caught up after ${waited}s"
}

# The registry's file age is a GRACE WINDOW here, never a verdict: a registry
# created minutes ago has legitimately not had a full sync cycle to reach S3, and
# nothing else in this branch distinguishes that from a replica that is gone.
REGISTRY_AGE=0
[ -e "$REGISTRY_FILE" ] && REGISTRY_AGE=$(( NOW - $(stat -c %Y "$REGISTRY_FILE") ))

case "$LOCAL_STATE" in
    "no database")
        # No registry file. Nothing writes one until a tenant is provisioned, so
        # an absent file with an absent replica is just the pre-registry state.
        # An absent file whose replica holds transactions is not: the local side
        # of the pair is gone, and that is divergence in the direction that ends
        # with a volume full of anonymous database ids.
        if [ -n "$REGISTRY_REPLICA_TXID" ]; then
            stale "the user registry: no registry.sqlite on this instance, but its replica holds transactions up to ${REGISTRY_REPLICA_TXID} — local and replica have diverged"
        fi
        echo "backup-check: no user registry on this instance yet — nothing to back up"
        ;;
    "ok")
        [ -n "$LOCAL_TXID" ] \
            || stale "the user registry: litestream status says 'ok' but names no local txid for ${REGISTRY_FILE}"
        if registry_behind; then
            await_registry_replica
        fi
        if [ -z "$REGISTRY_REPLICA_TXID" ]; then
            if [ "$REGISTRY_AGE" -gt "$MAX_AGE_SECONDS" ]; then
                stale "the user registry: no replica data at ${REGISTRY_URL%%\?*} — it is NOT backed up"
            fi
            echo "backup-check: the user registry has no replica yet but was created $(( REGISTRY_AGE / 60 ))m ago — not judged"
        elif txid_lt "$REGISTRY_REPLICA_TXID" "$LOCAL_TXID"; then
            stale "the user registry: its replica is BEHIND the local database — S3 holds up to txid ${REGISTRY_REPLICA_TXID}, the local database is at ${LOCAL_TXID}. Replication has stopped; the newest registry changes are not backed up."
        elif [ "$REGISTRY_REPLICA_TXID" = "$LOCAL_TXID" ]; then
            echo "backup-check: the user registry OK — replica in correspondence at txid ${LOCAL_TXID}" \
                 "(age is deliberately not judged: the registry is static by design)"
        else
            # Ahead, not behind — a local database that was restored to an
            # earlier point, or whose Litestream state was reset. Worth saying
            # out loud, but nothing local is missing from S3, which is the only
            # question this check exists to answer.
            echo "backup-check: the user registry OK — its replica is AHEAD of the local database" \
                 "(local ${LOCAL_TXID}, replica ${REGISTRY_REPLICA_TXID}); nothing local is unbacked-up"
        fi
        ;;
    "not initialized")
        # The file is there and Litestream has no local state for it. On a box
        # with a running sidecar that lasts seconds, so what it means afterwards
        # is that nothing is replicating this database — and with no local
        # position there is no correspondence to judge. Not being able to ask is
        # not the same answer as "fine".
        stale "the user registry: Litestream has no local state for ${REGISTRY_FILE} (status: not initialized) — the sidecar is not replicating it"
        ;;
    *)
        stale "the user registry: litestream status reports '${LOCAL_STATE}' for ${REGISTRY_FILE}"
        ;;
esac

# --- Query S3 for each tenant's replica (read-only) ------------------------
# Mirrors restore-litestream.sh's invocation: assume-role profile + bootstrap
# secret, region pinned in the derived replica URL. A read/list op — so broken
# creds surface here exactly as they would for replication.
for TENANT in "${TENANTS[@]}"; do
    REPLICA_URL="$(tenant_replica_url "$TENANT")" \
        || stale "tenant '${TENANT}': could not derive a replica URL (check litestream.env)"
    NEWEST="$(ltx_newest "$REPLICA_URL" "tenant '${TENANT}'")" \
        || stale "tenant '${TENANT}': could not read its replica at ${REPLICA_URL%%\?*}"
    judge_correspondence "tenant '${TENANT}'" "${TENANT}.sqlite" "$NEWEST" \
        "${MOUNTPOINT}/tenants/${TENANT}.sqlite" "$REPLICA_URL"
done

# --- Everything passed: ping the monitor + record the marker ---------------
mark_fresh
if [ -z "$PING" ]; then
    echo "backup-check: OK — the registry corresponds + ${#TENANTS[@]} tenant(s) fresh; no monitor to ping" \
         "(PKDUMP_BACKUP_PING_URL unset — Layer 1 cannot alert on a dead box)"
else
    echo "backup-check: OK — the registry corresponds + ${#TENANTS[@]} tenant(s) fresh; pinging monitor"
    curl -fsS -m 10 "$PING" >/dev/null 2>&1 || echo "backup-check: WARNING — monitor ping failed (will retry next run)" >&2
fi
