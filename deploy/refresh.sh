#!/usr/bin/env bash
#
# The nightly catalog LANDING run (pd-kncd, pd-lunn). What
# pkdump-refresh@<instance>.service actually runs.
#
# `pkdump data refresh` fetches every upstream and writes each response into
# the `raw/` prefix of the lake bucket. It builds no catalog: since pd-lunn
# `shared.sqlite` has exactly ONE builder, `pkdump-lake-derive shared`, run by
# pkdump-derive@<instance>.timer against the partition this job just landed.
# This file is that invocation, with the instance's data volume, image, lake
# settings and AWS credentials resolved from where they actually live, so a
# timer can run it unattended.
#
# Usage:
#   bash deploy/refresh.sh <instance> [refresh args...]
#
#   bash deploy/refresh.sh prod                 # fetch + land
#
# ── LANDING IS NO LONGER OPTIONAL ───────────────────────────────────────────
# There is no --land-raw and no PKDUMP_LAND_RAW opt-in any more, because there
# is nothing left for a non-landing run to do. The flag was the optional half
# of a command whose other half derived the catalog; with that half deleted, a
# refresh that does not land fetches every upstream, parses none of it and
# exits 0 — an expensive no-op against somebody else's API. So `lake.env` is
# REQUIRED here, and the two refusals below fire before the first fetch.
#
# ── AND THE DERIVE TIMER IS NO LONGER OPTIONAL EITHER ───────────────────────
# This is the cutover's one dangerous shape. Landing without deriving is a box
# whose catalog silently stops advancing: every night green, every timer
# healthy, prices frozen at the day of the upgrade. Nothing else here would
# report it — the landing succeeded, and the thing that did not happen has no
# unit to fail. So it is checked, by name, before anything is fetched.
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
#   0  every upstream was fetched and the bytes are in raw/
#   2  PARTIAL: the pokemontcg.io tail gave up, TCGCSV landed (see below)
#   1  it did not run, or it failed
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

# --- The derive timer, which is what turns tonight's bytes into a catalog ---
# Checked FIRST, before a container starts and long before an upstream is
# touched: a refusal that costs an hour of somebody else's API is a bad
# refusal.
#
# There is no seam and no way to skip it. A check that a harness can switch off
# is a check production can be missing, and this is the one condition under
# which the whole cutover fails silently. `systemctl --user is-enabled` is asked
# directly; tests/deploy/run.sh drives this script with a systemctl of its own
# on PATH, which exercises this branch rather than bypassing it.
DERIVE_UNIT="pkdump-derive@${INSTANCE}.timer"
DERIVE_STATE="$(systemctl --user is-enabled "$DERIVE_UNIT" 2>/dev/null || true)"
case "$DERIVE_STATE" in
enabled | enabled-runtime | static | indirect) ;;
*)
    echo "refresh: ${DERIVE_UNIT} is not enabled (${DERIVE_STATE:-not installed})." >&2
    echo "  This job LANDS; it does not build the catalog. Since pd-lunn the only thing" >&2
    echo "  that builds shared.sqlite is pkdump-lake-derive, and that unit is what runs it." >&2
    echo "  Landing without deriving is a box whose catalog silently stops advancing:" >&2
    echo "  every night green, prices frozen at the day of the upgrade. Refusing instead." >&2
    echo "" >&2
    echo "    systemctl --user enable --now ${DERIVE_UNIT}" >&2
    echo "" >&2
    echo "  See deploy/LAKE.md §8. If this box is meant to have no lake at all, it has no" >&2
    echo "  nightly catalog update either — build one by hand with 'pkdump setup'." >&2
    exit 1
    ;;
esac

# --- The lake's settings, which are now REQUIRED --------------------------
# This used to be sourced-when-present, because a box with no lake still
# refreshed its catalog exactly as it did before any of this existed. That is
# no longer a thing a box can do: the catalog is built from raw/, so a refresh
# with nowhere to land is a refresh with nothing to do (pd-lunn). The refusal
# names the file, the way deploy/derive.sh's does.
if [ -f "$LAKE_ENV" ]; then
    # shellcheck disable=SC1090
    { set -a; . "$LAKE_ENV"; set +a; }
else
    echo "refresh: ${LAKE_ENV} does not exist — this box has no lake configured." >&2
    echo "  The lake bucket is host config, like alerts.env and litestream.env beside it." >&2
    echo "  Without a landing zone there is nowhere for tonight's bytes to go, and nothing" >&2
    echo "  that will ever derive them into a catalog. See deploy/LAKE.md §3." >&2
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
if [ -z "${PKDUMP_LAKE_DIR:-}" ] &&
    { [ -z "${PKDUMP_LAKE_S3_BUCKET:-}" ] || [ -z "${PKDUMP_LAKE_S3_REGION:-}" ]; }; then
    echo "refresh: ${LAKE_ENV} does not set PKDUMP_LAKE_S3_BUCKET and PKDUMP_LAKE_S3_REGION." >&2
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

# The retry budget is in this list because pkdump-refresh@.service documents a
# drop-in `Environment=PKDUMP_HTTP_RETRY_ATTEMPTS=` as the way to widen it for a
# stretch of bad upstream weather — and a drop-in sets it in THIS process's
# environment, which is exactly the trip that pd-vk22 showed does not happen by
# itself. A knob a unit documents and a wrapper drops is the same silent no-op
# with a different variable name on it.
for VAR in PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_S3_PREFIX \
    PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_DIR AWS_PROFILE \
    PKDUMP_HTTP_RETRY_ATTEMPTS PKDUMP_HTTP_RETRY_BASE_MS; do
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
elif [ -z "${PKDUMP_LAKE_DIR:-}" ]; then
    # Not a refusal: an endpoint-backed stand-in can be reached with static keys
    # already in this environment, and the run fails loudly at its first PUT
    # either way. It is said out loud because "AccessDenied on part-0000" is a
    # much worse first clue than the sentence below.
    echo "refresh: landing into S3 with no credentials mounted for '${INSTANCE}'" >&2
    echo "  (expected ${CONF_DIR}/aws/config + the pkdump-${INSTANCE}-s3-bootstrap secret)." >&2
    echo "  Unless the AWS environment already carries keys, the first PUT will fail." >&2
fi

# --- Run it -----------------------------------------------------------------
# The data volume is mounted read-write even though this job no longer writes
# the catalog: it READS shared.sqlite to decide which sets are new, and SQLite
# cannot open a WAL database through a read-only mount at all. The read-only
# part of the claim is enforced one level in, at the connection —
# `pkdump_db::open_shared_readonly`.
#
# The server is running against this same volume, as it was when the refresh ran
# inside its container: same host inode, same POSIX locks.
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
echo "    data ${DATA}, image ${IMAGE}"

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

# Exit 2 is PARTIAL, not failed (pd-nons, crates/pkdump-cli/src/data.rs): the
# pokemontcg.io tail ran out of retries, the run carried on, and TCGCSV — the
# perishable half — was fetched and landed. The set list is as old as the last
# run that finished one.
#
# That status deliberately pages, and the reasoning in pkdump-refresh.service is
# right about the danger: a set list that silently stops advancing is a failure
# nothing else on this box would report. It is wrong about the threshold. ONE
# night of upstream 502s is not a silent stall, it is Tuesday — pokemontcg.io
# returned 502 on 2026-08-15 and the page arrived at 06:05 with nothing for Ryan
# to do about someone else's API. Pages like that are why he told me on
# 2026-08-16 he had started ignoring the channel, which is what let a genuine
# outage pass unnoticed.
#
# So the signal is kept and the threshold moved to where the original argument
# actually pointed: PERSISTENCE. A partial run is tolerated while a recent one
# succeeded, and pages once the set list has really stopped advancing. The marker
# lives on the data volume beside .backup-last-ok, so it survives redeploys and
# needs no new state anywhere.
# `DATA` is a volume NAME; the marker needs the host path it resolves to. Empty
# if the volume is gone, and every use below is guarded — a missing marker
# location must not decide whether a real stall pages.
DATA_MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$DATA" 2>/dev/null || true)"
TAIL_MARKER=""
if [ -n "$DATA_MOUNTPOINT" ]; then TAIL_MARKER="${DATA_MOUNTPOINT}/.refresh-last-tail-ok"; fi
PARTIAL_TOLERANCE_HOURS="${PKDUMP_REFRESH_PARTIAL_TOLERANCE_HOURS:-48}"

if [ "$RC" -eq 2 ]; then
    LAST_OK=0
    if [ -n "$TAIL_MARKER" ] && [ -r "$TAIL_MARKER" ]; then
        LAST_OK="$(cat "$TAIL_MARKER" 2>/dev/null || echo 0)"
    fi
    case "$LAST_OK" in ''|*[!0-9]*) LAST_OK=0 ;; esac
    STALL_H=$(( ( $(date +%s) - LAST_OK ) / 3600 ))

    if [ "$LAST_OK" -gt 0 ] && [ "$STALL_H" -lt "$PARTIAL_TOLERANCE_HOURS" ]; then
        echo "refresh: PARTIAL — the pokemontcg.io tail failed, prices and products were landed (${INSTANCE})."
        echo "  The set list last advanced ${STALL_H}h ago, inside the ${PARTIAL_TOLERANCE_HOURS}h tolerance — not paging."
        echo "  If this keeps up it WILL page: that is the stall the status exists to catch."
        exit 0
    fi

    if [ "$LAST_OK" -eq 0 ]; then
        echo "refresh: FAILED — the pokemontcg.io tail failed and no previous run is on record as having finished one (${INSTANCE})." >&2
        echo "  Nothing establishes that the set list was ever current, so this is not treated as a transient." >&2
    else
        echo "refresh: FAILED — the set list has not advanced in ${STALL_H}h (tolerance ${PARTIAL_TOLERANCE_HOURS}h) (${INSTANCE})." >&2
        echo "  Prices are still being landed; it is the pokemontcg.io tail that has stopped. This is the persistent stall, not one bad night." >&2
    fi
    exit 2
fi

if [ "$RC" -ne 0 ]; then
    echo "refresh: FAILED — nothing was landed (${INSTANCE})" >&2
    exit "$RC"
fi

# A full run: record that the tail finished, so the next partial one can tell a
# bad night from a stall.
if [ -n "$TAIL_MARKER" ]; then
    date +%s > "$TAIL_MARKER" 2>/dev/null || true
fi

# A run that never said where it was landing is the silent green no-op this
# whole file exists to make impossible. `pkdump-cli`'s landing::require prints
# that line before the first fetch, so its absence means the lake settings did
# not survive the trip into the container — a wiring regression, and it must not
# be reported as a successful run. It is unconditional now: there is no longer a
# shape of this job that legitimately lands nothing.
if ! printf '%s\n' "$OUT" | grep -q '^Landing raw upstream responses in '; then
    echo "refresh: the run never opened a landing zone (${INSTANCE})" >&2
    echo "  The lake settings did not reach the process. Nothing is in raw/, so there is" >&2
    echo "  nothing for tonight's derive to build a catalog from." >&2
    exit 1
fi

echo "refresh: OK — tonight's upstream responses are in raw/ (${INSTANCE})"
echo "  The catalog is built from them by pkdump-derive@${INSTANCE}.timer, not by this job."
