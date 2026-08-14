#!/usr/bin/env bash
#
# Delete an account from the TENANT ZONE, and prove it (pd-qbrf).
#
# `pkdump-erase delete` records a tombstone against the tenant's key, drops
# their partition, and then attempts every path by which their holdings or
# valuations could still be read — requiring every one to fail. This file is
# that invocation with the instance's data volume, image, master key and TENANT
# credentials resolved from where they actually live.
#
# Usage:
#   bash deploy/erase.sh <instance> <subcommand> [job args...]
#
#   bash deploy/erase.sh prod list   --tenant alice           # what is there
#   bash deploy/erase.sh prod verify --tenant alice           # ask; change nothing
#   bash deploy/erase.sh prod delete --tenant alice --yes --reason "account closed"
#
# ── THERE IS NO TIMER, AND THAT IS THE POINT ────────────────────────────────
# Every other job under deploy/ that runs a container is wrapped by a unit and
# fired by a calendar. This one is not, and must not be: a deletion is an act
# somebody decides to perform on one named account. A scheduled deleter is a
# thing that can delete the wrong account at 3am with nobody watching, and
# there is no undo — the tombstone is never lifted and the objects do not come
# back. `deploy/units-lib.sh` installs no unit for this.
#
# ── ITS OWN CONTAINER, NOT `podman exec` INTO THE SERVER ────────────────────
# Like ship.sh, derive.sh and the transform beside it, and for the reason
# pd-kncd cost a night's landing: `podman exec` cannot add a mount or a
# credential to a running container, so the only way to give this job the
# master key and the tenant-zone role through the server would be to put them
# ON the server. Nothing that serves a request holds either — and a deleter on
# the always-on web server would be the worst instance of that rule being
# broken.
#
# ── EXIT STATUS IS THE JOB'S, UNCHANGED ─────────────────────────────────────
#   0  deleted, and PROVEN unreachable on every path
#   4  it RAN and the deletion is NOT PROVEN. The data may well be gone; the
#      evidence is not there. Alarmed from here, because there is no unit and
#      therefore no OnFailure= to do it.
#   1  it could not proceed at all — no image, no key, no credentials, no such
#      tenant. Nothing was deleted.
#
# 4 is separate from 1 for the reason ship.sh separates 3 from 1: they need
# different first questions. "It never ran" is an operational problem to fix
# and retry; "it ran and cannot be proven" is a question about what is still
# reachable, and retrying is not obviously the answer.
#
# Prod-safe in the sense that matters here: it touches one prefix in one
# bucket, and no unit, no volume beyond the instance's own data directory, and
# no collection database. The ONLINE half of removing an account —
# `pkdump tenant detach` and `pkdump tenant purge` — is deliberately a
# different command; see deploy/TENANTS.md.
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: erase.sh <instance> <delete|verify|list> [job args...]}"
shift
SUBCOMMAND="${1:?usage: erase.sh <instance> <delete|verify|list> [job args...]}"
shift

case "$SUBCOMMAND" in
delete | verify | list) ;;
*)
	echo "erase: unknown subcommand ${SUBCOMMAND}. Expected delete, verify or list." >&2
	exit 1
	;;
esac

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${PKDUMP_KEYS_CONF_DIR:-${HOME}/.config/pkdump/${INSTANCE}}"

# Host config, and the refusals name it. PKDUMP_LAKE_ENV / PKDUMP_ALERTS_ENV
# are test seams (the store.env precedent); production sets neither.
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"

IMAGE="${PKDUMP_ERASE_IMAGE:-localhost/pkdump:${INSTANCE}}"
# Production leaves this UNSET: the tenant zone is real S3 over the default
# network, and a user-defined network would be a way to reach something that is
# not S3. tests/lake/deletion.sh sets it so the job can reach its own MinIO.
NETWORK="${PKDUMP_ERASE_NETWORK:-}"
# A podman volume name or a host path — `-v` takes either.
DATA="${PKDUMP_ERASE_DATA:-pkdump-${INSTANCE}-data}"
KEY_FILE="${CONF_DIR}/tenant-master.key"

if [ ! -f "$LAKE_ENV" ]; then
	echo "erase: ${LAKE_ENV} does not exist — this box has no lake configured." >&2
	echo "  The tenant zone lives in the lake's bucket under its own prefix and its own" >&2
	echo "  credentials; without that file there is no partition to drop." >&2
	echo "  See deploy/TENANT_ZONE.md." >&2
	exit 1
fi
# shellcheck disable=SC1090
{
	set -a
	. "$LAKE_ENV"
	set +a
}
# shellcheck disable=SC1090
[ -f "$ALERTS_ENV" ] && {
	set -a
	. "$ALERTS_ENV"
	set +a
}

# The credential boundary, checked here rather than discovered inside the
# container — the same guard ship.sh carries, and for the same reason: a
# configuration mistake found after podman has started reads as a container
# failure.
if [ -z "${PKDUMP_TENANT_AWS_PROFILE:-}" ]; then
	echo "erase: ${LAKE_ENV} does not set PKDUMP_TENANT_AWS_PROFILE." >&2
	echo "  The tenant zone shares the lake's bucket, so the ONLY thing separating the two" >&2
	echo "  is that they are reached by different credentials. There is no default." >&2
	echo "  See deploy/TENANT_ZONE.md." >&2
	exit 1
fi

# The master key is not needed to REVOKE anything — a tombstone is a row, and
# that is deliberately true even on a box that has lost its key (pd-ulds). It is
# needed to PROVE the revocation means something: without it nothing derives for
# anybody, so "no key opens this" would be true of every tenant alive and the
# verification refuses to conclude from it. Failing here says which file.
if [ ! -f "$KEY_FILE" ]; then
	echo "erase: no master key at ${KEY_FILE}." >&2
	echo "  A deletion can still be RECORDED without it — a tombstone is a registry row." >&2
	echo "  What cannot be done is PROVING the result: on a box that derives nothing for" >&2
	echo "  anybody, 'unreadable' is true of every tenant and says nothing about this one." >&2
	echo "  Restore it first: bash deploy/keys.sh ${INSTANCE} restore" >&2
	echo "  See deploy/KEYS.md and deploy/DELETION.md." >&2
	exit 1
fi

# Look in the store the instance actually lives in (pd-fite). No-op for prod,
# whose unit carries no store flags.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Is there an image to run at all? ---------------------------------------
if ! podman image exists "$IMAGE" 2>/dev/null; then
	echo "erase: no image ${IMAGE}." >&2
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
# Production sets neither: it uses the assume-role file plus the podman secret
# below, so its credentials are temporary and auto-refreshing.
for VAR in PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_S3_PREFIX \
	PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_DIR PKDUMP_TENANT_AWS_PROFILE \
	PKDUMP_TENANT_S3_PREFIX AWS_PROFILE \
	AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY; do
	[ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done

CRED_ARGS=()
if [ -f "${CONF_DIR}/aws/config" ] && podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1; then
	CRED_ARGS=(
		-v "${CONF_DIR}/aws/config:/aws/config:ro"
		--secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials"
		-e AWS_CONFIG_FILE=/aws/config
		-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials
	)
fi

# A `--stray` copy is a path INSIDE the container, so the file has to get there.
# The operator names a HOST path in PKDUMP_ERASE_STRAY and this mounts it under
# a fixed name, read-only — read-only because the one thing that must not happen
# to the evidence is this job modifying it. The matching `--stray-key` is passed
# through in "$@" like any other job argument; the job refuses one without the
# other, because a copy that failed to open under an unchecked key would prove
# nothing.
STRAY_MOUNT=()
STRAY_ARG=()
if [ -n "${PKDUMP_ERASE_STRAY:-}" ]; then
	[ -f "$PKDUMP_ERASE_STRAY" ] || {
		echo "erase: PKDUMP_ERASE_STRAY=${PKDUMP_ERASE_STRAY} is not a file." >&2
		echo "  A copy that is not there is not a copy that failed to decrypt." >&2
		exit 1
	}
	STRAY_MOUNT=(-v "${PKDUMP_ERASE_STRAY}:/stray/copy.enc:ro")
	STRAY_ARG=(--stray /stray/copy.enc)
fi

# --- Run it -----------------------------------------------------------------
# The data directory is mounted read-WRITE: the tombstone is a row in
# registry.sqlite, which lives there. The master key is mounted read-ONLY and
# as a single file — the destruction path must never be able to reach the
# thing whose loss would revoke everybody (pd-ulds, deploy/KEYS.md).

echo "==> erase ${INSTANCE} ${SUBCOMMAND}${*:+ $*}"
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
	${STRAY_MOUNT[@]+"${STRAY_MOUNT[@]}"} \
	--entrypoint pkdump-erase \
	"$IMAGE" "$SUBCOMMAND" --data-dir /data \
	${STRAY_ARG[@]+"${STRAY_ARG[@]}"} "$@" 2>&1 |
	tee "$LOG" || RC=$?

case "$RC" in
0)
	case "$SUBCOMMAND" in
	delete) echo "erase: DELETED — proven unreachable on every path (${INSTANCE})" ;;
	verify) echo "erase: PROVEN — no path reaches this tenant's data (${INSTANCE})" ;;
	list) : ;;
	esac
	;;
4)
	# There is no unit here and therefore no OnFailure=. This is the only
	# out-of-band signal, and a deletion is exactly the operation somebody
	# starts and walks away from.
	FAILED="$(sed -n 's/^ *OPEN  *\([a-z=]*\) .*/\1/p' "$LOG" | paste -sd' ' -)"
	echo "erase: NOT PROVEN — ${FAILED:-see the output above} (${INSTANCE})" >&2
	echo "  The deletion may have happened; the evidence that it did is incomplete." >&2
	echo "  See deploy/DELETION.md — the checks are named and each has its own cause." >&2
	"${SCRIPT_DIR}/alert.sh" "PokeDumpster DELETION NOT PROVEN (${INSTANCE})" \
		"Paths still open: ${FAILED:-see the journal}. A deletion ran and could not be proven; see deploy/DELETION.md." ||
		echo "erase: the NOT PROVEN alarm reached nobody (no Pushover channel configured) — this exit status is the remaining signal" >&2
	;;
*)
	echo "erase: FAILED — the run did not proceed, and nothing was deleted (${INSTANCE})" >&2
	;;
esac

exit "$RC"
