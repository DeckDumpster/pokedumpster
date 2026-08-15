#!/usr/bin/env bash
#
# The nightly ownership shipment (pd-dxn3), and the read-back that Phase 3
# values a collection from (pd-i08u). What pkdump-ship@<instance>.service
# actually runs.
#
# `pkdump-ship run` moves every registered tenant's ownership outbox into the
# tenant zone, encrypted under that tenant's derived key. `pkdump-ship
# holdings` brings the zone back into each tenant's `zone_holdings`. This file
# is both invocations, with the instance's data volume, image, master key and
# TENANT credentials resolved from where they actually live, so a timer can run
# them unattended.
#
# Usage:
#   bash deploy/ship.sh <instance> [--tenant HANDLE]
#
#   bash deploy/ship.sh prod                  # every registered tenant
#   bash deploy/ship.sh prod --tenant alice   # one, after a repair
#
# ── WHY BOTH HALVES ARE ONE UNIT ────────────────────────────────────────────
# pd-i08u deleted the online holdings read, so `zone_holdings` is the ONLY
# thing the transform values a collection from. A zone shipped and never read
# back is a transform that skips every tenant; a zone read back without being
# shipped to first is the stale materialisation the transform refuses. The two
# halves are only ever correct together, so they are one unit rather than two
# with an ordering dependency between them — and they need the same mounts
# anyway: the master key and the tenant profile, which nothing else on the box
# holds and the lake image must never hold.
#
# The wrapper's arguments go to BOTH halves, which is why `--tenant` is the
# only one documented: it is the one both subcommands take. `--max-rows` is a
# `pkdump-ship run` argument and part of how a part is ADDRESSED — run that
# command directly (deploy/TENANT_ZONE.md) rather than through the nightly
# wrapper.
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
#   0  every registered tenant shipped, and every one read back
#   2  the run COMPLETED, some tenants were skipped (absent or locked database,
#      or one whose key nobody has registered). A normal partial night: warned
#      here, and SuccessExitStatus=2 in the unit so it does not page.
#   3  SEQUENCE GAP — outbox events were LOST before they reached the zone.
#      The offline copy is incomplete and nothing else in the system would ever
#      notice. This pages, with its own message, because the journal tail
#      OnFailure= sends does not say which tenant or which range.
#   1  the run never started, or shipped nobody at all, or read nobody back.
#
# 3 is a separate status rather than folded into 1 because the two need
# different first sentences at 3am: "nothing shipped" is an outage, "something
# was lost" is a data-integrity fact that survives the outage being fixed.
#
# TWO halves now report into those four numbers, and the precedence is
# 3 > 1 > 2 > 0 — worst thing that happened wins, and a GAP outranks even a
# failed read-back because it is the only one of the four that no later run can
# repair. A read-back that skipped some tenants and a shipment that skipped
# some tenants are the same 2. The journal always names both halves, so a
# single number never has to carry both stories.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: ship.sh <instance> [--tenant HANDLE]}"
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
# cursor and the gap ledger in each tenant's database, and the read-back's is
# `zone_holdings`. The master key is mounted read-ONLY and as a single file —
# this job derives keys and must never be the thing that can replace one.

echo "==> ship ${INSTANCE}${*:+: $*}"
echo "    data ${DATA}, image ${IMAGE}, tenant profile ${PKDUMP_TENANT_AWS_PROFILE}"

LOG="$(mktemp)"
READBACK_LOG="$(mktemp)"
trap 'rm -f "$LOG" "$READBACK_LOG"' EXIT

NET_ARGS=()
[ -n "$NETWORK" ] && NET_ARGS=(--network "$NETWORK")

pkdump_ship() { # pkdump_ship <subcommand> [args…]
    podman run --rm --pull=never ${NET_ARGS[@]+"${NET_ARGS[@]}"} \
        -v "${DATA}:/data:Z" \
        -v "${KEY_FILE}:/keys/tenant-master.key:ro" \
        "${ENV_ARGS[@]}" ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} \
        --entrypoint pkdump-ship \
        "$IMAGE" "$@" --data-dir /data
}

RC=0
pkdump_ship run "$@" 2>&1 | tee "$LOG" || RC=$?
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
    # Anything that is not one of the job's own four numbers is podman's, not
    # the job's — 125 for a container that never started, 127 for a missing
    # entrypoint. It is a failed run, and normalising it here is what keeps the
    # precedence below arithmetic over exactly {0,2,3,1}.
    RC=1
    ;;
esac

# --- The read-back ----------------------------------------------------------
# pd-i08u. `zone_holdings` is what the transform values a collection from, and
# the only thing that writes it is this. Skipped when the shipment shipped
# NOTHING (exit 1): that is an unreachable bucket or a missing master key, and
# the read-back would meet the same wall — one clear failure beats two
# confusing ones. Every other status still reads back, a GAP included: the
# events that survived are in the zone, and withholding them from the
# valuation would add a second loss to the first (the same argument
# `pkdump-ship run` makes for shipping past a gap in the first place).
if [ "$RC" = 1 ]; then
    echo "ship: the zone was not read back — nothing shipped, so nothing new to read (${INSTANCE})" >&2
    exit "$RC"
fi

# Only `--tenant` is carried over: it is the one argument both halves take, and
# a shipment scoped to one tenant that then read back everybody would be a
# surprise. Anything else the caller passed is a `run` argument.
READBACK_ARGS=()
while [ "$#" -gt 0 ]; do
    case "$1" in
    --tenant)
        READBACK_ARGS+=(--tenant "$2")
        shift 2
        ;;
    --tenant=*)
        READBACK_ARGS+=("$1")
        shift
        ;;
    *) shift ;;
    esac
done

RB_RC=0
pkdump_ship holdings ${READBACK_ARGS[@]+"${READBACK_ARGS[@]}"} 2>&1 |
    tee "$READBACK_LOG" || RB_RC=$?
RB_OUT="$(cat "$READBACK_LOG")"

case "$RB_RC" in
0)
    echo "ship: READ BACK — every registered tenant's zone_holdings is current (${INSTANCE})"
    ;;
2)
    RB_SKIPPED="$(printf '%s\n' "$RB_OUT" | sed -n 's/^ *skipped \([^:]*\):.*/\1/p' | paste -sd' ' -)"
    echo "ship: READ BACK PARTIAL — tenants skipped: ${RB_SKIPPED:-unknown} (${INSTANCE})" >&2
    "${SCRIPT_DIR}/alert.sh" "PokeDumpster zone read-back PARTIAL (${INSTANCE})" \
        "Skipped: ${RB_SKIPPED:-unknown}. Those tenants get no value snapshot tonight; see journalctl --user -u pkdump-ship@${INSTANCE}.service" ||
        echo "ship: the READ BACK PARTIAL warning reached nobody (no Pushover channel configured) — the run itself is unaffected" >&2
    ;;
*)
    # Not silent even though OnFailure= pages: this one says WHICH half, and
    # "the shipment worked and the read-back did not" is the whole diagnosis.
    echo "ship: READ BACK FAILED — the zone reached nobody's zone_holdings, so tonight's valuation will skip everyone (${INSTANCE})" >&2
    # As above: the read-back has three numbers, and podman's are not among
    # them.
    RB_RC=1
    ;;
esac

# 3 > 1 > 2 > 0. A gap is the only one no later run can repair, so it is the
# one the unit is told about even when the read-back also went wrong; the
# journal above has already named both halves either way.
for CANDIDATE in 3 1 2; do
    case "${RC}:${RB_RC}" in
    "${CANDIDATE}:"* | *":${CANDIDATE}") exit "$CANDIDATE" ;;
    esac
done
exit 0
