#!/usr/bin/env bash
# Shared unit-failure diagnostics helper for deploy scripts. Sourced by
# deploy/ci.sh and deploy/deploy.sh — one definition, not two (db-g6ku).

# Capture the unit journal before a teardown destroys it. Called on any abort
# path where a systemd unit may have failed to start; the caller then exits.
# Both commands are best-effort: a box that lacks the unit or the journal
# should not suppress the evidence it does have.
dump_unit_diagnostics() {
    local unit="$1"
    echo "--- systemctl status ${unit} ---"
    systemctl --user status "$unit" --no-pager 2>&1 || true
    echo "--- journalctl --user -u ${unit} (last 40 lines) ---"
    journalctl --user -u "$unit" --no-pager -n 40 2>&1 || true
}
