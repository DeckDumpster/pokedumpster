#!/usr/bin/env bash
#
# Installing this checkout's systemd/Quadlet unit files for an instance (pd-2t6u).
#
# The unit files under deploy/ are TEMPLATES: {{INSTANCE}}, {{PORT}} and
# {{REPO_DIR}} are sed-substituted into $HOME/.config/{containers/systemd,
# systemd/user} at install time. So "the unit prod runs" is a copy, and a copy
# only tracks the repo if something rewrites it.
#
# Nothing did. deploy/setup.sh wrote the units; deploy/deploy.sh — the command
# an operator actually runs to ship a change — rebuilt the image and restarted,
# and reinstalled the units ONLY when the instance did not exist yet. Every
# unit-file change since an instance was first set up therefore stayed in the
# repo. Observed on the prod box 2026-08-09: pkdump-litestream-prod.container
# was still the pre-multi-tenant template, missing the Jun 2026
# `OnFailure=pkdump-alert@%n.service` — so the sidecar that silently stopped
# replicating had no failure alerting wired, while the repo said it did.
#
# Hence one function, used by both scripts, so "install the units" cannot mean
# two different sets of files. It is a CONVERGE step: every unit is re-rendered
# from the template every time, and the ones whose content actually changed are
# named on stdout. A deploy that silently rewrites nothing and a deploy that
# silently rewrites everything look the same otherwise, and this project has
# already paid for one of those.
#
# What is NOT here, deliberately: the config scaffolding (alerts.env, store.env,
# litestream.env). Those are an operator's files with an operator's secrets in
# them, created once and never clobbered — creating them is setting an instance
# UP, not shipping code to it. setup.sh keeps that job.
#
# The caller must have sourced deploy/store-lib.sh and activated the store it
# wants stamped into the generated Quadlet units — the flags are read from the
# environment pkdump_store_activate exports, not re-derived here.
#
# Sourced, not executed.

# Unit basenames written by the last pkdump_units_install call whose content
# changed. Empty means the installed units already matched this checkout.
PKDUMP_UNITS_CHANGED=()

# The alerting predicate + the OnFailure gate live in alert-gate.sh so the alert
# unit and the renderer cannot disagree about which instances may page (pd-n0lf).
# shellcheck source=deploy/alert-gate.sh
. "$(dirname "${BASH_SOURCE[0]}")/alert-gate.sh"

# _pkdump_units_render <dest> <stamp|nostamp> <template> [sed -e args...]
#
# Render a template to <dest>, but only replace the file when the result
# differs — so an unchanged unit keeps its mtime and the changed ones are the
# ones reported. Written via a dot-prefixed sibling and moved into place: a
# torn write in the Quadlet directory is a unit systemd cannot parse, and the
# dot prefix keeps a leftover out of the *.container glob.
_pkdump_units_render() {
    local dest="$1" stamp="$2" template="$3"
    shift 3
    local dir base tmp
    dir="$(dirname "$dest")"
    base="$(basename "$dest")"
    tmp="${dir}/.${base}.new.$$"

    sed "$@" "$template" > "$tmp"

    if [ "$stamp" = stamp ]; then
        # systemd does not inherit our PATH, so the unit carries the store flags
        # itself. A no-op without an active alternate store — which is prod.
        pkdump_store_stamp_unit "$tmp"
    fi

    if [ -f "$dest" ] && cmp -s "$tmp" "$dest"; then
        rm -f "$tmp"
    else
        mv "$tmp" "$dest"
        PKDUMP_UNITS_CHANGED+=("$base")
    fi
}

# pkdump_units_install <instance> [port]
#
# (Re)install every unit file this checkout ships for <instance>. Idempotent.
# The caller reloads systemd afterwards — this function only writes files.
#
# port: the host port to publish, or 0 / omitted for "decide". Deciding means
# the port the instance ALREADY publishes if it has one, and otherwise ":8080"
# so Podman picks a free one. Refreshing the units must never move a running
# instance's address: without that, `deploy.sh prod` would rewrite prod's
# Quadlet with a random host port and take prod off 8090 — an outage caused by
# shipping a unit-file fix.
pkdump_units_install() {
    local _alert_sed=()
    pkdump_units_alerting "$1" || _alert_sed=(-e '/^OnFailure=/d')
    local instance="$1" port="${2:-0}"
    local repo_dir quadlet_dir systemd_user_dir service_name
    repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    quadlet_dir="${HOME}/.config/containers/systemd"
    systemd_user_dir="${HOME}/.config/systemd/user"
    service_name="pkdump-${instance}"

    PKDUMP_UNITS_CHANGED=()
    mkdir -p "$quadlet_dir" "$systemd_user_dir"

    local existing_port port_mapping
    if [ "$port" = "0" ] && [ -f "${quadlet_dir}/${service_name}.container" ]; then
        existing_port="$(sed -n 's|^PublishPort=\([0-9]\{1,\}\):8080$|\1|p' \
            "${quadlet_dir}/${service_name}.container" | head -1)"
        if [ -n "$existing_port" ]; then
            port="$existing_port"
            echo "    Keeping the port '${instance}' already publishes: ${port}"
        fi
    fi
    # PORT=0 -> ":8080" lets Podman pick a free host port.
    if [ "$port" = "0" ]; then
        port_mapping=":8080"
    else
        port_mapping="${port}:8080"
    fi

    # --- The app's Quadlet unit ---------------------------------------------
    _pkdump_units_render "${quadlet_dir}/${service_name}.container" stamp \
        "$repo_dir/deploy/pkdump.container" \
        -e "s|{{INSTANCE}}|${instance}|g" \
        -e "s|{{PORT}}:8080|${port_mapping}|g" \
        "${_alert_sed[@]}"

    # --- Per-instance timer units -------------------------------------------
    # %i-templated units installed under a concrete instance name so several
    # instances can run side by side. Enable them after setup if desired.
    # {{REPO_DIR}} resolves ExecStart against THIS checkout (single-clone
    # deployment — the clone is not named per-instance).
    local ext
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-refresh@.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-refresh.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done

    # --- The offline catalog derive (pd-1uem) -------------------------------
    # The DERIVING half of the nightly pair; pkdump-refresh@ above is the
    # landing half. Installed for every instance and enabled for none — a box
    # whose catalog is still built by the online refresh needs no second
    # rebuild, and the unit's ConditionPathExists on lake.env keeps an enabled
    # timer inert on a box that has no landing zone at all.
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-derive@.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-derive.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done

    # --- The nightly catalog.prices build (pd-up36) -------------------------
    # The middle of the chain: the landing unit above puts the day in the
    # bucket, this turns it into a table, the transform below reads that table.
    # Installed for every instance and enabled for none, like the two around it
    # — the lakehouse itself is opt-in (deploy/setup-lake.sh), and the unit's
    # ConditionPathExists on lake.env keeps an enabled timer inert on a box that
    # has no lake.
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-prices@.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-prices.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done

    # --- The transform tier's nightly run (pd-8m5c) -------------------------
    # Per-tenant value snapshots from the lake, ordered after the refresh above.
    # Installed for every instance and enabled for none — the lakehouse itself is
    # opt-in (deploy/setup-lake.sh), and the unit's ConditionPathExists on
    # lake.env keeps an enabled timer inert on a box that has no lake.
    #
    # Installed HERE rather than by setup-lake.sh, with the rest of them, for the
    # reason this file exists: a unit template only reaches an instance if
    # something re-renders it on every deploy, and setup-lake.sh is run once.
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-value-snapshots@.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-value-snapshots.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done

    # --- Backup-failure alarming units (pokedumpster-ivq) -------------------
    # Layer 1: backup-freshness dead-man's switch (per-instance @ template).
    # Layer 2: OnFailure -> Pushover journal-tail push (instance-by-failed-unit).
    # Layer 4: host-wide low-disk check (not per-instance).
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-backup-check@.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-backup-check.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done
    _pkdump_units_render "${systemd_user_dir}/pkdump-alert@.service" nostamp \
        "$repo_dir/deploy/pkdump-alert@.service" \
        -e "s|{{REPO_DIR}}|${repo_dir}|g"
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-diskcheck.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-diskcheck.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done

    # Layer 0: is the site serving at all? Numbered below layer 1 because it is
    # the question the other layers assume the answer to. Backup freshness only
    # tells you about the box INDIRECTLY, on a 6h period with a 3h grace; on
    # 2026-08-16 the site was hard down and nothing paged, because the only
    # liveness signal was that proxy and it had already been failing for four
    # days over an unrelated bug.
    for ext in service timer; do
        _pkdump_units_render "${systemd_user_dir}/pkdump-heartbeat@.${ext}" nostamp \
            "$repo_dir/deploy/pkdump-heartbeat.${ext}" \
            -e "s|{{REPO_DIR}}|${repo_dir}|g"
    done

    # --- The Litestream backup sidecar (pokedumpster-8ch.3) -----------------
    _pkdump_units_render "${quadlet_dir}/pkdump-litestream-${instance}.container" stamp \
        "$repo_dir/deploy/pkdump-litestream.container" \
        -e "s|{{INSTANCE}}|${instance}|g" \
        -e "s|{{REPO_DIR}}|${repo_dir}|g" \
        "${_alert_sed[@]}"
}

# pkdump_units_report — say what the last install actually changed.
#
# The point of naming them: unit drift is invisible by construction (the
# installed copy and the template are different files in different trees), so
# the deploy that finally corrects months of it should say so out loud rather
# than look exactly like the deploy that corrected nothing.
pkdump_units_report() {
    if [ ${#PKDUMP_UNITS_CHANGED[@]} -eq 0 ]; then
        echo "    Unit files already match this checkout."
    else
        echo "    Unit files updated from this checkout (${#PKDUMP_UNITS_CHANGED[@]}):"
        printf '      %s\n' "${PKDUMP_UNITS_CHANGED[@]}"
    fi
}
