#!/usr/bin/env bash
#
# Local CI loop for PokeDumpster — the "super tight" inner dev cycle.
#
# Reproduces what a GitHub `ci.yml` workflow would do, as a plain script so
# it runs identically on a laptop and (eventually) on a CI runner. A future
# GitHub workflow should be a thin wrapper that just calls this file.
#
# Steps:
#   1. Tear down any stale container instance belonging to THIS checkout.
#  1b. Harness gate:  prove the shell harnesses can describe their own failure,
#                     that no harness picks a host port instead of asking the
#                     kernel, that they wait for conditions rather than for the
#                     clock, that the Containerfile's base images are pinned to
#                     a Debian release, and that a Layer 2 page leads with the
#                     line that says what went wrong. Hermetic and sub-second.
#                     See tests/lib/diagnostics_test.sh, tests/lib/ports_test.sh,
#                     tests/lib/wait_test.sh, tests/container/base_images_test.sh
#                     and tests/alarming/journal_summary_test.sh.
#   2. Rust gates:     cargo test, cargo clippy --all-targets, cargo fmt --check.
#   3. Frontend gate:  npm ci && npm test && npm run check && npm run build.
#   4. Container gate: build + start a `--test` instance, wait for the server
#                      to answer on its port, then tear it down.
#   5. Backup gate:    replicate four tenant databases through the SHIPPED
#                      deploy/litestream.yml to a throwaway MinIO and restore a
#                      NON-FIRST one, asserting it comes back as itself. See
#                      tests/litestream/run.sh.
#   6. DR drill:       run deploy/RESTORE.md's procedure with the shipped
#                      scripts — restore one tenant in place while the others
#                      keep exactly their own data, then walk the recovery matrix: after
#                      a RENAME, of a DETACHED tenant, and the load-bearing
#                      negative — restoring the tenant files WITHOUT the registry
#                      must FAIL, shown succeeding-and-anonymous first. See
#                      tests/litestream/drill.sh.
#  6b. Alarming gate:  make every backup-alarming layer FIRE at a local sink
#                      standing in for healthchecks.io + Pushover, and assert on
#                      what it sent. See tests/alarming/run.sh.
#   7. Recreate proof: create a user, remove her, create her again, and prove no
#                      restore of the second one can reach the first one's card
#                      — pd-pm7b, closed executably. See tests/litestream/recreate.sh.
#   8. Upgrade gate:   start the SHIPPED image against a data volume built in
#                      the OLD layout, migrate it onto opaque database ids, roll
#                      that back, and assert it serves the same collection at
#                      every step. See tests/tenants/upgrade.sh.
#   9. Handle gate:    start the SHIPPED image with tenant resolution ON and
#                      assert what it answers to a tenant header: malformed is
#                      400, unknown is 404, and single-tenant mode does not read
#                      the header at all. See tests/tenants/handles.sh.
#  10. Browser gate:   screenshot every route at 1440 and 768 against that
#                      same instance and diff against the committed baselines,
#                      and assert the DOM bounds that a screenshot cannot see —
#                      /collection renders a viewport-sized WINDOW of a
#                      56,635-row result, with no pager anywhere and the far
#                      end a scroll away. See tests/visual/README.md.
#  11. Schema-version gate: start a prod-shaped instance against a deliberately
#                      UNVERSIONED data volume — the shape every database on
#                      disk has, prod's included — and assert every database is
#                      adopted and serves; then assert one from the future is
#                      refused. See tests/schema-version/run.sh.
#  12. Lakehouse gate: stand up Nessie on a durable ROCKSDB store plus a
#                      throwaway MinIO, and drive the shipped PyIceberg job
#                      through write -> read -> commit -> TIME TRAVEL, then
#                      restart the catalog and re-read. Time travel is the only
#                      reason Iceberg + Nessie is here, and a stack that has
#                      lost it looks perfectly healthy. See tests/lake/run.sh.
#  13. Table gate:     build catalog.prices from the raw landing zone on a
#                      podman --internal network — no route to any upstream,
#                      asserted rather than assumed — and compare it row for
#                      row against the shared.sqlite built from the SAME
#                      upstream bytes. See tests/lake/prices.sh. And the same
#                      claim for the whole CATALOG: shared.sqlite derived from
#                      raw/ by the shipped image with egress provably blocked,
#                      row-identical to the online refresh. See
#                      tests/lake/derive.sh.
#  14. Transform gate: value snapshots for EVERY registered tenant, computed
#                      from that table. Byte-identical to what Rust's
#                      snapshot_today produces for the same tenant and date, a
#                      second tenant who never had a snapshot row gets one
#                      (pd-s5yn inverted), and a tenant nobody can write to is
#                      skipped with the run exiting 2 rather than 0. §10 then
#                      drives the SHIPPED deploy/value-snapshots.sh — the file
#                      the nightly timer executes — over the same lake. See
#                      tests/lake/value_snapshots.sh.
#  15. Refresh gate:   the other half of that — a real `pkdump data refresh`,
#                      through the shipped image, over a data directory with
#                      two provisioned tenants in it, must leave every tenant
#                      database byte-identical. The refresh is a SHARED-catalog
#                      job. See tests/refresh/tenant_bytes.sh.
#
# The intents UI harness (tests/ui) is deliberately NOT part of this loop:
# until the replay implementations are generated it needs an ANTHROPIC_API_KEY
# for Vision mode, which makes it slow and non-deterministic. (The browser gate
# in step 10 also drives Playwright, but offline and deterministically — that is
# the difference, not the browser.) Run the intents harness on its own:
#   (cd tests/ui && npx playwright install chromium && npx playwright test)
#
# Exits non-zero on the first failure. Fast and re-runnable.
#
# TIER SELECTION (pd-s2mj). Every step above is a named tier, and a caller may
# hand over the list of paths a change touched; the tiers those paths cannot
# affect are then reported as skipped instead of run. It is opt-in per run and
# it fails closed — no list means EVERY tier, and an unrecognised path means
# every tier. .github/workflows/ci.yml passes a list for PULL REQUESTS ONLY, so
# a developer, a polecat, `workflow_dispatch` and any push-triggered run all
# get the full suite by construction. The rule lives in deploy/ci-select.sh and
# tests/ci/select_test.sh asserts it, including that the names below and the
# names there have not drifted apart.
#
# Usage:
#   bash deploy/ci.sh
#   PKDUMP_CI_INSTANCE=myname bash deploy/ci.sh   # pin the instance name
#   PKDUMP_STORE_ROOT=/some/dir bash deploy/ci.sh # pin the container store
#   PKDUMP_STORE_ROOT= bash deploy/ci.sh          # use Podman's default store
#                                                 # (overrides host store.env)
#   PKDUMP_CI_CHANGED_FILES=list bash deploy/ci.sh  # select tiers from a list
#                                                 # of changed paths
#   PKDUMP_CI_SELECT_ONLY=1 bash deploy/ci.sh     # print the plan, run nothing
#
# Parallel-safe: the container instance is named per-checkout, so several
# polecats can run this concurrently from their own worktrees without tearing
# down each other's containers. Do not reintroduce a fixed instance name.
#
# Disk: nothing this script builds belongs on the disk prod runs from. Point
# PKDUMP_STORE_ROOT at another filesystem and the whole container store — images,
# layers, volumes and Buildah's cache mounts — goes there instead. Which disk
# that is on a given box is host config, not a repo constant, so it is read from
# ~/.config/pkdump/store.env; unconfigured, Podman's default store is used. See
# deploy/store-lib.sh. This script also refuses to start on a nearly-full disk,
# because the failure that produces does not look like a disk problem.
#
# A second store is not free: podman 4.9 lets one store's cleanup take the
# rootless-netns state the other one is holding, after which every container on
# a user-defined network in that store fails — steps 5, 6 and 6b create one, so
# the whole run does. pkdump_store_activate detects and repairs that below; the
# mechanism is in deploy/store-lib.sh (pd-yfev). `deploy/store-teardown.sh`
# removes a store outright, which nothing did before.
#
set -euo pipefail

# systemctl --user / podman need XDG_RUNTIME_DIR; CI runners and
# non-interactive shells often lack it.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Name our own failure (pd-8gjs). A gate that dies without printing why used to
# leave the EXIT trap's "Stopping pkdump-ci-..." as the last line in the log,
# which reads like a clean shutdown and says nothing about the step that died.
# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

# Bounded condition polling, in one place for every harness (pd-86er).
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"

# The instance name has to be unique per checkout. The swarm runs several
# polecats per rig, each from its own worktree, and every one of them runs
# this script; with a shared name, run B's opening teardown destroyed run A's
# container mid-suite (observed 2026-08-03, polecats pipboy and raider). The
# symptoms surfaced as screenshot instability in unrelated routes, so this
# also cost debugging time somewhere else entirely.
#
# Derived from the full worktree path, not its basename: every polecat
# worktree is .../polecats/<name>/pokedumpster, so a basename would collide
# for all of them. Hashing the whole path keeps the name stable per checkout
# — so the stale-cleanup below still finds the previous run's leftovers —
# while making a cross-checkout collision impossible.
INSTANCE="${PKDUMP_CI_INSTANCE:-ci-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
SERVICE_NAME="pkdump-${INSTANCE}"
CONTAINER="systemd-${SERVICE_NAME}"

START_TIME=$(date +%s)

CURRENT_STEP="startup"
step() { CURRENT_STEP="$*"; echo ""; echo "==> $*"; }

# --- 0. Tier selection -------------------------------------------------------
#
# EVERY TIER RUNS unless a caller hands over an explicit list of changed paths.
# A developer, a polecat, `workflow_dispatch`, and any push-triggered run
# therefore get the full suite by construction — skipping a tier takes an
# affirmative act by the caller, and the act is printed in the log below.
#
#   PKDUMP_CI_CHANGED_FILES=<file>   one changed path per line; the tiers those
#                                    paths require are run and the rest are
#                                    reported as skipped. .github/workflows/
#                                    ci.yml sets this for PULL REQUESTS ONLY.
#   PKDUMP_CI_SELECT_ONLY=1          print the plan and exit 0, touching
#                                    nothing. This is how tests/ci/select_test.sh
#                                    asserts on the real script rather than on a
#                                    copy of its rules.
#
# The rule itself, and why it fails closed, is deploy/ci-select.sh. Nothing
# about WHICH tiers exist lives in two places: the names below are checked
# against that file's canonical list by tests/ci/select_test.sh.
#
# shellcheck source=deploy/ci-select.sh
. "$SCRIPT_DIR/ci-select.sh"

if [ -n "${PKDUMP_CI_CHANGED_FILES:-}" ]; then
    # Named but unreadable is a refusal, not a fallback. A caller that meant to
    # select and instead typo'd a path would otherwise get a silent full run,
    # which is the safe direction but hides a broken workflow indefinitely.
    [ -f "$PKDUMP_CI_CHANGED_FILES" ] || {
        echo "ERROR: PKDUMP_CI_CHANGED_FILES=${PKDUMP_CI_CHANGED_FILES} is not a file." >&2
        exit 1
    }
    PKDUMP_CI_SELECTED="$(pkdump_ci_select_tiers < "$PKDUMP_CI_CHANGED_FILES" | tr '\n' ' ')"
    # `|| true` because an EMPTY list must reach the selector, which reads it as
    # "we do not know what changed" and runs everything. Without it grep's exit 1
    # on a zero count took the whole script down under `set -e` — a full-suite
    # answer turned into a dead run, found by tests/ci/select_test.sh §4.
    SELECTION_SOURCE="$(grep -c '' < "$PKDUMP_CI_CHANGED_FILES" || true) changed path(s) in ${PKDUMP_CI_CHANGED_FILES}"
else
    PKDUMP_CI_SELECTED="$(pkdump_ci_all_tiers | tr '\n' ' ')"
    SELECTION_SOURCE="no changed-path list given — running everything"
fi

# Guard for one tier's steps. Exit status 2 from the library means the name is
# not a tier at all — a typo, which must be fatal here rather than reading as
# "not selected" and skipping a gate on every run from now on.
tier() {
    local rc=0
    pkdump_ci_tier_selected "$1" || rc=$?
    case "$rc" in
        0) return 0 ;;
        2)
            echo "ERROR: ci.sh guards on '$1', which is not one of: ${PKDUMP_CI_ALL_TIERS}" >&2
            exit 1
            ;;
    esac
    echo ""
    echo "==> (skipped: tier '$1' is not required by these changes)"
    return 1
}

step "CI tiers"
echo "    selection: ${SELECTION_SOURCE}"
for _t in $PKDUMP_CI_ALL_TIERS; do
    if pkdump_ci_tier_selected "$_t"; then echo "    RUN   $_t"; else echo "    skip  $_t"; fi
done

if [ -n "${PKDUMP_CI_SELECT_ONLY:-}" ]; then
    echo ""
    echo "==> PKDUMP_CI_SELECT_ONLY set — plan printed, nothing run."
    exit 0
fi

# --- 0b. Container store + disk floor ----------------------------------------

# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
# Unset means "not answered here, ask the host" — ~/.config/pkdump/store.env.
# Set, including set to empty, means the caller decided and is left alone.
# Answered nowhere means Podman's default store, which is also prod's.
pkdump_store_load_config
pkdump_store_activate

step "Disk floor check"
# Before the build, not after it dies: at 697M free a cargo link failed with
# `ld terminated with signal 7 [Bus error]`, which reads as a toolchain bug and
# cost real time to diagnose (pd-fite). Both disks matter — $HOME still holds
# the toolchain caches and the default store even when the container store moves.
bash "$SCRIPT_DIR/diskcheck.sh" --floor "$HOME" "${PKDUMP_STORE_ROOT:-$HOME}"

DF_BEFORE="$(df -h "$HOME" | tail -n1)"

# --- 1. Clean up any stale ci instance --------------------------------------

step "Cleaning up stale '${INSTANCE}' instance..."
bash "$SCRIPT_DIR/teardown.sh" "$INSTANCE" --purge 2>/dev/null || true
# `until=24h` matters for the same reason the instance name does: a bare
# `image prune -f` is global, so it will happily delete a dangling layer that
# a concurrently-running polecat's build is still using. Age-filtering keeps
# the housekeeping without reaching into another run's build.
podman image prune -f --filter "until=24h" >/dev/null 2>&1 || true

# Tear the ci instance down again on exit, whatever happens.
cleanup() {
    local rc=$?
    bash "$SCRIPT_DIR/teardown.sh" "$INSTANCE" --purge 2>/dev/null || true
    # After the teardown noise, so "which step failed, and with what status" is
    # the last thing in the log rather than something to infer from where it
    # stops.
    [[ $rc -eq 0 ]] || diag "!! CI FAILED during step: ${CURRENT_STEP} (status ${rc})"
}
trap cleanup EXIT

# --- 1b. Lint tier ------------------------------------------------------------
# Hermetic and sub-second: proves the diagnostics above actually report a
# silenced failure, before spending ten minutes on gates that rely on them.
#
# THIS TIER ALWAYS RUNS — it is what a docs-only PR gets, and what keeps such a
# run capable of going red. Everything in it reads the tree and compiles
# nothing, which is what makes it affordable unconditionally.

if tier lint; then
    step "Harness diagnostics self-test (tests/lib/diagnostics_test.sh)"
    bash "$REPO_DIR/tests/lib/diagnostics_test.sh"

    # Same tier, same reason: a host port picked from a band instead of taken
    # from the kernel has been found and fixed in five files now, and each time
    # the fix reached one of them. §6-§8 of this gate assert on the tree, so a
    # sixth relapse fails here in a second rather than forty minutes in as
    # "address already in use". See tests/lib/ports.sh.
    step "Harness host-port self-test (tests/lib/ports_test.sh)"
    bash "$REPO_DIR/tests/lib/ports_test.sh"

    # Same tier, same shape of ratchet: the harnesses used to spend about two
    # minutes a run on fixed sleeps that asserted nothing except how fast this
    # box is. §1-§4 prove the replacement asks before it sleeps and — the one
    # that matters — that it GIVES UP, since an unbounded poll would hang CI
    # instead of failing it. §5-§6 assert on the tree, so a `sleep 8` cannot
    # come back one reasonable-looking line at a time. See tests/lib/wait.sh.
    step "Harness condition-polling self-test (tests/lib/wait_test.sh)"
    bash "$REPO_DIR/tests/lib/wait_test.sh"

    # Same tier, and it guards every gate below that builds the image: the
    # builder stage rode a moving tag, upstream retagged it from bookworm to
    # trixie, and the image started shipping a binary that cannot exec against
    # the bookworm runtime (pd-pejn). Reading the Containerfile costs a second;
    # finding out in step 4 costs the build first. See
    # tests/container/base_images_test.sh.
    step "Container base-image pins (tests/container/base_images_test.sh)"
    bash "$REPO_DIR/tests/container/base_images_test.sh"

    # Path selection is itself a gate now, and one whose failure mode is a tier
    # quietly not running. Hermetic and sub-second, so it sits here rather than
    # behind the compile. See tests/ci/select_test.sh.
    step "CI tier selection fails closed (tests/ci/select_test.sh)"
    bash "$REPO_DIR/tests/ci/select_test.sh"

    # Same tier again, and the same shape of bug: a layer that fires and says
    # nothing. §6 of the alarming gate proves Layer 2 PUSHES; this proves what
    # it pushes is readable — the causal line first, no OCI metadata, no
    # systemd boilerplate — against journal tails captured from the real units
    # (pd-pwk8).
    step "Alert page content (tests/alarming/journal_summary_test.sh)"
    bash "$REPO_DIR/tests/alarming/journal_summary_test.sh"

    # A formatter check, not a compile — `cargo fmt` parses and never builds,
    # so it costs a second and belongs with the lint tier rather than behind
    # `cargo test`.
    step "cargo fmt --check"
    ( cd "$REPO_DIR" && cargo fmt --check )
fi

# --- 2. Rust gates ----------------------------------------------------------

if tier rust; then
    step "cargo test"
    ( cd "$REPO_DIR" && cargo test )

    # --profile test so clippy REUSES what `cargo test` just built. Without it
    # clippy defaults to the dev profile -- a different artifact set -- and
    # recompiles all 352 crates for the lint alone (2m11s measured 2026-08-12).
    step "cargo clippy --all-targets"
    ( cd "$REPO_DIR" && cargo clippy --all-targets --profile test -- -D warnings )
fi

# --- 2b. Deploy-script gates ------------------------------------------------
# The low-disk guard, the store-root resolution and the unit-file install are
# shell, so they get a shell test — including one that shows the guard actually
# firing, and one that drives deploy.sh over stale installed units and asserts
# every one comes back matching the shipped template (pd-2t6u).

if tier deploy; then
    step "Deploy scripts: unit install + store resolution + low-disk guard + scheduling"
    bash "$REPO_DIR/tests/deploy/run.sh"
fi

# --- 3. Frontend gate -------------------------------------------------------

if tier frontend; then
    step "Frontend: npm ci && npm test && npm run check && npm run build"
    (
        cd "$REPO_DIR/frontend"
        npm ci
        # Design-token gates: WCAG AA contrast for every declared pairing, the
        # reference/semantic layer split, and the two ratchets — raw colour and
        # raw dimension — which fail on any INCREASE in values chosen outside
        # the token layer. Node's built-in runner, no extra deps.
        npm test
        npm run check
        npm run build
    )
fi

# --- 4. Container gate ------------------------------------------------------
# Selected whenever the browser tier is — the screenshots are taken against
# this instance, and the SvelteKit build they are screenshotting is baked into
# its image. ci-select.sh refuses a selection that has one without the other.

PORT=""
if tier container; then
    step "Building and starting '--test' container instance..."
    bash "$SCRIPT_DIR/setup.sh" "$INSTANCE" --test
    systemctl --user start "$SERVICE_NAME"

    step "Waiting for the server to answer..."
    # Asked four times a second rather than once every two: the check is a
    # `podman port` and a loopback curl, both cheap, and the two-second interval
    # was most of the time this step took on a server that was already up
    # (pd-86er). The 60-second bound is unchanged.
    server_answering() {
        PORT=$(podman port "$CONTAINER" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)
        if [ -n "$PORT" ] && curl -sf -o /dev/null "http://localhost:${PORT}/"; then
            echo "    Server is up on port ${PORT}."
            return 0
        fi
        PORT=""
        return 1
    }
    wait_until 60 0.25 server_answering || true
    if [ -z "$PORT" ]; then
        echo "ERROR: server failed to start within timeout."
        journalctl --user -u "$SERVICE_NAME" --no-pager -n 40 2>/dev/null || true
        exit 1
    fi
fi

# --- 5. Backup gate ---------------------------------------------------------
# Litestream replicates every tenant from one sidecar and a restore hands back
# the right tenant's collection. Self-contained (own network, own MinIO, own
# temp dir) — it does not touch the instance started above, nor any real bucket.

if tier litestream; then
    step "Litestream multi-tenant replication + restore"
    bash "$REPO_DIR/tests/litestream/run.sh"

    # --- 6. DR drill --------------------------------------------------------
    # The operator procedure in deploy/RESTORE.md, executed with the shipped
    # scripts against a real Quadlet sidecar: restore one tenant in place, in
    # time, and onto a bare volume, and assert the other tenants are
    # byte-identical every time. Its own instance name / volume / MinIO /
    # secret — it touches nothing else.

    step "Multi-tenant DR drill (deploy/RESTORE.md, executed)"
    bash "$REPO_DIR/tests/litestream/drill.sh"

    # --- 6b. Alarming gate --------------------------------------------------
    # A backup that is not alarmed is a backup nobody knows is broken — which
    # is the state this project was actually in for months. Every layer is made
    # to fire at a local recorder and asserted on what it sent. Its own
    # instance, its own MinIO, its own unit-name prefix, both endpoints on
    # 127.0.0.1 — it touches no pkdump-*@prod unit and contacts no external
    # service.

    step "Backup alarming: every layer fires (tests/alarming/run.sh)"
    bash "$REPO_DIR/tests/alarming/run.sh"

    # --- 7. Recreated-handle proof ------------------------------------------
    # pd-pm7b as an executable statement rather than an argument: a handle is
    # created, removed and created again through the real `pkdump tenant`
    # commands, and no restore of the second user — latest or point-in-time
    # inside the retention window — can produce the first user's card. Its own
    # MinIO, its own $PKDUMP_HOME, its own prefix; it touches nothing else here.

    step "Recreated handle cannot inherit a replica (pd-pm7b)"
    bash "$REPO_DIR/tests/litestream/recreate.sh"
fi

# --- 8. Upgrade-path gate ---------------------------------------------------
# Fresh instances are not the upgrade path. deploy/setup.sh --test creates its
# volume already in the current layout, which is exactly why two alignment beads
# both verified single-tenant startup and prod still went down on the first
# automated deploy of the last migration (pd-uoph). This starts the shipped image
# against a volume built in the OLD shape. Its own image tag, container, port and
# temp dir — it does not touch the instance started above.

if tier tenants; then
    step "Upgrade path: old-layout volume -> migrate -> rollback (pd-hqee)"
    bash "$REPO_DIR/tests/tenants/upgrade.sh"

    # --- 9. Tenant-header gate ----------------------------------------------
    # What the shipped image answers to a tenant header, over real HTTP:
    # malformed is a 400 naming the rule, well-formed-but-unknown is a 404, and
    # single-tenant mode does not read the header at all. The distinction is a
    # status code, so it has to be asserted on the wire — a 400 flattened into
    # a 404 by the middleware would satisfy every unit test in the crate. Its
    # own image tag, container, port and temp dir — it does not touch the
    # instance started above.

    step "Tenant header: malformed 400 vs unknown 404 (pd-4g7c)"
    bash "$REPO_DIR/tests/tenants/handles.sh"
fi

# --- 10. Browser gate --------------------------------------------------------
# Runs against the container started above rather than standing up a second
# one. A pixel diff fails CI; approving it is explicit — tests/visual/README.md.
#
# Selected for ANY change under frontend/, not just route files: a token, a
# shared component or a rule in app.css repaints every route at once, and
# pd-tf4h is what a selector that guessed otherwise already cost. See
# deploy/ci-select.sh.

if tier browser; then
    step "Browser: every route screenshotted, and /collection's DOM bounded"
    PKDUMP_BASE_URL="http://localhost:${PORT}" bash "$REPO_DIR/tests/visual/playwright.sh"
fi

# --- 11. Schema-version gate ------------------------------------------------
# The upgrade path, not the fresh install: a prod-shaped container started
# against a volume the PRE-GATE binary would have left behind. Its own instance
# name, volume and port — it does not touch the instance started above.

if tier schema; then
    step "Schema version: an unversioned volume is adopted, a future one is refused"
    bash "$REPO_DIR/tests/schema-version/run.sh"
fi

# --- 12. Lakehouse gate -----------------------------------------------------
# The offline lakehouse substrate (pd-fzeb), end to end: the PyIceberg job image
# builds, Nessie serves an Iceberg REST catalog over a durable version store,
# and a table can be written, committed twice and read back AS OF the first
# commit. Its own network, MinIO, Nessie, image tag and temp dir — it touches no
# pkdump-* unit, no real bucket, and no tenant database.

if tier lake; then
    step "Lakehouse: PyIceberg + Nessie write/read/time-travel round trip"
    bash "$REPO_DIR/tests/lake/run.sh"

    # --- 13. catalog.prices from raw ----------------------------------------
    # The claim the landing zone exists to make good on, made mechanically: the
    # build job runs on an --internal podman network, so there is no upstream to
    # call even if it wanted one, and its output is compared row for row against
    # the shared.sqlite built from the same upstream bytes.

    step "Lakehouse: catalog.prices built from raw/ alone, with no network"
    bash "$REPO_DIR/tests/lake/prices.sh"

    # --- 14. The transform tier ---------------------------------------------
    # Value snapshots for EVERY registered tenant, computed from the lake. Two
    # claims at once: the rows are byte-identical to what value_history::
    # snapshot_today produces for the same tenant and date, and a second tenant
    # who has never had a snapshot row gets one — which is pd-s5yn inverted
    # into a test. A tenant whose database is missing or locked is skipped and
    # the run exits 2.

    step "Lakehouse: per-tenant value snapshots, for every tenant"
    bash "$REPO_DIR/tests/lake/value_snapshots.sh"

    # --- 14b. shared.sqlite from raw ----------------------------------------
    # The same claim as §13, one level up: the whole CATALOG, not one table.
    # `pkdump-lake-derive` runs in the SHIPPED image on an --internal podman
    # network — §2 of the gate proves egress is gone by trying to reach
    # 1.1.1.1 from that image — and its output is compared row by row against
    # the shared.sqlite the online refresh built from the same upstream bytes.
    #
    # It lives in this tier rather than `container` because it is a lakehouse
    # claim: a frontend change cannot affect whether a catalog rebuilds from
    # raw/, and the container tier is what a frontend change selects.

    step "Lakehouse: shared.sqlite derived from raw/ alone, with no network"
    bash "$REPO_DIR/tests/lake/derive.sh"
fi

# --- 15. The refresh writes no tenant bytes ---------------------------------
# The transform tier above is only half the fix for pd-s5yn; this is the other
# half. A real `pkdump data refresh` runs through the shipped image over a data
# directory holding two provisioned tenants — one of them the handle
# $PKDUMP_USER defaults to, which is the exact database the deleted step 7 used
# to write — and every tenant database has to come out byte-identical. Its
# upstream is a local fixture that publishes nothing, so the run completes in
# seconds and depends on nobody's uptime; §5 of the gate asserts it really ran
# the derivation phases rather than exiting early.

if tier refresh; then
    step "Refresh: the catalog refresh writes zero tenant bytes"
    bash "$REPO_DIR/tests/refresh/tenant_bytes.sh"
fi

# The intents UI harness is intentionally not run here — see the header.
echo ""
echo "    (intents UI harness not run — see the note in this script's header)"

# --- Done -------------------------------------------------------------------

ELAPSED=$(( $(date +%s) - START_TIME ))
echo ""
# The whole point of the alternate store: a CI run must not eat the disk prod
# runs from. Printed every run so a regression shows up as a number, not as a
# mystery bus error three weeks later (pd-fite).
echo "==> Disk holding \$HOME (prod's):"
echo "    before: ${DF_BEFORE}"
echo "    after:  $(df -h "$HOME" | tail -n1)"
echo ""
echo "==> CI passed in ${ELAPSED}s."
