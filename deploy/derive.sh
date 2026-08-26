#!/usr/bin/env bash
#
# The nightly offline catalog derive (pd-1uem). What
# pkdump-derive@<instance>.service actually runs.
#
# `pkdump-lake-derive shared` rebuilds the instance's `shared.sqlite` from ONE
# `raw/` partition — the bytes the landing unit put in the bucket — replaying
# every upstream response instead of fetching it. This file is that invocation,
# with the instance's data volume, image and lake credentials resolved from
# where they actually live, so a timer can run it unattended.
#
# Usage:
#   bash deploy/derive.sh <instance> [--ingest-date YYYY-MM-DD] [job args...]
#
#   bash deploy/derive.sh prod                            # today's partition
#   bash deploy/derive.sh prod --ingest-date 2026-08-09   # rebuild an older day
#
# ── TWO UNITS, AND WHY ──────────────────────────────────────────────────────
# Landing and deriving are separate scheduled units on purpose: a derive can
# then run against yesterday's raw on a night the fetch failed. What makes that
# a feature rather than a trap is that the derive builds the partition it was
# ASKED for and refuses anything else — it never reaches for the newest
# available date, so yesterday's raw cannot quietly become today's catalog.
#
# ── THE DATE, AND WHERE THE CLOCK BELONGS ───────────────────────────────────
# The job REFUSES to default --ingest-date from the clock, deliberately and for
# the same reason `pkdump-lake-value-snapshots` refuses --date: rebuilding an
# older partition IS the same operation, and a job that reads the clock has two
# behaviours where it should have one. A scheduler, on the other hand, is
# exactly the thing that is allowed to know what day it is — so today's date is
# named here, once, and passed in explicitly. An explicit --ingest-date from the
# caller wins.
#
# ── EXIT STATUS IS THE JOB'S, UNCHANGED ─────────────────────────────────────
#   0  the catalog was derived from that partition
#   1  it was not — the partition is absent, incomplete, clockless, missing a
#      URL the derivation asked for, or the derivation itself failed
#
# There is no exit 2 here, unlike the value-snapshot wrapper, and that asymmetry
# is the point. That job writes N tenant databases and "some of them" is a real
# and normal outcome. This one writes ONE catalog: it either holds that date's
# data or it does not, and a partial catalog reads as cards that do not exist.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: derive.sh <instance> [--ingest-date YYYY-MM-DD] [job args...]}"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"

# Host config, and the refusal names it — the lake bucket is a fact about this
# AWS account, never a repo constant. PKDUMP_LAKE_ENV is a test seam (the
# store.env / PKDUMP_ALERTS_ENV precedent); production never sets it.
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"

# Where the job runs. Defaults are the conventions deploy/setup.sh installs;
# each is overridable so tests/deploy/run.sh can drive this exact script against
# a stand-in. Production sets none of them.
IMAGE="${PKDUMP_DERIVE_IMAGE:-localhost/pkdump:${INSTANCE}}"
# A podman volume name or a host path — `-v` takes either.
DATA="${PKDUMP_DERIVE_DATA:-pkdump-${INSTANCE}-data}"

if [ ! -f "$LAKE_ENV" ]; then
    echo "derive: ${LAKE_ENV} does not exist — this box has no lake configured." >&2
    echo "  The bucket's name is host config, like alerts.env and litestream.env beside it." >&2
    echo "  Without a landing zone there is no raw/ to derive from — and since pd-lunn" >&2
    echo "  nothing else builds the catalog either. 'pkdump data refresh' lands; this job" >&2
    echo "  builds. A box with no lake has no nightly catalog update: 'pkdump setup'." >&2
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

# --- Is there an image to run at all? ---------------------------------------
# The same guard the value-snapshot wrapper carries, for the same reason: the
# image is built locally and named localhost/…, so without this the run dies
# inside podman retrying `localhost` as a registry, and the operator reads a
# network error instead of "this instance was never set up".
if ! podman image exists "$IMAGE" 2>/dev/null; then
    echo "derive: no image ${IMAGE}." >&2
    echo "  '${INSTANCE}' has not been built on this box: bash deploy/setup.sh ${INSTANCE}" >&2
    exit 1
fi

# --- The job's command line -------------------------------------------------

ARGS=("$@")
case " $* " in
*" --ingest-date "* | *" --ingest-date="*) ;;
*) ARGS=(--ingest-date "$(date -u +%F)" "${ARGS[@]}") ;;
esac

# --- The container's environment --------------------------------------------
# Only the lake's own settings. The derive reads raw/ and writes SQLite; it
# talks to no catalog service and needs no Nessie.
ENV_ARGS=()
for VAR in PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_S3_PREFIX \
    PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_DIR AWS_PROFILE; do
    [ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done

# Credentials, the same assume-role path the Litestream sidecar and the
# transform tier use: a non-secret role profile as a file, the bootstrap key as
# a podman secret, auto-refreshing temporary credentials and never a long-lived
# static key. Both are mounted only when both exist — an instance with no AWS
# config is a test instance pointed at a directory-backed lake.
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
# The data directory is mounted read-WRITE: the catalog is the job's output.
# Not :ro either way — shared.sqlite is a WAL database and SQLite cannot open
# one through a read-only mount at all.
#
# Streamed straight to the journal rather than captured: variant expansion
# prints a line per thousand cards over ~50k of them, so a run that hangs must
# be visible WHILE it is hanging rather than only in its epitaph. Nothing reads
# the output back any more — the coverage warning that used to grep it went
# with the fallback (item 4), and what is left is the exit status.

echo "==> derive ${INSTANCE}: ${ARGS[*]}"
echo "    data ${DATA}, image ${IMAGE}"

RC=0
podman run --rm --pull=never \
    -v "${DATA}:/data:Z" \
    "${ENV_ARGS[@]}" ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} \
    --entrypoint pkdump-lake-derive \
    "$IMAGE" shared --db /data/shared.sqlite --data-dir /data "${ARGS[@]}" 2>&1 || RC=$?

if [ "$RC" -eq 0 ]; then
    # Exit 0 now carries the lineage claim on its own. A URL the partition does
    # not hold is a refusal inside the job (item 4 removed the fallback), so
    # there is no longer a "correct catalog, unreproducible lineage" outcome to
    # warn about — that shape exits 1 and pages like any other failure.
    echo "derive: OK — the catalog was rebuilt from raw/ (${INSTANCE})"
else
    echo "derive: FAILED — the catalog was NOT rebuilt (${INSTANCE})" >&2
fi

exit "$RC"
