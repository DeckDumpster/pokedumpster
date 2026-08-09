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
#                      stay byte-identical, then walk the recovery matrix: after
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
#  10. Visual gate:    screenshot every route at 1440 and 768 against that
#                      same instance and diff against the committed baselines.
#                      See tests/visual/README.md for the approval workflow.
#  11. Schema-version gate: start a prod-shaped instance against a deliberately
#                      UNVERSIONED data volume — the shape every database on
#                      disk has, prod's included — and assert every database is
#                      adopted and serves; then assert one from the future is
#                      refused. See tests/schema-version/run.sh.
#
# The intents UI harness (tests/ui) is deliberately NOT part of this loop:
# until the replay implementations are generated it needs an ANTHROPIC_API_KEY
# for Vision mode, which makes it slow and non-deterministic. (The visual gate
# in step 10 also drives Playwright, but offline and deterministically — that is
# the difference, not the browser.) Run the intents harness on its own:
#   (cd tests/ui && npx playwright install chromium && npx playwright test)
#
# Exits non-zero on the first failure. Fast and re-runnable.
#
# Usage:
#   bash deploy/ci.sh
#   PKDUMP_CI_INSTANCE=myname bash deploy/ci.sh   # pin the instance name
#
# Parallel-safe: the container instance is named per-checkout, so several
# polecats can run this concurrently from their own worktrees without tearing
# down each other's containers. Do not reintroduce a fixed instance name.
#
set -euo pipefail

# systemctl --user / podman need XDG_RUNTIME_DIR; CI runners and
# non-interactive shells often lack it.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

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

step() { echo ""; echo "==> $*"; }

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
    bash "$SCRIPT_DIR/teardown.sh" "$INSTANCE" --purge 2>/dev/null || true
}
trap cleanup EXIT

# --- 2. Rust gates ----------------------------------------------------------

step "cargo test"
( cd "$REPO_DIR" && cargo test )

step "cargo clippy --all-targets"
( cd "$REPO_DIR" && cargo clippy --all-targets -- -D warnings )

step "cargo fmt --check"
( cd "$REPO_DIR" && cargo fmt --check )

# --- 3. Frontend gate -------------------------------------------------------

step "Frontend: npm ci && npm test && npm run check && npm run build"
(
    cd "$REPO_DIR/frontend"
    npm ci
    # Design-token gates: WCAG AA contrast for every declared pairing, the
    # reference/semantic layer split, and the two ratchets — raw colour and raw
    # dimension — which fail on any INCREASE in values chosen outside the token
    # layer. Node's built-in runner, no extra deps.
    npm test
    npm run check
    npm run build
)

# --- 4. Container gate ------------------------------------------------------

step "Building and starting '--test' container instance..."
bash "$SCRIPT_DIR/setup.sh" "$INSTANCE" --test
systemctl --user start "$SERVICE_NAME"

step "Waiting for the server to answer..."
PORT=""
for _ in $(seq 1 30); do
    PORT=$(podman port "$CONTAINER" 8080/tcp 2>/dev/null | head -1 | cut -d: -f2 || true)
    if [ -n "$PORT" ] && curl -sf -o /dev/null "http://localhost:${PORT}/"; then
        echo "    Server is up on port ${PORT}."
        break
    fi
    PORT=""
    sleep 2
done
if [ -z "$PORT" ]; then
    echo "ERROR: server failed to start within timeout."
    journalctl --user -u "$SERVICE_NAME" --no-pager -n 40 2>/dev/null || true
    exit 1
fi

# --- 5. Backup gate ---------------------------------------------------------
# Litestream replicates every tenant from one sidecar and a restore hands back
# the right tenant's collection. Self-contained (own network, own MinIO, own
# temp dir) — it does not touch the instance started above, nor any real bucket.

step "Litestream multi-tenant replication + restore"
bash "$REPO_DIR/tests/litestream/run.sh"

# --- 6. DR drill ------------------------------------------------------------
# The operator procedure in deploy/RESTORE.md, executed with the shipped scripts
# against a real Quadlet sidecar: restore one tenant in place, in time, and onto
# a bare volume, and assert the other tenants are byte-identical every time.
# Its own instance name / volume / MinIO / secret — it touches nothing else.

step "Multi-tenant DR drill (deploy/RESTORE.md, executed)"
bash "$REPO_DIR/tests/litestream/drill.sh"

# --- 6b. Alarming gate ------------------------------------------------------
# A backup that is not alarmed is a backup nobody knows is broken — which is the
# state this project was actually in for months. Every layer is made to fire at a
# local recorder and asserted on what it sent. Its own instance, its own MinIO,
# its own unit-name prefix, both endpoints on 127.0.0.1 — it touches no
# pkdump-*@prod unit and contacts no external service.

step "Backup alarming: every layer fires (tests/alarming/run.sh)"
bash "$REPO_DIR/tests/alarming/run.sh"

# --- 7. Recreated-handle proof ----------------------------------------------
# pd-pm7b as an executable statement rather than an argument: a handle is
# created, removed and created again through the real `pkdump tenant` commands,
# and no restore of the second user — latest or point-in-time inside the
# retention window — can produce the first user's card. Its own MinIO, its own
# $PKDUMP_HOME, its own prefix; it touches nothing else here.

step "Recreated handle cannot inherit a replica (pd-pm7b)"
bash "$REPO_DIR/tests/litestream/recreate.sh"

# --- 8. Upgrade-path gate ---------------------------------------------------
# Fresh instances are not the upgrade path. deploy/setup.sh --test creates its
# volume already in the current layout, which is exactly why two alignment beads
# both verified single-tenant startup and prod still went down on the first
# automated deploy of the last migration (pd-uoph). This starts the shipped image
# against a volume built in the OLD shape. Its own image tag, container, port and
# temp dir — it does not touch the instance started above.

step "Upgrade path: old-layout volume -> migrate -> rollback (pd-hqee)"
bash "$REPO_DIR/tests/tenants/upgrade.sh"

# --- 9. Tenant-header gate --------------------------------------------------
# What the shipped image answers to a tenant header, over real HTTP: malformed
# is a 400 naming the rule, well-formed-but-unknown is a 404, and single-tenant
# mode does not read the header at all. The distinction is a status code, so it
# has to be asserted on the wire — a 400 flattened into a 404 by the middleware
# would satisfy every unit test in the crate. Its own image tag, container,
# port and temp dir — it does not touch the instance started above.

step "Tenant header: malformed 400 vs unknown 404 (pd-4g7c)"
bash "$REPO_DIR/tests/tenants/handles.sh"

# --- 10. Visual-regression gate ---------------------------------------------
# Runs against the container started above rather than standing up a second
# one. A pixel diff fails CI; approving it is explicit — tests/visual/README.md.

step "Visual regression: every route, two viewports"
PKDUMP_BASE_URL="http://localhost:${PORT}" bash "$REPO_DIR/tests/visual/playwright.sh"

# --- 11. Schema-version gate ------------------------------------------------
# The upgrade path, not the fresh install: a prod-shaped container started
# against a volume the PRE-GATE binary would have left behind. Its own instance
# name, volume and port — it does not touch the instance started above.

step "Schema version: an unversioned volume is adopted, a future one is refused"
bash "$REPO_DIR/tests/schema-version/run.sh"

# The intents UI harness is intentionally not run here — see the header.
echo ""
echo "    (intents UI harness not run — see the note in this script's header)"

# --- Done -------------------------------------------------------------------

ELAPSED=$(( $(date +%s) - START_TIME ))
echo ""
echo "==> CI passed in ${ELAPSED}s."
