#!/usr/bin/env bash
#
# The nightly ownership shipment (pd-dxn3). What
# pkdump-ship@<instance>.service actually runs.
#
# `pkdump-ship run` moves every registered tenant's ownership outbox into the
# tenant zone, encrypted under that tenant's derived key. This file is that
# invocation, with the instance's data volume, image, master key and TENANT
# credentials resolved from where they actually live, so a timer can run it
# unattended.
#
# Usage:
#   bash deploy/ship.sh <instance> [job args...]
#
#   bash deploy/ship.sh prod                  # every registered tenant
#   bash deploy/ship.sh prod --tenant alice   # one, after a repair
#
# ── ITS OWN CONTAINER, NOT `podman exec` INTO THE SERVER ────────────────────
# Like the derive and the transform beside it, and for the reason pd-kncd cost
# a night's landing: `podman exec` cannot add a mount or a credential to a
# running container, so the only way to give this job the master key and the
# tenant-zone role through the server would be to put them ON the server. That
# is precisely the coupling the tenant zone exists to prevent — an always-on
# web server holding a credential that reaches every tenant's holdings and a
# key that derives every tenant's key. Nothing that serves a request holds
# either.
#
# ── NO DATE ─────────────────────────────────────────────────────────────────
# Unlike derive.sh and value-snapshots.sh, this wrapper passes no date and the
# job takes none. Every partition value the shipper writes comes out of the
# event's own timestamp, so there is nothing for a scheduler to choose and no
# clock for it to lend. See crates/pkdump-ship/src/lib.rs, decision 1.
#
# ── EXIT STATUS IS THE JOB'S, UNCHANGED — AND THERE ARE FOUR ────────────────
#   0  every registered tenant shipped
#   2  the run COMPLETED, some tenants were skipped (absent or locked database,
#      or one whose key nobody has registered). A normal partial night: warned
#      here, and SuccessExitStatus=2 in the unit so it does not page.
#   3  SEQUENCE GAP — outbox events were LOST before they reached the zone.
#      The offline copy is incomplete and nothing else in the system would ever
#      notice. This pages, with its own message, because the journal tail
#      OnFailure= sends does not say which tenant or which range.
#   1  the run never started, or shipped nobody at all.
#
# 3 is a separate status rather than folded into 1 because the two need
# different first sentences at 3am: "nothing shipped" is an outage, "something
# was lost" is a data-integrity fact that survives the outage being fixed.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: ship.sh <instance> [job args...]}"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${PKDUMP_KEYS_CONF_DIR:-${HOME}/.config/pkdump/${INSTANCE}}"

# Host config, and the refusals name it. PKDUMP_LAKE_ENV / PKDUMP_ALERTS_ENV
# are test seams (the store.env precedent); production sets neither.
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"

IMAGE="${PKDUMP_SHIP_IMAGE:-localhost/pkdump:${INSTANCE}}"
# The podman network the job runs on. Production leaves it UNSET: the tenant
# zone is real S3, reached over the default network, and putting this job on a
# user-defined network would be a way to reach something that is not S3.
# tests/lake/shipper.sh sets it so the job can reach its own MinIO.
NETWORK="${PKDUMP_SHIP_NETWORK:-}"
# A podman volume name or a host path — `-v` takes either.
DATA="${PKDUMP_SHIP_DATA:-pkdump-${INSTANCE}-data}"
KEY_FILE="${CONF_DIR}/tenant-master.key"

if [ ! -f "$LAKE_ENV" ]; then
    echo "ship: ${LAKE_ENV} does not exist — this box has no lake configured." >&2
    echo "  The tenant zone lives in the lake's bucket under its own prefix and its own" >&2
    echo "  credentials; without that file there is nowhere to ship to." >&2
    echo "  Create the bucket, write the file, then: bash deploy/setup-tenant-zone.sh --apply" >&2
    exit 1
fi
# shellcheck disable=SC1090
{ set -a; . "$LAKE_ENV"; set +a; }
[ -f "$ALERTS_ENV" ] && { set -a; . "$ALERTS_ENV"; set +a; }

# The credential boundary, checked here rather than discovered inside the
# container. `TenantZoneConfig` refuses an unset profile and refuses one equal
# to the catalog's, but it does so after podman has started, which makes a
# configuration mistake read as a container failure.
if [ -z "${PKDUMP_TENANT_AWS_PROFILE:-}" ]; then
    echo "ship: ${LAKE_ENV} does not set PKDUMP_TENANT_AWS_PROFILE." >&2
    echo "  The tenant zone shares the lake's bucket, so the ONLY thing separating the two" >&2
    echo "  is that they are reached by different credentials. There is no default: falling" >&2
    echo "  back to the catalog's profile would erase the boundary silently." >&2
    echo "  See deploy/TENANT_ZONE.md." >&2
    exit 1
fi

# The master key is what makes a tenant's part unreadable to anyone without it.
# A run without one derives nothing and skips every tenant — which the job
# reports as exit 1 — but it is worth failing here, where the message can say
# which file and which command.
if [ ! -f "$KEY_FILE" ]; then
    echo "ship: no master key at ${KEY_FILE}." >&2
    echo "  Every object in the tenant zone is encrypted under a key derived from it, so" >&2
    echo "  there is nothing to ship without one. Mint it (once, and back it up before" >&2
    echo "  anything depends on it): bash deploy/keys.sh ${INSTANCE} init" >&2
    echo "  See deploy/KEYS.md." >&2
    exit 1
fi

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Is there an image to run at all? ---------------------------------------
# The same guard the derive and transform wrappers carry: the image is built
# locally and named localhost/…, so without this the run dies inside podman
# retrying `localhost` as a registry and the operator reads a network error
# instead of "this instance was never set up".
if ! podman image exists "$IMAGE" 2>/dev/null; then
    echo "ship: no image ${IMAGE}." >&2
    echo "  '${INSTANCE}' has not been built on this box: bash deploy/setup.sh ${INSTANCE}" >&2
    exit 1
fi

# --- The container's environment --------------------------------------------
# The lake's location, the TENANT profile, and nothing that would let this
# process reach the catalog zone by accident. AWS_PROFILE is forwarded because
# `TenantZoneConfig` reads it for exactly one purpose — refusing to be equal to
# it — and a comparison against a value that never arrived cannot refuse
# anything.
ENV_ARGS=(-e "PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key")
#
# AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are forwarded for the test
# substrate only — a MinIO takes static keys and has no role to assume.
# **Production sets neither**: it uses the assume-role file plus the podman
# secret below, so its credentials are temporary and auto-refreshing (the
# standing rule for every S3 path here). They are listed rather than silently
# inherited because `podman run` passes no environment on by default, and a
# gate that could not reach its own object store would be a gate nobody ran.
for VAR in PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_S3_PREFIX \
    PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_DIR PKDUMP_TENANT_AWS_PROFILE \
    PKDUMP_TENANT_S3_PREFIX AWS_PROFILE \
    AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY; do
    [ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done

# Credentials, the same assume-role path every other job here uses: a
# non-secret role profile as a file, the bootstrap key as a podman secret,
# auto-refreshing temporary credentials, never a long-lived static key.
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
# The data directory is mounted read-WRITE: the shipper's own output is the
# cursor and the gap ledger in each tenant's database. The master key is
# mounted read-ONLY and as a single file — this job derives keys and must never
# be the thing that can replace one.

echo "==> ship ${INSTANCE}${*:+: $*}"
echo "    data ${DATA}, image ${IMAGE}, tenant profile ${PKDUMP_TENANT_AWS_PROFILE}"

LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

NET_ARGS=()
[ -n "$NETWORK" ] && NET_ARGS=(--network "$NETWORK")

RC=0
podman run --rm --pull=never ${NET_ARGS[@]+"${NET_ARGS[@]}"} \
    -v "${DATA}:/data:Z" \
    -v "${KEY_FILE}:/keys/tenant-master.key:ro" \
    "${ENV_ARGS[@]}" ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} \
    --entrypoint pkdump-ship \
    "$IMAGE" run --data-dir /data "$@" 2>&1 |
    tee "$LOG" || RC=$?
OUT="$(cat "$LOG")"

case "$RC" in
0)
    echo "ship: OK — every registered tenant's outbox is in the tenant zone (${INSTANCE})"
    ;;
2)
    # Named, so the journal line says WHO rather than "some".
    SKIPPED="$(printf '%s\n' "$OUT" | sed -n 's/^ *skipped \([^:]*\):.*/\1/p' | paste -sd' ' -)"
    echo "ship: PARTIAL — tenants skipped: ${SKIPPED:-unknown} (${INSTANCE})" >&2
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster shipping PARTIAL (${INSTANCE})" \
        "Skipped: ${SKIPPED:-unknown}. Everyone else shipped; see journalctl --user -u pkdump-ship@${INSTANCE}.service" ||
        echo "ship: the PARTIAL warning reached nobody (no Pushover channel configured) — the run itself is unaffected" >&2
    ;;
3)
    # The one this whole item exists to make possible. A gap means outbox
    # events were LOST: the tenant zone is missing them and no later run can
    # find them, because the sequence numbers are never reissued.
    GAPS="$(printf '%s\n' "$OUT" | sed -n 's/^ *!! SEQUENCE GAP //p' | paste -sd'; ' -)"
    echo "ship: SEQUENCE GAP — outbox events were lost (${INSTANCE}): ${GAPS:-see the journal}" >&2
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster OUTBOX GAP (${INSTANCE})" \
        "Events were LOST before reaching the tenant zone: ${GAPS:-see the journal}. Recorded in each collection's ownership_outbox_gap; see deploy/TENANT_ZONE.md." ||
        echo "ship: the GAP alarm reached nobody (no Pushover channel configured) — the unit still fails, which is the remaining signal" >&2
    ;;
*)
    # OnFailure= on the unit pushes the journal tail for this one; a second
    # push from here would say less and arrive twice.
    echo "ship: FAILED — nothing shipped (${INSTANCE})" >&2
    ;;
esac

exit "$RC"
