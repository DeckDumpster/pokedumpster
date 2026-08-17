#!/usr/bin/env bash
#
# "Is backup alarming actually ARMED on this instance?" — one command, one
# truthful yes/no (pd-dfb2118c).
#
# The failure this exists to make impossible: every backup-shaped signal reading
# green while nothing is watching. That has now happened twice. In Jun 2026 the
# nightly backup failed for 11 days with the unit looking fine; on 2026-08-08
# Litestream sat `active`, logged "snapshot complete", and replicated nothing to
# S3 for 20 minutes. Both were noticed by a human happening to look.
#
# The alarming layers were built after the first incident and then never armed —
# and nothing distinguished "armed" from "installed but inert", because every
# individual piece looked healthy: the units were installed, the config files
# existed, the scripts exited 0. This script asks the only question that matters
# — if a backup broke right now, would anything reach Ryan? — and answers it
# with an exit code.
#
# It is strict on purpose. ARMED means every gate passed, including that the
# checker has actually COMPLETED A SUCCESSFUL RUN and left a fresh marker.
# "Configured but never run" is reported as NOT ARMED, because an untested
# alarm is indistinguishable from a broken one.
#
# Read-only by default: it inspects units, config and the data volume, and
# sends nothing. Safe to run against prod.
#
# Usage:
#   bash deploy/alarm-status.sh <instance>             # inspect (read-only)
#   bash deploy/alarm-status.sh <instance> --verify    # inspect, then EXERCISE:
#                                                      #   run the checker for real
#                                                      #   (pings the monitor) and
#                                                      #   send a real Pushover push
#
# Exit: 0 = ARMED, 1 = NOT ARMED.
set -uo pipefail   # NOT -e: this script's job is to report failures, not die on one

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

VERIFY=false
POSITIONAL=()
for arg in "$@"; do
    case "$arg" in
        --verify) VERIFY=true ;;
        -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done
if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/alarm-status.sh <instance> [--verify]" >&2
    exit 1
fi
INSTANCE="${POSITIONAL[0]}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
# Unit-name prefix. Production is always 'pkdump'; only tests set this, so a gate
# can install prefixed copies of the SHIPPED templates and exercise this script
# end-to-end without writing over the operator's real units (tests/alarming/run.sh).
P="${PKDUMP_UNIT_PREFIX:-pkdump}"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
VOLUME="pkdump-${INSTANCE}-data"

# Same file resolution as the scripts being audited, including the test-only
# override — otherwise this could report a state no layer would actually see.
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"
[ -f "$ALERTS_ENV" ]                && { set -a; . "$ALERTS_ENV";                set +a; }
[ -f "${CONF_DIR}/alerts.env" ]     && { set -a; . "${CONF_DIR}/alerts.env";     set +a; }
[ -f "${CONF_DIR}/litestream.env" ] && { set -a; . "${CONF_DIR}/litestream.env"; set +a; }

# The banner's threshold, so this agrees with what the app shows (Layer 3 /
# crates/pkdump-server/src/routes/backup.rs).
STALE_HOURS="${PKDUMP_BACKUP_STALE_HOURS:-12}"

FAILURES=0
REMEDIES=()

ok()   { printf '  \033[32mOK  \033[0m %s\n' "$1"; }
bad()  {
    printf '  \033[31mFAIL\033[0m %s\n' "$1"
    FAILURES=$((FAILURES + 1))
    [ -n "${2:-}" ] && REMEDIES+=("$2")
    return 0
}
info() { printf '  ---- %s\n' "$1"; }
layer(){ printf '\n%s\n' "$1"; }

gate() { # gate <condition-result> <label> [remedy]
    if [ "$1" = 0 ]; then ok "$2"; else bad "$2" "${3:-}"; fi
}

# A configured value is one that is non-empty AND not the placeholder
# deploy/setup.sh scaffolds. "CHANGE_ME is set" is exactly the kind of
# technically-true reading this script exists to refuse.
configured() { [ -n "${1:-}" ] && [ "${1}" != CHANGE_ME ]; }

unit_prop() { systemctl --user show "$1" -p "$2" --value 2>/dev/null; }

echo "PokeDumpster backup alarming — instance '${INSTANCE}'"
echo "  config: ${CONF_DIR}/alerts.env, ${ALERTS_ENV}"

# ── Layer 1 — off-box freshness dead-man's switch (primary) ─────────────────
# The only layer that survives the box going away, and the only one that catches
# "replication silently stopped". Everything else is a nicety next to this.
layer "Layer 1 — off-box freshness dead-man's switch (${P}-backup-check@${INSTANCE})"

[ -f "${SYSTEMD_USER_DIR}/${P}-backup-check@.service" ] && [ -f "${SYSTEMD_USER_DIR}/${P}-backup-check@.timer" ]
gate $? "checker units installed" "bash deploy/setup.sh ${INSTANCE}   # reinstall the units"

TIMER="${P}-backup-check@${INSTANCE}.timer"
[ "$(systemctl --user is-enabled "$TIMER" 2>/dev/null)" = enabled ]
gate $? "timer enabled" "systemctl --user enable --now ${TIMER}"

[ "$(systemctl --user is-active "$TIMER" 2>/dev/null)" = active ]
gate $? "timer running" "systemctl --user start ${TIMER}"

if configured "${PKDUMP_BACKUP_PING_URL:-}"; then
    ok "off-box monitor configured (${PKDUMP_BACKUP_PING_URL%%\?*})"
else
    bad "off-box monitor NOT configured — nothing off this box is watching" \
        "\$EDITOR ${CONF_DIR}/alerts.env   # set PKDUMP_BACKUP_PING_URL"
fi

# Has it ever actually completed? A oneshot that has never started also reports
# Result=success, so the exit timestamp is what separates "passed" from "never
# tried". An unexercised alarm is not an armed alarm.
SVC="${P}-backup-check@${INSTANCE}.service"
LAST_EXIT="$(unit_prop "$SVC" ExecMainExitTimestamp)"
LAST_RESULT="$(unit_prop "$SVC" Result)"
if [ -z "$LAST_EXIT" ]; then
    bad "checker has never run — its success is unproven" \
        "systemctl --user start ${SVC}   # run it once, now"
elif [ "$LAST_RESULT" = success ]; then
    ok "last checker run succeeded (${LAST_EXIT})"
else
    bad "last checker run FAILED (result=${LAST_RESULT}, ${LAST_EXIT})" \
        "journalctl --user -u ${SVC} -n 40 --no-pager"
fi

# The marker is the checker's own record of the last time it CONFIRMED the S3
# replicas were fresh. It is also what the in-app banner reads, so a stale marker
# is a real backup problem, not a reporting one.
MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME" 2>/dev/null)"
MARKER="${MOUNTPOINT:-/nonexistent}/.backup-last-ok"
if [ -z "$MOUNTPOINT" ]; then
    bad "data volume '${VOLUME}' not found — cannot read the freshness marker" \
        "podman volume ls | grep ${VOLUME}"
elif [ ! -f "$MARKER" ]; then
    bad "no .backup-last-ok marker — no run has ever confirmed a fresh replica" \
        "systemctl --user start ${SVC}   # run it once, now"
else
    MARKER_EPOCH="$(tr -dc '0-9' < "$MARKER")"
    MARKER_AGE_H=$(( ( $(date +%s) - ${MARKER_EPOCH:-0} ) / 3600 ))
    if [ "$MARKER_AGE_H" -le "$STALE_HOURS" ]; then
        ok "last confirmed-fresh backup ${MARKER_AGE_H}h ago (<= ${STALE_HOURS}h)"
    else
        bad "last confirmed-fresh backup ${MARKER_AGE_H}h ago (> ${STALE_HOURS}h) — BACKUPS ARE STALE" \
            "bash deploy/backup-check.sh ${INSTANCE}   # see which tenant"
    fi
fi

# ── Layer 2 — OnFailure push ────────────────────────────────────────────────
layer "Layer 2 — OnFailure push to Pushover (${P}-alert@)"

[ -f "${SYSTEMD_USER_DIR}/${P}-alert@.service" ]
gate $? "alert unit installed" "bash deploy/setup.sh ${INSTANCE}"

if configured "${PUSHOVER_TOKEN:-}" && configured "${PUSHOVER_USER:-}"; then
    ok "Pushover credentials configured"
else
    bad "Pushover credentials unset or still CHANGE_ME — every push would be dropped" \
        "\$EDITOR ${ALERTS_ENV}   # set PUSHOVER_TOKEN and PUSHOVER_USER"
fi

# Wiring, not intent: ask systemd what each unit will actually do on failure.
# A unit missing its OnFailure= fails in silence, which is how the sidecar's
# crash-loop would have gone unnoticed.
for U in "${P}-litestream-${INSTANCE}.service" "${P}-refresh@${INSTANCE}.service" "$SVC"; do
    ONFAIL="$(unit_prop "$U" OnFailure)"
    case "$ONFAIL" in
        "${P}-alert@"*) ok "${U} fires ${ONFAIL} on failure" ;;
        "")             bad "${U} has no OnFailure= (or is not installed) — its failures are silent" \
                            "bash deploy/setup.sh ${INSTANCE}" ;;
        *)              bad "${U} fires '${ONFAIL}', not ${P}-alert@" ;;
    esac
done

# ── Layer 4 — low-disk alert (host-wide) ────────────────────────────────────
layer "Layer 4 — low-disk alert (${P}-diskcheck, host-wide)"

[ "$(systemctl --user is-enabled "${P}-diskcheck.timer" 2>/dev/null)" = enabled ]
gate $? "disk timer enabled" "systemctl --user enable --now ${P}-diskcheck.timer"

[ "$(systemctl --user is-active "${P}-diskcheck.timer" 2>/dev/null)" = active ]
gate $? "disk timer running" "systemctl --user start ${P}-diskcheck.timer"

DISK_PATH="${PKDUMP_DISK_PATH:-$HOME}"
info "watching ${DISK_PATH} — $(df --output=pcent "$DISK_PATH" 2>/dev/null | tail -n1 | tr -d ' ' || echo '?') used, alerts at ${PKDUMP_DISK_THRESHOLD:-90}%"

# ── Layer 3 — in-app banner (passive, never gates) ──────────────────────────
# Deliberately not a gate: the banner pages nobody, so its absence cannot make
# the difference between alarmed and unalarmed. Reported because a disagreement
# between it and the marker above is worth seeing.
layer "Layer 3 — in-app staleness banner (passive — not a gate)"
APP_PORT="$(podman port "systemd-${P}-${INSTANCE}" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2)"
if [ -z "$APP_PORT" ]; then
    info "app container not running — banner state not queried"
else
    BODY="$(curl -fsS -m 5 "http://localhost:${APP_PORT}/api/backup-status" 2>/dev/null)"
    if [ -z "$BODY" ]; then
        info "app is up but /api/backup-status did not answer"
    else
        info "app reports: ${BODY}"
    fi
fi

# ── Verdict ─────────────────────────────────────────────────────────────────
echo ""
if [ "$FAILURES" -eq 0 ]; then
    printf '\033[32mALARMING: ARMED\033[0m — instance %s. A broken backup would reach you.\n' "$INSTANCE"
else
    printf '\033[31mALARMING: NOT ARMED\033[0m — instance %s. %d check(s) failed.\n' "$INSTANCE" "$FAILURES"
    echo "A backup could break right now and nothing would tell you."
    if [ ${#REMEDIES[@]} -gt 0 ]; then
        echo ""
        echo "To arm it:"
        # One line per distinct action, in the order they were hit: several gates
        # share a fix ("run it once") and a list that repeats itself reads like
        # more work than it is.
        printf '  %s\n' "${REMEDIES[@]}" | awk '!seen[$0]++'
    fi
fi

# ── --verify: stop reading, start firing ────────────────────────────────────
# Reading configuration proves configuration. This proves DELIVERY: it runs the
# real checker (which pings the real monitor) and sends a real push. It is the
# last step of arming, and the one that turns "should work" into "did".
if [ "$VERIFY" = true ]; then
    layer "--verify — exercising the layers for real"

    VERIFY_FAILURES=0

    echo "  running ${SVC} (this pings the off-box monitor)..."
    # `systemctl start` on a Type=oneshot blocks until it finishes and exits
    # non-zero if it failed — but only when its output is NOT piped, or the
    # pipeline's status is sed's. Keep the two separate.
    systemctl --user start "$SVC" >/dev/null 2>&1 || VERIFY_FAILURES=$((VERIFY_FAILURES + 1))
    echo "    checker exited $(unit_prop "$SVC" ExecMainStatus) (result=$(unit_prop "$SVC" Result))"
    journalctl --user -u "$SVC" -n 15 --no-pager 2>/dev/null | sed 's/^/    /'

    echo ""
    echo "  sending a test Pushover push..."
    # PKDUMP_ALERT_NO_SUPPRESS: --verify exists to answer "would a page reach
    # Ryan RIGHT NOW", and the whole point of pd-hqdt's suppression is that an
    # identical alert does not. Two verifies in a day differ only in a clock, so
    # the second would be suppressed and read as a delivery failure — arming
    # would report NOT ARMED on a perfectly healthy channel.
    if PKDUMP_ALERT_NO_SUPPRESS=1 bash "${SCRIPT_DIR}/alert.sh" "PokeDumpster alarm test (${INSTANCE})" \
            "Test push from alarm-status.sh --verify on $(hostname) at $(date -u +%Y-%m-%dT%H:%M:%SZ). If you are reading this, Layer 2/4 delivery works."; then
        echo "    push accepted by Pushover — check your phone."
    else
        echo "    PUSH FAILED — Layers 2 and 4 would reach nobody."
        VERIFY_FAILURES=$((VERIFY_FAILURES + 1))
    fi

    echo ""
    if [ "$VERIFY_FAILURES" -eq 0 ]; then
        printf '  \033[32mVERIFY: both channels delivered.\033[0m\n'
    else
        printf '  \033[31mVERIFY: %d channel(s) failed to deliver.\033[0m\n' "$VERIFY_FAILURES"
    fi
    FAILURES=$((FAILURES + VERIFY_FAILURES))
    echo "  Confirm the OTHER end yourself: the healthchecks.io check for"
    echo "  '${INSTANCE}' should have just gone green, and the push should be on your phone."
fi

[ "$FAILURES" -eq 0 ]
