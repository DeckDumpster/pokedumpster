#!/usr/bin/env bash
#
# The nightly catalog refresh (pd-kncd). What
# pkdump-refresh@<instance>.service actually runs.
#
# `pkdump data refresh` fetches every upstream and rebuilds `shared.sqlite`.
# With landing on (`--land-raw` / `PKDUMP_LAND_RAW=1`) it also writes every
# response it fetched into the `raw/` prefix of the lake bucket, before parsing
# it. This file is that invocation, with the instance's data volume, image, lake
# settings and AWS credentials resolved from where they actually live, so a
# timer can run it unattended.
#
# Usage:
#   bash deploy/refresh.sh <instance> [refresh args...]
#
#   bash deploy/refresh.sh prod                 # fetch + derive, no landing
#   bash deploy/refresh.sh prod --land-raw      # …and land every response
#
# ── WHY THIS FILE EXISTS: podman exec DROPS THE ENVIRONMENT ─────────────────
# The unit used to be one line:
#
#   ExecStart=/bin/sh -c 'exec podman exec systemd-pkdump-%i pkdump data refresh'
#
# `podman exec` does NOT forward the calling process's environment. The exec'd
# process gets the CONTAINER's env plus whatever explicit `-e` flags are on the
# command line, and nothing else. So the documented way to turn landing on — a
# systemd drop-in setting `Environment=PKDUMP_LAND_RAW=1` on this unit — set the
# variable in the wrapper's environment, where the refresh could never see it.
# Measured (pd-vk22):
#
#   $ PKDUMP_LAND_RAW=1 podman exec systemd-pkdump-mutant \
#         sh -c 'echo ${PKDUMP_LAND_RAW:-<unset>}'
#   <unset>
#
# The result was the worst available failure: a green nightly timer that landed
# nothing, with no error anywhere. That is precisely the state deploy/LAKE.md §3
# promises cannot exist ("misconfigured and landed nothing must not look
# alike"), so the promise had to become a mechanism.
#
# ── AND WHY NOT JUST ADD `-e` TO THE EXEC ───────────────────────────────────
# Because the environment was only half of it. The app container mounts the data
# volume and nothing else (pd-8gjd): no AWS config, no bootstrap secret, no lake
# settings. Landing needs all three, and `podman exec` cannot add a mount to a
# container that is already running — only the container's own unit can.
#
# Putting them there would mean the always-on web server holds ambient
# write credentials for the lake bucket, which is exactly the coupling
# `pkdump-lake` is bin-shaped and offline-only to prevent: NOTHING ON THE
# SERVING PATH TOUCHES THE LAKE. It would also make `Secret=` a hard start
# dependency of the app on every box, so an instance with no S3 bootstrap secret
# — every test instance — would fail to serve at all.
#
# So the refresh runs in its OWN container, from the same image, over the same
# volume. That is not a new pattern: it is the one deploy/derive.sh and
# deploy/value-snapshots.sh already use, and it makes the two halves of the
# nightly pair symmetrical — the DERIVING half already ran through a wrapper,
# and the LANDING half was the only job still reaching into the server's
# container to get its work done.
#
# ── EXIT STATUS IS THE COMMAND'S, UNCHANGED ─────────────────────────────────
#   0  the catalog was refreshed (and, with landing on, the bytes are in raw/)
#   1  it was not
#
# There is no exit 2 here and no SuccessExitStatus= on the unit, for the same
# reason deploy/derive.sh has none: this writes ONE catalog, and a catalog that
# is quietly smaller reads as cards that do not exist.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: refresh.sh <instance> [refresh args...]}"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"

# Host config. PKDUMP_LAKE_ENV is a test seam (the store.env / PKDUMP_ALERTS_ENV
# precedent); production never sets it.
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"

# Where the job runs. Defaults are the conventions deploy/setup.sh installs;
# each is overridable so tests/deploy/run.sh can drive this exact script against
# a stand-in. Production sets none of them.
IMAGE="${PKDUMP_REFRESH_IMAGE:-localhost/pkdump:${INSTANCE}}"
# A podman volume name or a host path — `-v` takes either.
DATA="${PKDUMP_REFRESH_DATA:-pkdump-${INSTANCE}-data}"

# --- Is landing being asked for? --------------------------------------------
# Two ways in, and they must agree: the flag on this script's own command line,
# and PKDUMP_LAND_RAW in the environment (which is how the unit's drop-in turns
# it on, since a unit runs a fixed command line). The same two spellings
# crates/pkdump-cli/src/landing.rs accepts.
LAND_RAW=0
case " $* " in *" --land-raw "*) LAND_RAW=1 ;; esac
case "${PKDUMP_LAND_RAW:-}" in 1 | true | yes) LAND_RAW=1 ;; esac

# --- The lake's settings ----------------------------------------------------
# Sourced when the file is there, and NOT required: a box with no lake still
# refreshes its catalog exactly as it did before any of this existed. That is
# the difference from deploy/derive.sh, which reads raw/ and has nothing to do
# without one.
if [ -f "$LAKE_ENV" ]; then
    # shellcheck disable=SC1090
    { set -a; . "$LAKE_ENV"; set +a; }
elif [ "$LAND_RAW" = 1 ]; then
    # Asked for and unconfigured is a REFUSAL that names the file, never a
    # silent skip — deploy/LAKE.md §3. The binary refuses on this too, but it
    # would be refusing about a path inside the container; the file an operator
    # has to write is this one, on the host, and this is the process that reads
    # it.
    echo "refresh: landing was asked for but ${LAKE_ENV} does not exist." >&2
    echo "  The lake bucket is host config, like alerts.env and litestream.env beside it." >&2
    echo "  Write that file (deploy/LAKE.md §3), or stop asking for --land-raw." >&2
    exit 1
fi

# A lake.env that exists and configures nothing the code reads is the SAME
# refusal, and it is the one that actually happened (pd-ub8n): the file on the
# box was written from the design note as PKDUMP_LAKE_BUCKET / _REGION /
# _RAW_PREFIX, which nothing reads. The binary refuses on this too — but from
# inside the container, where $HOME is /root, so its message names
# `/root/.config/pkdump/lake.env`: a path that does not exist on either side and
# tells the operator nothing about the file they have to fix. The names are
# checked HERE, where the real file is, so the refusal can name it.
#
# Deliberately not an alias table. Teaching the code to accept both spellings is
# the fallback logic this project's No-Fallback convention forbids, and a
# half-configured lake that half-works is worse than one that refuses.
if [ "$LAND_RAW" = 1 ] && [ -z "${PKDUMP_LAKE_DIR:-}" ] &&
    { [ -z "${PKDUMP_LAKE_S3_BUCKET:-}" ] || [ -z "${PKDUMP_LAKE_S3_REGION:-}" ]; }; then
    echo "refresh: landing was asked for but ${LAKE_ENV} does not set PKDUMP_LAKE_S3_BUCKET and PKDUMP_LAKE_S3_REGION." >&2
    echo "  Those exact names are what crates/pkdump-lake/src/config.rs reads. An earlier draft" >&2
    echo "  of that file used PKDUMP_LAKE_BUCKET / PKDUMP_LAKE_REGION, which nothing reads —" >&2
    echo "  'the lake is configured' and 'configured with the names the code reads' are not the" >&2
    echo "  same statement (pd-ub8n). See deploy/LAKE.md §3." >&2
    exit 1
fi

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Is there an image to run at all? ---------------------------------------
# The same guard the other two wrappers carry, for the same reason: the image is
# built locally and named localhost/…, so without this the run dies inside
# podman retrying `localhost` as a registry, and the operator reads a network
# error instead of "this instance was never set up".
if ! podman image exists "$IMAGE" 2>/dev/null; then
    echo "refresh: no image ${IMAGE}." >&2
    echo "  '${INSTANCE}' has not been built on this box: bash deploy/setup.sh ${INSTANCE}" >&2
    exit 1
fi

# --- The container's environment --------------------------------------------
# PKDUMP_HOME is what the app container's own unit sets; a fresh container needs
# it named here or the data dir defaults to ~/.pkdump inside the container and
# the refresh rebuilds a catalog nothing will ever read.
ENV_ARGS=(-e PKDUMP_HOME=/data)

# Forwarded UNCONDITIONALLY when set — never gated on the lake being configured.
# A wrapper that dropped this because it could not find a bucket would have
# rebuilt the exact bug this file exists to fix: landing asked for, nothing
# landed, exit 0.
[ "$LAND_RAW" = 1 ] && ENV_ARGS+=(-e PKDUMP_LAND_RAW=1)

for VAR in PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_S3_PREFIX \
    PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_DIR AWS_PROFILE; do
    [ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done

# Credentials, the same assume-role path the Litestream sidecar, Nessie and the
# other two wrappers use: a non-secret role profile as a file, the bootstrap key
# as a podman secret, auto-refreshing temporary credentials and never a
# long-lived static key. Both are mounted only when both exist — an instance
# with no AWS config is a test instance landing into a directory or a MinIO.
CRED_ARGS=()
if [ -f "${CONF_DIR}/aws/config" ] && podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1; then
    CRED_ARGS=(
        -v "${CONF_DIR}/aws/config:/aws/config:ro"
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials"
        -e AWS_CONFIG_FILE=/aws/config
        -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials
    )
elif [ "$LAND_RAW" = 1 ] && [ -z "${PKDUMP_LAKE_DIR:-}" ]; then
    # Not a refusal: an endpoint-backed stand-in can be reached with static keys
    # already in this environment, and the run fails loudly at its first PUT
    # either way. It is said out loud because "AccessDenied on part-0000" is a
    # much worse first clue than the sentence below.
    echo "refresh: landing into S3 with no credentials mounted for '${INSTANCE}'" >&2
    echo "  (expected ${CONF_DIR}/aws/config + the pkdump-${INSTANCE}-s3-bootstrap secret)." >&2
    echo "  Unless the AWS environment already carries keys, the first PUT will fail." >&2
fi

# --- Run it -----------------------------------------------------------------
# The data volume is mounted read-WRITE: the catalog is the job's output. Not
# :ro either way — shared.sqlite is a WAL database and SQLite cannot open one
# through a read-only mount at all.
#
# The server is running against this same volume, as it was when the refresh ran
# inside its container: same host inode, same POSIX locks, one writer.
#
# --entrypoint, because the image's is `pkdump serve`.
# --pull=never, because the image is built locally and named localhost/… — a
# pull attempt can only ever be podman treating `localhost` as a registry.
#
# Through `tee` rather than captured: variant expansion prints a line per
# thousand cards over ~50k of them, so a run that hangs must be visible in the
# journal WHILE it is hanging rather than only in its epitaph. `pipefail` is
# what keeps the job's status the one that is read, not tee's.

echo "==> refresh ${INSTANCE}: $*"
echo "    data ${DATA}, image ${IMAGE}, landing $([ "$LAND_RAW" = 1 ] && echo on || echo off)"

LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

RC=0
podman run --rm --pull=never \
    -v "${DATA}:/data:Z" \
    "${ENV_ARGS[@]}" ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} \
    --entrypoint pkdump \
    "$IMAGE" data refresh "$@" 2>&1 |
    tee "$LOG" || RC=$?
OUT="$(cat "$LOG")"

if [ "$RC" -ne 0 ]; then
    echo "refresh: FAILED — the catalog was NOT refreshed (${INSTANCE})" >&2
    exit "$RC"
fi

# Landing on and a run that never said where it was landing is the silent
# green no-op this whole file exists to make impossible. `pkdump-cli`'s
# landing::open prints that line before the first fetch, so its absence means
# the flag did not survive the trip into the container — a wiring regression,
# not a data problem, and it must not be reported as a successful refresh.
if [ "$LAND_RAW" = 1 ] && ! printf '%s\n' "$OUT" | grep -q '^Landing raw upstream responses in '; then
    echo "refresh: landing was asked for but the run never opened a landing zone (${INSTANCE})" >&2
    echo "  PKDUMP_LAND_RAW did not reach the process — the catalog is fine, raw/ is not." >&2
    exit 1
fi

echo "refresh: OK — the catalog was refreshed (${INSTANCE})"
