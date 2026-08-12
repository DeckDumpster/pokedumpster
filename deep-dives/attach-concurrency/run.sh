#!/usr/bin/env bash
#
# Measurement runner: N tenants reading the shared catalog through ATTACH.
#
# Bead pd-jgd4, epic pd-gckl. The harness itself is a Rust example
# (crates/pkdump-db/examples/attach_concurrency.rs) because the workload has
# to be the REAL read path — pkdump_db::binder::get_binder_page — rather than
# a SQL transcription of it that could drift from the code being defended.
# This script only picks sizes, checks there is room for them, and runs it.
#
# Prod-safe. It touches no pkdump-* unit, no pkdump-prod-data volume, no live
# collection.sqlite and no $PKDUMP_HOME: everything happens inside WORK, and
# the catalog it reads is one the harness generates.
#
# Usage:
#   deep-dives/attach-concurrency/run.sh                 # ~4 min from cold
#   SECONDS_PER=15 LEVELS=1,2,4,8,16,32 ...run.sh        # longer / wider
#   WORK=/mnt/big/pd-attach KEEP=1 ...run.sh             # keep the fixtures
#
# WORK defaults under $TMPDIR. The fixtures are large (a prod-scale catalog
# plus one private copy per reader slot for the control arm), so point TMPDIR
# or WORK at a filesystem with room — the script refuses to start without it.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

SETS="${SETS:-200}"
CARDS_PER_SET="${CARDS_PER_SET:-220}"
OWNED="${OWNED:-3000}"
SECONDS_PER="${SECONDS_PER:-10}"
REPS="${REPS:-3}"
LEVELS="${LEVELS:-1,2,4,8,16}"

MAX_READERS=$(echo "$LEVELS" | tr ',' '\n' | sort -n | tail -1)
WORK="${WORK:-${TMPDIR:-/tmp}/pd-attach-concurrency}"

# One catalog (~110 MiB at the default size) plus one private copy per reader
# slot for the control arm and the tenant databases, then headroom for the
# write-ahead log: the writer arm deliberately runs with readers holding a
# checkpoint off, and the WAL gets into the hundreds of MiB doing it. Checked
# up front because running out of disk halfway through wastes the whole run.
NEED_MB=$(( (MAX_READERS + 2) * 130 + 1500 ))

step() { echo ""; echo "==> $*"; }

step "Work dir: ${WORK}"
mkdir -p "$WORK"
AVAIL_MB=$(df -Pm "$WORK" | awk 'NR==2 {print $4}')
if [ "$AVAIL_MB" -lt "$NEED_MB" ]; then
    echo "ERROR: ${WORK} has ${AVAIL_MB} MiB free; this run needs about ${NEED_MB} MiB."
    echo "       Set WORK=/some/roomier/path (or TMPDIR) and re-run."
    exit 1
fi
echo "    ${AVAIL_MB} MiB free, ~${NEED_MB} MiB needed."

if [ "${KEEP:-0}" != "1" ]; then
    trap 'rm -rf "$WORK"' EXIT
fi

step "Building the harness (release — a debug build measures rustc, not SQLite)"
( cd "$REPO_DIR" && cargo build --release --example attach_concurrency )

step "Measuring: levels ${LEVELS}, ${REPS} rep(s) x ${SECONDS_PER}s per scenario"
# Not $REPO_DIR/target: cargo writes wherever CARGO_TARGET_DIR says, and CI now
# points that outside the checkout entirely (pd-uikc). Same expression the other
# two scripts that reach for a built binary already use.
"${CARGO_TARGET_DIR:-$REPO_DIR/target}/release/examples/attach_concurrency" \
    --work "$WORK" \
    --sets "$SETS" \
    --cards-per-set "$CARDS_PER_SET" \
    --owned "$OWNED" \
    --seconds "$SECONDS_PER" \
    --reps "$REPS" \
    --levels "$LEVELS"

if [ "${KEEP:-0}" = "1" ]; then
    echo ""
    echo "KEEP=1 — fixtures and result.json left in ${WORK}"
fi
