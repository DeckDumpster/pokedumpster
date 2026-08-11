#!/usr/bin/env bash
#
# The nightly per-tenant value snapshot (pd-8m5c). What
# pkdump-value-snapshots@<instance>.service actually runs.
#
# `pkdump-lake-value-snapshots` values EVERY registered tenant's collection from
# `catalog.prices` at a pinned Nessie commit and writes `collection_value_snapshot`
# into that tenant's own database. deploy/LAKE.md §7 documents it as a hand-run
# podman invocation; this file is that invocation, with the instance's network,
# data volume and credentials resolved from where they actually live, so a timer
# can run it unattended.
#
# Usage:
#   bash deploy/value-snapshots.sh <instance> [--date YYYY-MM-DD] [job args...]
#
#   bash deploy/value-snapshots.sh prod                     # today
#   bash deploy/value-snapshots.sh prod --date 2026-08-09   # backfill one day
#   bash deploy/value-snapshots.sh prod --tenant alice      # one-off repair
#
# ── THE DATE, AND WHERE THE CLOCK BELONGS ───────────────────────────────────
# The job REFUSES to default --date from the clock, deliberately: pointing it at
# an older date IS the backfill, and a job that reads the clock has two
# behaviours where it should have one. A scheduler, on the other hand, is exactly
# the thing that is allowed to know what day it is — so today's date is named
# here, once, and passed in explicitly. An explicit --date from the caller wins.
#
# ── EXIT STATUS IS THE JOB'S, UNCHANGED ─────────────────────────────────────
#   0  every registered tenant snapshotted
#   2  the run COMPLETED, some tenants were skipped (absent or locked database)
#   1  the run never started (no registry, no catalog, an empty lake)
#
# 2 is passed through rather than flattened to 0, because the unit is where
# "partial is not failure" is expressed (SuccessExitStatus=2) and a wrapper that
# swallowed it would leave the unit no way to tell 2 from 0. What this script
# adds on 2 is a PARTIAL line naming the skipped tenants and a Pushover warning —
# a run that half-completes must not be silent, and it must not page as a failure
# either. A warning that cannot be delivered (no channel configured, which is
# every test instance) is reported and does NOT become an error: unlike
# backup-check.sh's stale(), nothing here has decided something is wrong.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: value-snapshots.sh <instance> [--date YYYY-MM-DD] [job args...]}"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"

# Host config, and the refusal names it — the lake bucket is a fact about this
# AWS account, never a repo constant. PKDUMP_LAKE_ENV is a test seam (the
# store.env / PKDUMP_ALERTS_ENV precedent); production never sets it.
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
# Host-wide Pushover creds for the PARTIAL warning below.
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"

# Where the job runs. Defaults are the conventions deploy/setup-lake.sh installs;
# each is overridable so tests/lake/value_snapshots.sh can drive this exact script
# against its own throwaway network, image and fixture directory instead of an
# instance's volume. Production sets none of them.
NETWORK="${PKDUMP_LAKE_NETWORK:-pkdump-lake-${INSTANCE}}"
JOB_IMAGE="${PKDUMP_LAKE_JOB_IMAGE:-localhost/pkdump-lake:${INSTANCE}}"
# A podman volume name or a host path — `-v` takes either.
DATA="${PKDUMP_LAKE_DATA:-pkdump-${INSTANCE}-data}"

if [ ! -f "$LAKE_ENV" ]; then
    echo "value-snapshots: ${LAKE_ENV} does not exist — this box has no lake configured." >&2
    echo "  The bucket's name is host config, like alerts.env and litestream.env beside it." >&2
    echo "  Create the bucket, write the file, then: bash deploy/setup-lake.sh ${INSTANCE}" >&2
    exit 1
fi
# shellcheck disable=SC1090
{ set -a; . "$LAKE_ENV"; set +a; }
[ -f "$ALERTS_ENV" ] && { set -a; . "$ALERTS_ENV"; set +a; }

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Is there a lakehouse here at all? --------------------------------------
# The unit's ConditionPathExists on lake.env does NOT answer this: deploy/setup.sh
# scaffolds that file (commented out) on every box, so it exists everywhere and
# gates nothing. Without this check, a timer enabled for an instance whose
# lakehouse was never installed dies inside podman — three retries pinging
# `localhost` as a container registry for a local-only image, then status 125 —
# and the operator reads a network error instead of "you never ran setup-lake.sh".
#
# It is a FAILURE (1), not a skip: a timer armed at nothing is a value history
# that never advances, which is the exact silence this whole bead exists to end.
if ! podman image exists "$JOB_IMAGE" 2>/dev/null; then
    echo "value-snapshots: no lake job image ${JOB_IMAGE}." >&2
    echo "  The lakehouse is not installed for '${INSTANCE}': bash deploy/setup-lake.sh ${INSTANCE}" >&2
    exit 1
fi
if ! podman network exists "$NETWORK" 2>/dev/null; then
    echo "value-snapshots: no lake network ${NETWORK}." >&2
    echo "  The lakehouse is not installed for '${INSTANCE}': bash deploy/setup-lake.sh ${INSTANCE}" >&2
    exit 1
fi

# --- The job's command line -------------------------------------------------

ARGS=("$@")
case " $* " in
*" --date "* | *" --date="*) ;;
*) ARGS=(--date "$(date +%F)" "${ARGS[@]}") ;;
esac

# --- The container's environment --------------------------------------------
# Nessie vends the warehouse location, the S3 endpoint and the region through
# /iceberg/v1/config, so the job needs the catalog URL and nothing else against a
# real deployment. The PKDUMP_LAKE_S3_* names are forwarded when lake.env sets
# them because the test substrate (MinIO) takes static keys — see
# lake/src/pkdump_lake/catalog.py.
ENV_ARGS=(-e "PKDUMP_LAKE_NESSIE_URI=${PKDUMP_LAKE_NESSIE_URI:-http://pkdump-nessie-${INSTANCE}:19120/iceberg/}")
for VAR in PKDUMP_LAKE_REF PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION \
    PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_S3_ACCESS_KEY_ID \
    PKDUMP_LAKE_S3_SECRET_ACCESS_KEY PKDUMP_LAKE_S3_PATH_STYLE AWS_PROFILE; do
    [ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done

# Credentials, the same assume-role path the Litestream sidecar and Nessie use:
# a non-secret role profile as a file, the bootstrap key as a podman secret,
# auto-refreshing temporary credentials and never a long-lived static key. Both
# are mounted only when both exist — an instance with no AWS config is a test
# instance pointed at a MinIO, and mounting a secret that was never created
# fails the run before the job can say anything useful.
CRED_ARGS=()
if [ -f "${CONF_DIR}/aws/config" ] && podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1; then
    CRED_ARGS=(
        -v "${CONF_DIR}/aws/config:/aws/config:ro"
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials"
        -e AWS_CONFIG_FILE=/aws/config
        -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials
    )
fi

# --- Run it -----------------------------------------------------------------
# The data directory is mounted read-WRITE: the job's whole output is a write
# into each tenant's own database. Not :ro for the catalog either — these are WAL
# databases and SQLite cannot open one through a read-only mount.

echo "==> value-snapshots ${INSTANCE}: ${ARGS[*]}"
echo "    network ${NETWORK}, data ${DATA}, image ${JOB_IMAGE}"

# Through `tee` rather than captured: the job prints a line per tenant and the
# price scan takes a while, so a run that hangs must be visible in the journal
# while it is hanging rather than only in its epitaph. `pipefail` is what keeps
# the job's status the one that is read, not tee's.
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

RC=0
# --pull=never: the job image is built locally by deploy/setup-lake.sh and named
# localhost/…, so a pull attempt can only ever be podman treating `localhost` as
# a registry. There is nothing to fetch and nothing to fall back to.
podman run --rm --pull=never --network "$NETWORK" \
    -v "${DATA}:/data:Z" \
    "${ENV_ARGS[@]}" ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} \
    "$JOB_IMAGE" pkdump-lake-value-snapshots --data-dir /data "${ARGS[@]}" 2>&1 |
    tee "$LOG" || RC=$?
OUT="$(cat "$LOG")"

case "$RC" in
0)
    echo "value-snapshots: OK — every registered tenant snapshotted (${INSTANCE})"
    ;;
2)
    # Named, so the journal line is greppable and says WHO rather than "some".
    SKIPPED="$(printf '%s\n' "$OUT" | sed -n 's/^ *skipped \([^:]*\):.*/\1/p' | paste -sd' ' -)"
    echo "value-snapshots: PARTIAL — tenants skipped: ${SKIPPED:-unknown} (${INSTANCE})" >&2
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster value snapshots PARTIAL (${INSTANCE})" \
        "Skipped: ${SKIPPED:-unknown}. The run completed for everyone else; see journalctl --user -u pkdump-value-snapshots@${INSTANCE}.service" ||
        echo "value-snapshots: the PARTIAL warning reached nobody (no Pushover channel configured) — the run itself is unaffected" >&2
    ;;
*)
    echo "value-snapshots: FAILED — the run never started (${INSTANCE})" >&2
    ;;
esac

exit "$RC"
