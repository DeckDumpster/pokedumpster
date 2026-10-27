#!/usr/bin/env bash
# health-probe-lib.sh — the ONE way a host-local probe asks a running poke whether it is up.
#
# With PKDUMP_HEALTH_REQUIRE_AUTH=1 /health needs a session or a Pocket ID client_credentials
# bearer. There is no loopback exemption in the app and there cannot be: behind the rootless
# podman publish the container never sees a loopback peer, and cloudflared reaches the same host
# port, so an address cannot tell a host probe from the internet (sp-396d3g). So every host probe
# carries a token from the host's machine client, read from <conf>/probe.env (0600, written by
# deploy/oidc-config.sh):
#
#   PKDUMP_PROBE_CLIENT_ID  PKDUMP_PROBE_CLIENT_SECRET  PKDUMP_PROBE_TOKEN_URL  [PKDUMP_PROBE_AUDIENCE]
#
# Unset entirely means no token is sent (the flag is off). PKDUMP_HEARTBEAT_* are accepted as the
# older spelling. A PARTIAL set, or a token that cannot be obtained, is a failed probe — never a
# probe sent without credentials that would then be misread as an app failure.
#
#   pkdump_health_probe <url> [timeout-seconds]   -> prints the HTTP status ("000" = no answer)

pkdump_health_probe() {
    local url="${1:?url}" timeout="${2:-10}"
    local id="${PKDUMP_PROBE_CLIENT_ID:-${PKDUMP_HEARTBEAT_CLIENT_ID:-}}"
    local secret="${PKDUMP_PROBE_CLIENT_SECRET:-${PKDUMP_HEARTBEAT_CLIENT_SECRET:-}}"
    local token_url="${PKDUMP_PROBE_TOKEN_URL:-${PKDUMP_HEARTBEAT_TOKEN_URL:-}}"
    local aud="${PKDUMP_PROBE_AUDIENCE:-${PKDUMP_HEARTBEAT_AUDIENCE:-}}"
    local auth=() tok code

    if [ -n "$id$secret$token_url" ]; then
        if [ -z "$id" ] || [ -z "$secret" ] || [ -z "$token_url" ]; then
            echo "health-probe: client id, secret and token URL must all be set — probe fails" >&2
            echo 000
            return 0
        fi
        tok="$(curl -sS --max-time "$timeout" -X POST "$token_url" \
            --data-urlencode grant_type=client_credentials \
            --data-urlencode "client_id=$id" \
            --data-urlencode "client_secret=$secret" \
            ${aud:+--data-urlencode "audience=$aud"} 2>/dev/null \
            | sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p')" || tok=""
        if [ -z "$tok" ]; then
            echo "health-probe: could not obtain a bearer token — probe fails" >&2
            echo 000
            return 0
        fi
        auth=(-H "Authorization: Bearer $tok")
    fi
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$timeout" ${auth[@]+"${auth[@]}"} "$url" 2>&1)" || code=000
    echo "$code"
}
