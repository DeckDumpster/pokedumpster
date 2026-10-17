#!/usr/bin/env bash
# Per-worker server instances for the browser tier (sp-lwr1fk).
#
# Playwright runs PKDUMP_VISUAL_WORKERS workers and each worker talks to its OWN
# `--test` instance, seeded from the committed fixture, so no two workers ever
# share a server or a collection database. That is the isolation: nothing about
# the specs has to be proven read-only, and a spec that starts writing cannot
# race a neighbour. The mapping worker -> instance is fixtures.ts; this file
# only stands the extra instances up and tears them down.
#
# Instance 0 is whatever the caller already started (deploy/ci.sh's, or
# run.sh's). Instances 1..N-1 are `<instance>-w<i>`. Under deploy/ci.sh
# PKDUMP_PREBUILT_IMAGE is set, so they re-tag the one image built that run.
#
# Source after deploy/store-lib.sh. It sources tests/lib/wait.sh itself.

# shellcheck source=tests/lib/wait.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/tests/lib/wait.sh"

VISUAL_WORKERS="${PKDUMP_VISUAL_WORKERS:-2}"
VISUAL_EXTRA=()

# visual_workers_start <repo-dir> <instance> <base-url-of-instance-0>
# Sets PKDUMP_BASE_URLS (comma separated, index = worker).
visual_workers_start() {
    local repo="$1" inst="$2" url0="$3" i name port
    PKDUMP_BASE_URLS="$url0"
    for ((i = 1; i < VISUAL_WORKERS; i++)); do
        name="${inst}-w${i}"
        VISUAL_EXTRA+=("$name")
        bash "$repo/deploy/teardown.sh" "$name" --purge >/dev/null 2>&1 || true
        bash "$repo/deploy/setup.sh" "$name" --test || return 1
        systemctl --user start "pkdump-${name}" || return 1
        port=""
        _w_answering() {
            port=$(podman port "systemd-pkdump-${name}" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)
            [ -n "$port" ] && curl -sf -o /dev/null "http://localhost:${port}/"
        }
        wait_until 60 0.25 _w_answering || { echo "ERROR: worker instance ${name} did not answer." >&2; return 1; }
        PKDUMP_BASE_URLS="${PKDUMP_BASE_URLS},http://localhost:${port}"
    done
    export PKDUMP_BASE_URLS
}

visual_workers_stop() {
    local repo="$1" name
    for name in "${VISUAL_EXTRA[@]:-}"; do
        [ -n "$name" ] && bash "$repo/deploy/teardown.sh" "$name" --purge >/dev/null 2>&1 || true
    done
    return 0
}
