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

# Pushover caps message length at 1024 chars; trim to stay well under.
MSG="$(printf '%s' "$MSG" | tail -c 900)"

curl -fsS -m 15 \
    --form-string "token=${PUSHOVER_TOKEN}" \
    --form-string "user=${PUSHOVER_USER}" \
    --form-string "title=${TITLE}" \
    --form-string "message=${MSG}" \
    --form-string "priority=${PUSHOVER_PRIORITY:-0}" \
    "$PUSHOVER_API_URL" >/dev/null
