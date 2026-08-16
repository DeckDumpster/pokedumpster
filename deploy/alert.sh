#!/usr/bin/env bash
#
# Push a notification via Pushover (pokedumpster-ivq.1: channel = Pushover).
# The shared notification sink for every alarming layer:
#   - backup-check.sh  (Layer 1, staleness detected on a live box)
#   - diskcheck.sh     (Layer 4, low disk)
#   - pkdump-alert@.service (Layer 2, a systemd unit's journal tail on failure)
#
# Reads PUSHOVER_TOKEN / PUSHOVER_USER from the environment — load the host-wide
# ~/.config/pkdump/alerts.env first. No new runtime deps beyond curl.
#
# Asked to alert and unable to alert is a FAILURE (exit 1), not a no-op (pd-1717,
# same defect class as backup-check.sh's old "skipping"). Nothing calls this
# script speculatively: every caller has already decided something is wrong, so
# an unconfigured channel means that decision reached nobody. Exiting non-zero
# makes the caller's unit fail, which is the only remaining way to see it.
#
# A page that repeats unchanged is a page that trains you to ignore the channel
# (pd-hqdt), so an identical alert is sent ONCE per window — see "repeat
# suppression" below.
#
# Usage:
#   alert.sh "<title>" "<message>"     # message as an argument
#   some_cmd | alert.sh "<title>"      # message on stdin (e.g. a journal tail)
set -euo pipefail

# The Pushover endpoint. Production never sets this; tests point it at a local
# sink to prove the push actually leaves the script (tests/alarming/run.sh).
PUSHOVER_API_URL="${PUSHOVER_API_URL:-https://api.pushover.net/1/messages.json}"

TITLE="${1:-PokeDumpster alert}"
# Second arg is the message; if absent, read it from stdin (pipe form).
if [ "$#" -ge 2 ]; then
    MSG="$2"
else
    MSG="$(cat)"
fi

# CHANGE_ME is what deploy/setup.sh scaffolds into alerts.env, so treat it as
# unset — a placeholder that reached curl would fail the request anyway, just
# later and less legibly.
if [ -z "${PUSHOVER_TOKEN:-}" ] || [ -z "${PUSHOVER_USER:-}" ] \
   || [ "${PUSHOVER_TOKEN}" = CHANGE_ME ] || [ "${PUSHOVER_USER}" = CHANGE_ME ]; then
    echo "alert.sh: FAILED — PUSHOVER_TOKEN/USER unset or still CHANGE_ME; this alert reached nobody." >&2
    echo "  Dropped alert: ${TITLE}" >&2
    echo "  Fix: fill ~/.config/pkdump/alerts.env, then: bash deploy/alarm-status.sh <instance>" >&2
    exit 1
fi

# --- repeat suppression (pd-hqdt) -------------------------------------------
# pkdump-value-snapshots@prod failed the same way four nights running and pushed
# four byte-identical pages. Every one was CORRECT and not one was actionable,
# and what they bought was a human who swipes this channel away — which is how
# the outage that came next (the sidecar's rootless-netns failure) reached
# nobody. A pager that repeats itself is a pager being switched off by hand.
#
# So: the same alert is sent ONCE per window. Four rules hold it honest.
#
#   1. The FIRST page always goes, and SAYS it will not repeat. A reader who is
#      not told that reads the silence afterwards as "it stopped", which is the
#      one misreading worse than the noise.
#   2. A CHANGED alert always goes, immediately, however recently its neighbour
#      did. Suppression is keyed on what the page SAYS, never on the unit alone.
#   3. Anything that cannot be decided pages. No sha256sum, no writable state
#      dir, a clock that went backwards, an unreadable stamp — every one of them
#      sends. This is alert-gate.sh's rule and it is the same rule: a silently
#      disarmed alert is indistinguishable from a backup that quietly stopped.
#   4. Nothing is recorded unless the push SUCCEEDED. A curl that failed must be
#      free to try again on the next run; recording first would turn one dropped
#      page into a whole window of silence.
#
# The signature is (exact title, message with digit runs collapsed). The title
# is exact because that is where every caller puts identity and severity — the
# instance, the failed unit, the disk percentage — and an escalation from 85% to
# 99% must page. The message is normalised because the SAME failure carries
# numbers that move on every occurrence: ages, byte counts, pids, clocks. A
# signature any digit defeats would suppress nothing on the days it matters;
# these four pages were byte-identical only because that particular message
# happens to carry no numbers. What it costs is real and deliberate: two
# failures of one caller whose text differs ONLY in a number read as one
# signature. A failure that is genuinely different says so in words.
SUPPRESS_SECONDS="${PKDUMP_ALERT_SUPPRESS_SECONDS:-86400}"
ALERT_STATE_DIR="${PKDUMP_ALERT_STATE_DIR:-${XDG_STATE_HOME:-${HOME:-/tmp}/.local/state}/pkdump/alerts}"

# 86400 -> "24h", 5400 -> "1h30m", 300 -> "5m", 45 -> "45s". The page quotes the
# window that is actually configured, not the one this comment was written
# against — and a test running a 2-second window reads as "2s", not "0h".
window_words() {
    if [ "$1" -lt 60 ]; then printf '%ds' "$1"
    elif [ "$1" -lt 3600 ]; then printf '%dm' $(($1 / 60))
    elif [ $(($1 % 3600)) -eq 0 ]; then printf '%dh' $(($1 / 3600))
    else printf '%dh%dm' $(($1 / 3600)) $((($1 % 3600) / 60)); fi
}

STAMP=""
NOTICE=""
if [ "$SUPPRESS_SECONDS" -gt 0 ] && [ -z "${PKDUMP_ALERT_NO_SUPPRESS:-}" ] &&
    command -v sha256sum >/dev/null 2>&1 &&
    mkdir -p "$ALERT_STATE_DIR" 2>/dev/null; then

    SIG="$({ printf '%s\n' "$TITLE"; printf '%s' "$MSG" | sed 's/[0-9][0-9]*/#/g'; } |
        sha256sum | cut -c1-40)"
    STAMP="${ALERT_STATE_DIR}/${SIG}"
    NOW="$(date +%s)"

    if [ -r "$STAMP" ]; then
        LAST="$(head -n1 "$STAMP" 2>/dev/null | tr -dc '0-9')"
        AGE=$((NOW - ${LAST:-0}))
        # A negative age means the clock moved backwards under us; page.
        if [ -n "$LAST" ] && [ "$AGE" -ge 0 ] && [ "$AGE" -lt "$SUPPRESS_SECONDS" ]; then
            echo "alert.sh: SUPPRESSED — identical to the page sent $(window_words "$AGE") ago; this alert can page again in $(window_words $((SUPPRESS_SECONDS - AGE)))." >&2
            echo "  Suppressed alert: ${TITLE}" >&2
            echo "  This is pd-hqdt, not a delivery failure. A CHANGED alert pages immediately." >&2
            exit 0
        fi
    fi

    # This page is going out, so it is the one that owes the reader the notice.
    NOTICE="$(printf '\n\n(Repeats of this same alert are suppressed for %s — you will NOT be paged again for it unless it changes.)' \
        "$(window_words "$SUPPRESS_SECONDS")")"
fi

# Pushover caps message length at 1024 chars; trim to stay well under. Keep the
# HEAD, not the tail (pd-pwk8): every caller leads with the cause — the checkers
# say what is wrong in their first line, and journal-summary.sh exists precisely
# to put the causal line first. `tail -c` kept whatever the writer put LAST,
# which for a journal tail is systemd's boilerplate.
# The final sed drops a multi-byte character the cut may have split in half;
# invalid UTF-8 would make Pushover reject the whole request.
#
# The notice is appended AFTER the trim and paid for out of the same budget: it
# is the one line that must survive on a page long enough to be cut, and a
# trailing line is the first thing a head-trim throws away.
BUDGET=$((900 - $(printf '%s' "$NOTICE" | wc -c)))
if [ "$(printf '%s' "$MSG" | wc -c)" -gt "$BUDGET" ]; then
    MSG="$(printf '%s' "$MSG" | head -c "$((BUDGET - 3))" |
        LC_ALL=C sed '$s/[\xc0-\xff][\x80-\xbf]*$//')..."
fi

curl -fsS -m 15 \
    --form-string "token=${PUSHOVER_TOKEN}" \
    --form-string "user=${PUSHOVER_USER}" \
    --form-string "title=${TITLE}" \
    --form-string "message=${MSG}${NOTICE}" \
    --form-string "priority=${PUSHOVER_PRIORITY:-0}" \
    "$PUSHOVER_API_URL" >/dev/null

# Delivered — now, and only now, start the window. The second line is for a
# human reading the state dir; nothing parses it.
if [ -n "$STAMP" ]; then
    { date +%s; printf '%s\n' "$TITLE"; } >"$STAMP" 2>/dev/null || true
    # Keep the directory from growing a file per distinct failure forever. A
    # stamp older than the window can never suppress anything again.
    find "$ALERT_STATE_DIR" -maxdepth 1 -type f \
        -mmin "+$(((SUPPRESS_SECONDS + 59) / 60))" -delete 2>/dev/null || true
fi
