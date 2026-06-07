#!/usr/bin/env bash
#
# Push a notification via Pushover (pokedumpster-ivq.1: channel = Pushover).
# The shared notification sink for every alarming layer:
#   - backup-check.sh  (Layer 1, staleness detected on a live box)
#   - diskcheck.sh     (Layer 4, low disk)
#   - pkdump-alert@.service (Layer 2, a systemd unit's journal tail on failure)
#
# Reads PUSHOVER_TOKEN / PUSHOVER_USER from the environment — load the host-wide
# ~/.config/pkdump/alerts.env first. No-op (exit 0) when unset, so dev/test and
# unconfigured boxes are unaffected. No new runtime deps beyond curl.
#
# Usage:
#   alert.sh "<title>" "<message>"     # message as an argument
#   some_cmd | alert.sh "<title>"      # message on stdin (e.g. a journal tail)
set -euo pipefail

TITLE="${1:-PokeDumpster alert}"
# Second arg is the message; if absent, read it from stdin (pipe form).
if [ "$#" -ge 2 ]; then
    MSG="$2"
else
    MSG="$(cat)"
fi

if [ -z "${PUSHOVER_TOKEN:-}" ] || [ -z "${PUSHOVER_USER:-}" ]; then
    echo "alert.sh: PUSHOVER_TOKEN/USER unset — skipping push (title: ${TITLE})" >&2
    exit 0
fi

# Pushover caps message length at 1024 chars; trim to stay well under.
MSG="$(printf '%s' "$MSG" | tail -c 900)"

curl -fsS -m 15 \
    --form-string "token=${PUSHOVER_TOKEN}" \
    --form-string "user=${PUSHOVER_USER}" \
    --form-string "title=${TITLE}" \
    --form-string "message=${MSG}" \
    --form-string "priority=${PUSHOVER_PRIORITY:-0}" \
    https://api.pushover.net/1/messages.json >/dev/null
