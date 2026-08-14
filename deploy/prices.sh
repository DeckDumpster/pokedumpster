#!/usr/bin/env bash
#
# The nightly catalog.prices build (pd-up36). What
# pkdump-prices@<instance>.service actually runs.
#
# `pkdump-lake-build-prices` turns one `raw/` partition into one `observed_date`
# partition of the Iceberg table `catalog.prices`. deploy/LAKE.md §6 documented
# it as a hand-run podman invocation; this file is that invocation, with the
# instance's network and lake credentials resolved from where they actually
# live, so a timer can run it unattended.
#
# Usage:
#   bash deploy/prices.sh <instance> [--ingest-date YYYY-MM-DD] [job args...]
#
#   bash deploy/prices.sh prod                            # today's partition
#   bash deploy/prices.sh prod --ingest-date 2026-08-09   # rebuild an older day
#
# ── THE GAP THIS CLOSES ─────────────────────────────────────────────────────
# pd-8m5c put the transform tier on a timer, and it values every tenant's
# collection from the newest quote in `catalog.prices` at or before its --date.
# Nothing built that table on a schedule. So on a box with the lakehouse armed,
# the nightly value snapshot was correct arithmetic over whatever day someone
# last ran the build by hand — advancing every night, with nothing anywhere
# saying the prices had stopped moving. Same failure class as pd-s5yn: a job
# that looks like it ran.
#
# ── THE DATE, AND WHERE THE CLOCK BELONGS ───────────────────────────────────
# The job REFUSES to default --ingest-date from the clock, deliberately and for
# the same reason `pkdump-lake-derive` and `pkdump-lake-value-snapshots` refuse
# theirs: rebuilding an older partition IS the same operation, and a job that
# reads the clock has two behaviours where it should have one. A scheduler, on
# the other hand, is exactly the thing that is allowed to know what day it is —
# so today's date is named here, once, and passed in explicitly. UTC, because
# that is what a raw/ ingest_date partition is named in. An explicit
# --ingest-date from the caller wins.
#
# ── BUILD ANYWAY, RECORD PROVENANCE, ALARM ON AGE ───────────────────────────
# `--allow-incomplete` is passed by default, and that is a decision rather than
# a convenience.
#
# The build refuses a day holding no COMPLETE run, and `complete` is
# conservative across datasets (deploy/LAKE.md §1): one landing invocation
# carries one `run=`, so a pokemontcg.io tail that died marks the *prices*
# manifest incomplete even though every price fetch succeeded. That is the
# normal shape of a flaky night, and the price bytes are fine. A nightly job
# that failed and paged on it would page most nights — and a pager that cries
# wolf gets ignored, which this project has already paid a day for (pd-me6h).
#
# So the day is built, the snapshot records `pkdump.raw-complete=false`, and
# the alarm moves onto the property that actually matters: the AGE of the
# newest partition. That check runs below, on every run, whatever the build
# did. A check that only fires on the failure path is a check that almost never
# runs, and one nobody would notice had broken.
#
# ── EXIT STATUS: 0 / 2 / 1 ARE THREE DIFFERENT ANSWERS ──────────────────────
#   0  today's partition was built AND catalog.prices is fresh.
#   2  the build did not produce today's partition, and the table is STILL
#      within its freshness window. A single missed night — a landing that
#      failed, a transient S3 error — is not an outage: yesterday's prices
#      value today's collection correctly enough, and tomorrow's build fills
#      the day in. The unit's SuccessExitStatus=2 keeps that out of `failed`,
#      and the run is not silent either: it says MISSED and pushes a warning.
#   1  catalog.prices is STALE — nothing new has landed in it for longer than
#      the window — or the freshness of the table could not be established at
#      all. Both page. Not being able to ask is not the same answer as "fine".
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: prices.sh <instance> [--ingest-date YYYY-MM-DD] [job args...]}"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"

# Host config, and the refusal names it — the lake bucket is a fact about this
# AWS account, never a repo constant. PKDUMP_LAKE_ENV is a test seam (the
# store.env / PKDUMP_ALERTS_ENV precedent); production never sets it.
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
# Host-wide Pushover creds for the warnings below.
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"

# Where the job runs. Defaults are the conventions deploy/setup-lake.sh
# installs; each is overridable so a gate can drive this exact script against
# its own throwaway network and image. Production sets none of them.
NETWORK="${PKDUMP_LAKE_NETWORK:-pkdump-lake-${INSTANCE}}"
JOB_IMAGE="${PKDUMP_LAKE_JOB_IMAGE:-localhost/pkdump-lake:${INSTANCE}}"

if [ ! -f "$LAKE_ENV" ]; then
    echo "prices: ${LAKE_ENV} does not exist — this box has no lake configured." >&2
    echo "  The bucket's name is host config, like alerts.env and litestream.env beside it." >&2
    echo "  Create the bucket, write the file, then: bash deploy/setup-lake.sh ${INSTANCE}" >&2
    exit 1
fi
# shellcheck disable=SC1090
{ set -a; . "$LAKE_ENV"; set +a; }
[ -f "$ALERTS_ENV" ] && { set -a; . "$ALERTS_ENV"; set +a; }

# How far behind the newest partition may fall before this pages. Two days, so
# one missed night is absorbed and a build that has actually stopped is caught
# on its third morning. Overridable per box, never per run.
MAX_AGE_DAYS="${PKDUMP_LAKE_PRICES_MAX_AGE_DAYS:-2}"

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Is there a lakehouse here at all? --------------------------------------
# The unit's ConditionPathExists on lake.env does NOT answer this: deploy/setup.sh
# scaffolds that file (commented out) on every box, so it exists everywhere and
# gates nothing. Without this check the run dies inside podman — three retries
# pinging `localhost` as a container registry for a local-only image, then status
# 125 — and the operator reads a network error instead of "you never ran
# setup-lake.sh".
#
# It is a FAILURE (1), not a skip: a timer armed at nothing is a price table
# that never advances, which is the exact silence this whole script exists to
# end.
if ! podman image exists "$JOB_IMAGE" 2>/dev/null; then
    echo "prices: no lake job image ${JOB_IMAGE}." >&2
    echo "  The lakehouse is not installed for '${INSTANCE}': bash deploy/setup-lake.sh ${INSTANCE}" >&2
    exit 1
fi
if ! podman network exists "$NETWORK" 2>/dev/null; then
    echo "prices: no lake network ${NETWORK}." >&2
    echo "  The lakehouse is not installed for '${INSTANCE}': bash deploy/setup-lake.sh ${INSTANCE}" >&2
    exit 1
fi

# --- The job's command line -------------------------------------------------

TODAY="$(date -u +%F)"
ARGS=("$@")
# Which day the build is being asked for, so the journal line can name it rather
# than assume today: an explicit --ingest-date is how an older partition gets
# rebuilt through this same path.
BUILD_DATE="$TODAY"
PREV=""
for A in "$@"; do
    [ "$PREV" = --ingest-date ] && BUILD_DATE="$A"
    case "$A" in
    --ingest-date=*) BUILD_DATE="${A#--ingest-date=}" ;;
    esac
    PREV="$A"
done
case " $* " in
*" --ingest-date "* | *" --ingest-date="*) ;;
*) ARGS=(--ingest-date "$TODAY" "${ARGS[@]}") ;;
esac
# See the header: the day is built even when its manifest says the landing run
# did not finish, and the snapshot records that it was not a whole day.
case " $* " in
*" --allow-incomplete "*) ;;
*) ARGS+=(--allow-incomplete) ;;
esac

# --- The container's environment --------------------------------------------
# Nessie vends the warehouse location, the S3 endpoint and the region through
# /iceberg/v1/config, so the job needs the catalog URL and nothing else against
# a real deployment. The PKDUMP_LAKE_S3_* names are forwarded when lake.env sets
# them because the test substrate (MinIO) takes static keys — see
# lake/src/pkdump_lake/catalog.py.
#
# No data volume is mounted, deliberately: this job reads raw/ from the bucket
# and writes Iceberg. It opens no SQLite file, and nothing keyed by a tenant is
# within its reach.
ENV_ARGS=(-e "PKDUMP_LAKE_NESSIE_URI=${PKDUMP_LAKE_NESSIE_URI:-http://pkdump-nessie-${INSTANCE}:19120/iceberg/}")
for VAR in PKDUMP_LAKE_REF PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION \
    PKDUMP_LAKE_S3_PREFIX PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_S3_ACCESS_KEY_ID \
    PKDUMP_LAKE_S3_SECRET_ACCESS_KEY PKDUMP_LAKE_S3_PATH_STYLE AWS_PROFILE \
    PKDUMP_LAKE_S3_ROLE_ARN PKDUMP_LAKE_S3_BASE_PROFILE; do
    [ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done

# Credentials, the same assume-role path the Litestream sidecar, Nessie and the
# transform tier use: a non-secret role profile as a file, the bootstrap key as
# a podman secret, auto-refreshing temporary credentials and never a long-lived
# static key. Both are mounted only when both exist — an instance with no AWS
# config is a test instance pointed at a MinIO, and mounting a secret that was
# never created fails the run before the job can say anything useful.
CRED_ARGS=()
if [ -f "${CONF_DIR}/aws/config" ] && podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1; then
    CRED_ARGS=(
        -v "${CONF_DIR}/aws/config:/aws/config:ro"
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials"
        -e AWS_CONFIG_FILE=/aws/config
        -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials
    )
fi

# --pull=never: the job image is built locally by deploy/setup-lake.sh and named
# localhost/…, so a pull attempt can only ever be podman treating `localhost` as
# a registry. There is nothing to fetch and nothing to fall back to.
lake_job() {
    podman run --rm --pull=never --network "$NETWORK" \
        "${ENV_ARGS[@]}" ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} \
        "$JOB_IMAGE" "$@"
}

# --- Build the day ----------------------------------------------------------

echo "==> prices ${INSTANCE}: ${ARGS[*]}"
echo "    network ${NETWORK}, image ${JOB_IMAGE}"

BUILD_RC=0
lake_job pkdump-lake-build-prices "${ARGS[@]}" 2>&1 || BUILD_RC=$?

if [ "$BUILD_RC" -eq 0 ]; then
    echo "prices: built — catalog.prices holds that day (${INSTANCE})"
else
    # Not the verdict. Whether this matters is the freshness check's answer,
    # below: a build that missed one night over a table still holding
    # yesterday is a different event from a table nothing has written to in a
    # week, and only the second one is worth waking someone for.
    echo "prices: the build did not produce that partition (${INSTANCE})" >&2
fi

# --- Then ask how old the table actually is ---------------------------------
# Every run, whatever the build did — see the header. On the success path this
# is also the one thing that checks the table rather than the job's own report
# of itself.

AGE_RC=0
lake_job pkdump-lake-prices-age --as-of "$TODAY" --max-age-days "$MAX_AGE_DAYS" 2>&1 || AGE_RC=$?

case "$AGE_RC" in
0)
    if [ "$BUILD_RC" -eq 0 ]; then
        echo "prices: OK — today's partition built, catalog.prices is fresh (${INSTANCE})"
        exit 0
    fi
    # A miss over a fresh table. Warned, not paged — and the warning being
    # undeliverable (no Pushover channel, which is every test instance) is
    # reported rather than promoted to a failure: unlike the stale branch
    # below, nothing here has decided something is wrong.
    echo "prices: MISSED — no partition built for ${BUILD_DATE}, but catalog.prices is still" \
        "within ${MAX_AGE_DAYS} day(s) (${INSTANCE})" >&2
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster price build MISSED a day (${INSTANCE})" \
        "No catalog.prices partition for ${BUILD_DATE}; the table is still fresh, so tenants are valued from a recent day. It becomes an alarm if this repeats past ${MAX_AGE_DAYS} day(s). See journalctl --user -u pkdump-prices@${INSTANCE}.service" ||
        echo "prices: the MISSED warning reached nobody (no Pushover channel configured) — the run itself is unaffected" >&2
    exit 2
    ;;
3)
    # The alarm this whole bead exists for. The unit's OnFailure= will push the
    # journal tail as well; this push is the one that carries the cause in its
    # first line, the same arrangement deploy/backup-check.sh uses.
    echo "prices: STALE — catalog.prices has not advanced in more than ${MAX_AGE_DAYS} day(s) (${INSTANCE})" >&2
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster prices are STALE (${INSTANCE})" \
        "catalog.prices has not advanced in more than ${MAX_AGE_DAYS} day(s). Every tenant is still being valued nightly — from prices that stopped arriving. See journalctl --user -u pkdump-prices@${INSTANCE}.service" || true
    exit 1
    ;;
*)
    echo "prices: FAILED — could not establish the age of catalog.prices (${INSTANCE})" >&2
    exit 1
    ;;
esac
