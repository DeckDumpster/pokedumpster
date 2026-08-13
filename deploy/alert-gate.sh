#!/usr/bin/env bash
# alert-gate.sh — decide whether a failed unit should page a human.
#
# pd-n0lf. Every alerting unit fires `OnFailure=pkdump-alert@%n.service`, which is
# right for prod and wrong for everything else: a CI disaster-recovery drill stands
# up pkdump-litestream-drill-<hash>, tears it down as the test intends, and systemd
# reports the teardown as a failure. Nine such pages fired between 22:59 and 04:11
# on 2026-08-12/13 — every one a drill, none prod.
#
# WHY THE GATE LIVES HERE and not in the unit files: the per-instance units
# (pkdump-<inst>.container, pkdump-litestream-<inst>.container) could have their
# OnFailure= stripped at render, but the timer units are TEMPLATES — one
# pkdump-refresh@.service file backs prod and every drill alike. Editing it for a
# drill disarms prod. The documented fix, an `OnFailure=` drop-in resetting the
# list, DOES NOT WORK: verified on systemd 255, the inherited value survives both
# an empty assignment and an empty-then-set pair. So the decision is made once,
# here, at the moment of notification, where the failed unit's identity is known.
#
# FAIL TOWARD PAGING. An unrecognised unit name pages. A silently disarmed alert
# looks exactly like a backup that quietly stopped running, which is the failure
# this whole layer exists to catch.
#
# Usage: alert-gate.sh <failed-unit-name>   -> exit 0 = page, 1 = stay silent
#        alert-gate.sh --self-test

# Units that guard the whole host rather than one instance. These always page.
PKDUMP_HOST_WIDE_UNITS="${PKDUMP_HOST_WIDE_UNITS:-pkdump-diskcheck pkdump-nessie}"

# pkdump_units_alerting <instance> — true when this instance may page.
# Overridable for a second real deployment: PKDUMP_ALERT_INSTANCES="prod staging".
pkdump_units_alerting() {
    local instance="$1" want
    for want in ${PKDUMP_ALERT_INSTANCES:-prod}; do
        [ "$instance" = "$want" ] && return 0
    done
    return 1
}

# pkdump_alert_instance_of <unit> — echo the instance a unit belongs to, or the
# empty string when it is host-wide or unparseable (both of which page).
pkdump_alert_instance_of() {
    local u="${1%.service}"; u="${u%.timer}"; local w
    case "$u" in
        *@*)                  printf '%s' "${u#*@}"; return ;;
        pkdump-litestream-*)  printf '%s' "${u#pkdump-litestream-}"; return ;;
    esac
    for w in $PKDUMP_HOST_WIDE_UNITS; do
        [ "$u" = "$w" ] && return
    done
    case "$u" in
        pkdump-*)             printf '%s' "${u#pkdump-}"; return ;;
    esac
}

pkdump_alert_should_page() {
    local inst; inst="$(pkdump_alert_instance_of "$1")"
    [ -z "$inst" ] && return 0          # host-wide or unknown -> page
    pkdump_units_alerting "$inst"
}

_gate_self_test() {
    local fails=0
    check() { # <unit> <expect page|silent> [alert-set]
        local got exp="$2"
        if PKDUMP_ALERT_INSTANCES="${3:-prod}" pkdump_alert_should_page "$1"; then got=page; else got=silent; fi
        if [ "$got" = "$exp" ]; then printf '  ok    %-46s %s\n' "$1" "$got"
        else printf '  FAIL  %-46s got=%s want=%s\n' "$1" "$got" "$exp"; fails=$((fails+1)); fi
    }
    echo "prod must page:"
    check pkdump-prod.service                      page
    check pkdump-litestream-prod.service           page
    check pkdump-refresh@prod.service              page
    check pkdump-backup-check@prod.service         page
    check pkdump-value-snapshots@prod.service      page
    check pkdump-derive@prod.timer                 page
    check pkdump-prices@prod.service               page
    echo "host-wide guards must page:"
    check pkdump-diskcheck.service                 page
    check pkdump-nessie.service                    page
    echo "unrecognised must page (fail-safe):"
    check some-other-thing.service                 page
    check ''                                       page
    echo "the pages that actually fired must be silent:"
    check pkdump-litestream-drill-9f2c1a.service   silent
    check pkdump-litestream-drill-abc.service      silent
    check pkdump-refresh@drill-9f2c1a.service      silent
    check pkdump-backup-check@ci-1234.service      silent
    check pkdump-test.service                      silent
    check pkdump-litestream-mutant.service         silent
    echo "a second real deployment can opt in:"
    check pkdump-litestream-staging.service        page   "prod staging"
    check pkdump-refresh@staging.service           page   "prod staging"
    check pkdump-litestream-drill-x.service        silent "prod staging"
    [ "$fails" -eq 0 ] && { echo "ALL PASS"; return 0; }
    echo "$fails FAILED"; return 1
}

# Sourced -> define the functions and nothing else. Executed -> act as the gate.
#
# The test MUST be BASH_SOURCE-vs-$0, not "$#" — a sourced script inherits the
# CALLER's positional parameters, so `deploy.sh prod` sourcing this file through
# units-lib.sh would have seen $1="prod", run the gate, and exited the deploy
# script outright. Caught before it shipped; do not "simplify" this back.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    case "${1:-}" in
        --self-test) _gate_self_test; exit $? ;;
        "")          echo "usage: alert-gate.sh <failed-unit-name> | --self-test" >&2; exit 2 ;;
    esac
    pkdump_alert_should_page "$1"; exit $?
fi
