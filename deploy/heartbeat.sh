#!/usr/bin/env bash
# heartbeat.sh — prove the SITE IS SERVING, to an off-box monitor, every few minutes.
#
# THE GAP THIS FILLS. On 2026-08-16 Ryan reported: "i got NO alerts when the actual
# website was hard down because the server was offline." He was right, and there were
# two independent reasons:
#
#   1. NOTHING checked HTTP availability. The only box-liveness signal was the backup
#      freshness dead-man — an indirect proxy on a 6h period with a 3h grace, so up to
#      NINE HOURS late even when working.
#   2. That dead-man was already saturated. backup-check had failed on every run since
#      2026-08-14, tripping /fail each time, so healthchecks.io sat in a failed state.
#      A monitor already "down" does not re-alert, so a real outage produced no
#      transition and no page. The false alarm MASKED the real one.
#
# So this is deliberately a SEPARATE check with its own URL. Sharing one with backup
# freshness would rebuild exactly the masking that caused the silence: two unrelated
# failure modes collapsing onto one signal, where the noisy one hides the vital one.
#
# It asks the only question that matters to a visitor — does an HTTP request to the
# real listener return a page — and pings only when the answer is yes. Anything else
# (process alive but wedged, container up but 502, box gone) stops the ping, and the
# monitor's grace window turns that into an alert without this script needing to
# detect, classify or transmit anything. A dead box cannot send its own alarm; that is
# the entire point of an off-box dead-man.
#
# THE PROBE IS LOCAL AND THAT IS NOT A WEAKNESS. On 2026-08-15 this host went dark
# for 11h05m with the I226-V NIC wedged: it was never down, journald wrote straight
# through, and a localhost probe would have returned 200 the entire time. The ping
# still could not have left the box, because it has to cross the NIC that wedged.
#
# So the PING is the reachability test and the probe only decides whether one is
# worth sending. Silence covers both "the site is broken" and "nobody outside can
# reach it" — a pair that the Telegram bot and cloudflared both detected within
# seconds of that outage and could only write to a local log nobody could read.
set -uo pipefail

INSTANCE="${1:-prod}"
CONF_DIR="${PKDUMP_CONF_DIR:-$HOME/.config/pkdump/$INSTANCE}"
[ -f "$CONF_DIR/alerts.env" ] && . "$CONF_DIR/alerts.env"

URL="${PKDUMP_HEARTBEAT_URL:-}"
PING="${PKDUMP_UPTIME_PING_URL:-}"
TIMEOUT="${PKDUMP_HEARTBEAT_TIMEOUT:-10}"

if [ -z "$URL" ]; then
    echo "heartbeat: PKDUMP_HEARTBEAT_URL unset for '${INSTANCE}' — nothing to probe" >&2
    exit 0
fi

# --fail so a 5xx is a failure rather than a page of error text, and -sS so a real
# problem still prints a reason. No retry on purpose: a retry here would paper over
# exactly the intermittent unavailability a visitor would experience.
#
# When the site closes /health to anonymous callers (PKDUMP_HEALTH_REQUIRE_AUTH=1) a probe of
# its PUBLIC URL must carry a Pocket ID client_credentials token. Set all three of
# PKDUMP_HEARTBEAT_CLIENT_ID / _CLIENT_SECRET / _TOKEN_URL in alerts.env and it is sent;
# leave them unset and nothing changes. A probe of the host-local listener needs none: the
# app admits loopback. A token that cannot be obtained is a failed probe, never a skipped one.
auth=()
if [ -n "${PKDUMP_HEARTBEAT_CLIENT_ID:-}" ] || [ -n "${PKDUMP_HEARTBEAT_CLIENT_SECRET:-}" ] \
    || [ -n "${PKDUMP_HEARTBEAT_TOKEN_URL:-}" ]; then
    tok="$(curl -sS --max-time "$TIMEOUT" -X POST "${PKDUMP_HEARTBEAT_TOKEN_URL:-}" \
        --data-urlencode grant_type=client_credentials \
        --data-urlencode "client_id=${PKDUMP_HEARTBEAT_CLIENT_ID:-}" \
        --data-urlencode "client_secret=${PKDUMP_HEARTBEAT_CLIENT_SECRET:-}" \
        ${PKDUMP_HEARTBEAT_AUDIENCE:+--data-urlencode "audience=$PKDUMP_HEARTBEAT_AUDIENCE"} 2>/dev/null \
        | sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p')" || tok=""
    if [ -n "$tok" ]; then
        auth=(-H "Authorization: Bearer $tok")
    else
        echo "heartbeat: could not obtain a bearer token — probe fails" >&2
        code="000"
    fi
fi

if [ "${code:-}" != "000" ]; then
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT" ${auth[@]+"${auth[@]}"} "$URL" 2>&1)" || code="000"
fi

if [ "$code" = "200" ]; then
    if [ -n "$PING" ]; then
        curl -fsS -m 10 "$PING" >/dev/null 2>&1 \
            || echo "heartbeat: served 200 but the monitor ping failed (will retry next run)" >&2
    else
        echo "heartbeat: ${INSTANCE} served 200 — but PKDUMP_UPTIME_PING_URL is unset," \
             "so NOTHING off-box would notice this box disappearing" >&2
    fi
    echo "heartbeat: ${INSTANCE} OK — ${URL} served 200"
    exit 0
fi

# DO NOT ping /fail here. An outage is signalled by SILENCE, not by a message: a box
# that has lost power, lost its network, or been OOM-killed cannot send anything, and a
# design that depended on it doing so would miss the case it exists for. The monitor's
# grace window is what turns silence into an alert.
echo "heartbeat: ${INSTANCE} FAILED — ${URL} returned '${code}'; withholding the ping so the off-box monitor alarms" >&2
exit 1
