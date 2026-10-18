#!/usr/bin/env bash
#
# The shared Nessie catalog, made durable (sp-87nj2v).
#
# Nessie's RocksDB version store is the ONLY thing that points at the Iceberg
# metadata files. Lose the disk and every table of every project sharing the
# instance is stranded in S3 with no catalog entry. This is the backup, the
# alarm and the restore drill; deploy/LAKE.md §10 is the runbook.
#
# Usage:
#   bash deploy/nessie-backup.sh <instance> backup   # export -> off-box, then verify it
#   bash deploy/nessie-backup.sh <instance> check    # the alarm: exit 3 stale/absent, 4 corrupt
#   bash deploy/nessie-backup.sh <instance> drill    # restore into a SCRATCH Nessie, read every table
#
# ── HOW THE EXPORT IS TAKEN ─────────────────────────────────────────────────
# `nessie-server-admin export` opens the version store itself, and RocksDB takes
# an exclusive lock, so Nessie is STOPPED for the minute the export takes and
# restarted (and waited on until it answers) by a trap that runs however this
# script ends. That is a deliberate trade: a copy of live RocksDB files is not a
# backup, and the only consumer of the catalog is the nightly batch, which this
# job is ordered AFTER. The admin image is derived from the server image's tag,
# so the exporter can never be a different Nessie version from the data.
# The table INVENTORY (where each table's metadata is, its row count) is taken
# from the live catalog BEFORE the stop; it is what the drill holds a restore to.
#
# NOT YET EXERCISED AGAINST THE REAL nessie-server-admin IMAGE (the box that
# wrote this had no registry access): the `export --path` / `import --path`
# spelling follows the 0.104 docs. The first run on prod is the proof — do
# `drill` straight after, and do not trust the backup until it has passed.
#
# The backup bucket may live in a DIFFERENT AWS account from the lake (sp-77eatm), so
# the three modes that touch it (put, check, fetch) sign with a credential of their own:
# PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE (a shared-credentials file, mounted read-only)
# + PKDUMP_NESSIE_BACKUP_PROFILE. Those containers get NONE of the lake's identity, and
# the lake reads (inventory, drill) get none of the backup's. Unset, both use the lake's.
#
# Host config (lake.env): PKDUMP_NESSIE_BACKUP_URL=s3://<bucket>/<prefix> — NO
# default, and it must not be the lake bucket (the lake has no retention rule we
# would want applied to backups, and a lake-scoped credential must not be what
# can delete them).
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

INSTANCE="${1:?usage: nessie-backup.sh <instance> backup|check|drill}"
MODE="${2:?usage: nessie-backup.sh <instance> backup|check|drill}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"
NETWORK="${PKDUMP_LAKE_NETWORK:-pkdump-lake-${INSTANCE}}"
JOB_IMAGE="${PKDUMP_LAKE_JOB_IMAGE:-localhost/pkdump-lake:${INSTANCE}}"
QUADLET="${PKDUMP_NESSIE_QUADLET:-${HOME}/.config/containers/systemd/pkdump-nessie-${INSTANCE}.container}"
UNIT="pkdump-nessie-${INSTANCE}.service"
MAX_AGE_HOURS="${PKDUMP_NESSIE_BACKUP_MAX_AGE_HOURS:-26}"
KEEP="${PKDUMP_NESSIE_BACKUP_KEEP:-14}"

die() { echo "nessie-backup: $*" >&2; exit 1; }

[ -f "$LAKE_ENV" ] || die "${LAKE_ENV} does not exist — this box has no lake configured (bash deploy/setup-lake.sh ${INSTANCE})"
# shellcheck disable=SC1090
{ set -a; . "$LAKE_ENV"; set +a; }
[ -f "$ALERTS_ENV" ] && { set -a; . "$ALERTS_ENV"; set +a; }

if [ -n "${PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE:-}" ]; then
    [ -n "${PKDUMP_NESSIE_BACKUP_PROFILE:-}" ] ||
        die "PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE is set without PKDUMP_NESSIE_BACKUP_PROFILE in ${LAKE_ENV}"
    [ -r "$PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE" ] ||
        die "PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE ${PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE} is not a readable file"
fi
[ -n "${PKDUMP_NESSIE_BACKUP_URL:-}" ] ||
    die "PKDUMP_NESSIE_BACKUP_URL is not set in ${LAKE_ENV} — name the backup bucket (s3://bucket/prefix); there is no default"
BACKUP_BUCKET="${PKDUMP_NESSIE_BACKUP_URL#s3://}"; BACKUP_BUCKET="${BACKUP_BUCKET%%/*}"
if [ -n "${PKDUMP_LAKE_S3_BUCKET:-}" ] && [ "$BACKUP_BUCKET" = "$PKDUMP_LAKE_S3_BUCKET" ]; then
    die "PKDUMP_NESSIE_BACKUP_URL is in the lake bucket (${BACKUP_BUCKET}); backups belong on the backup side"
fi

# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate
# shellcheck source=deploy/lake-lib.sh
. "$SCRIPT_DIR/lake-lib.sh"

podman image exists "$JOB_IMAGE" 2>/dev/null || die "no lake job image ${JOB_IMAGE} (bash deploy/setup-lake.sh ${INSTANCE})"
podman network exists "$NETWORK" 2>/dev/null || die "no lake network ${NETWORK} (bash deploy/setup-lake.sh ${INSTANCE})"
[ -f "$QUADLET" ] || die "no Nessie unit at ${QUADLET} (bash deploy/setup-lake.sh ${INSTANCE})"

CATALOG_URI="$(pkdump_lake_catalog_uri "$INSTANCE")"
CATALOG_HEALTH_URL="$(pkdump_lake_health_url "$CATALOG_URI")"

# --- the server's own configuration, read from its unit ---------------------
# The exporter and the scratch server must see exactly the version-store and
# warehouse settings the real one does; reading them out of the installed
# Quadlet means they cannot drift from it.
SERVER_IMAGE="$(sed -n 's/^Image=//p' "$QUADLET" | head -1)"
case "$SERVER_IMAGE" in
*/nessie:*) ADMIN_IMAGE="${PKDUMP_NESSIE_ADMIN_IMAGE:-${SERVER_IMAGE/\/nessie:/\/nessie-server-admin:}}" ;;
*) die "cannot derive the admin image from server image '${SERVER_IMAGE}'" ;;
esac
NESSIE_VERSION="${SERVER_IMAGE##*:}"
SERVER_ARGS=()
while IFS= read -r line; do
    line="${line//%h/$HOME}"
    case "$line" in
    Environment=*) SERVER_ARGS+=(-e "${line#Environment=}") ;;
    EnvironmentFile=*) SERVER_ARGS+=(--env-file "${line#EnvironmentFile=}") ;;
    Secret=*) SERVER_ARGS+=(--secret "${line#Secret=}") ;;
    Volume=*:/nessie:*) ;; # the data dir: each mode names its own
    Volume=*) SERVER_ARGS+=(-v "${line#Volume=}") ;;
    esac
done <"$QUADLET"

# --- the lake job container (PyIceberg + the backup module) ------------------
ENV_ARGS=(-e "PKDUMP_LAKE_NESSIE_URI=${CATALOG_URI}" -e "PKDUMP_NESSIE_BACKUP_URL=${PKDUMP_NESSIE_BACKUP_URL}")
for VAR in PKDUMP_LAKE_REF PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_S3_PREFIX \
    PKDUMP_LAKE_S3_ENDPOINT PKDUMP_LAKE_S3_ACCESS_KEY_ID PKDUMP_LAKE_S3_SECRET_ACCESS_KEY \
    PKDUMP_LAKE_S3_PATH_STYLE AWS_PROFILE PKDUMP_LAKE_S3_ROLE_ARN PKDUMP_LAKE_S3_BASE_PROFILE \
    PKDUMP_NESSIE_BACKUP_S3_ENDPOINT AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_DEFAULT_REGION; do
    [ -n "${!VAR:-}" ] && ENV_ARGS+=(-e "${VAR}=${!VAR}")
done
# The Rust lake jobs (kit sp-3yxvq1) assume from PKDUMP_LAKE_S3_PROFILE and ignore
# the BASE_PROFILE the Python jobs read: without it they sign with the ROLE profile
# AWS_PROFILE names and fail with "failed to load signing credential" (sp-090v9u).
# Same profile, same lake.env source; an explicit PKDUMP_LAKE_S3_PROFILE wins.
if [ -n "${PKDUMP_LAKE_S3_BASE_PROFILE:-}" ] && [ -z "${PKDUMP_LAKE_S3_PROFILE:-}" ]; then
    ENV_ARGS+=(-e "PKDUMP_LAKE_S3_PROFILE=${PKDUMP_LAKE_S3_BASE_PROFILE}")
fi
CRED_ARGS=()
if [ -f "${CONF_DIR}/aws/config" ] && podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1; then
    CRED_ARGS=(-v "${CONF_DIR}/aws/config:/aws/config:ro"
        --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials"
        -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials)
fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pkdump-nessie-backup.XXXXXX")"
lake_job() { # lake_job <network-uri-override|-> args...
    local uri="$1"; shift
    local env=("${ENV_ARGS[@]}")
    [ "$uri" != - ] && env+=(-e "PKDUMP_LAKE_NESSIE_URI=${uri}")
    podman run --rm --pull=never --network "$NETWORK" "${env[@]}" \
        ${CRED_ARGS[@]+"${CRED_ARGS[@]}"} -v "${WORK}:/work:Z" "$JOB_IMAGE" "$@"
}
# backup_job: the store-facing modes, signed ONLY by the backup credential when one is named.
backup_job() {
    if [ -z "${PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE:-}" ]; then
        lake_job - "$@"
        return
    fi
    local env=(-e "PKDUMP_NESSIE_BACKUP_URL=${PKDUMP_NESSIE_BACKUP_URL}"
        -e AWS_SHARED_CREDENTIALS_FILE=/backup-aws/credentials
        -e "AWS_PROFILE=${PKDUMP_NESSIE_BACKUP_PROFILE}")
    local var
    for var in PKDUMP_NESSIE_BACKUP_S3_ENDPOINT AWS_REGION AWS_DEFAULT_REGION; do
        [ -n "${!var:-}" ] && env+=(-e "${var}=${!var}")
    done
    podman run --rm --pull=never --network "$NETWORK" "${env[@]}" \
        -v "${PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE}:/backup-aws/credentials:ro,Z" \
        -v "${WORK}:/work:Z" "$JOB_IMAGE" "$@"
}
now_utc() { date -u +%Y-%m-%dT%H:%M:%SZ; }

wait_answering() { # wait_answering <health-url> — 120s, failing rather than proceeding
    local i
    for i in $(seq 1 60); do
        pkdump_lake_catalog_answering "$NETWORK" "$JOB_IMAGE" "$1" && return 0
        sleep 2
    done
    return 1
}

# Nessie's data dir, from the unit's own Volume= line.
DATA_DIR="$(sed -n 's/^Volume=\(.*\):\/nessie:.*/\1/p' "$QUADLET" | head -1)"
DATA_DIR="${DATA_DIR//%h/$HOME}"
[ -n "$DATA_DIR" ] || die "no Volume=…:/nessie in ${QUADLET}"

NESSIE_STOPPED=0
SCRATCH=""
cleanup() {
    local rc=$?
    [ -n "$SCRATCH" ] && podman rm -f "$SCRATCH" >/dev/null 2>&1 || true
    if [ "$NESSIE_STOPPED" = 1 ]; then
        echo "nessie-backup: restarting ${UNIT}" >&2
        systemctl --user start "$UNIT" || rc=1
        wait_answering "$CATALOG_HEALTH_URL" || { echo "nessie-backup: ${UNIT} did not answer after restart" >&2; rc=1; }
    fi
    rm -rf "$WORK" 2>/dev/null || podman unshare rm -rf "$WORK" 2>/dev/null || true
    exit "$rc"
}
trap cleanup EXIT

alarm() { # alarm <title> <body>
    "${SCRIPT_DIR}/alert.sh" "$1" "$2" || echo "nessie-backup: the alert reached nobody (no Pushover channel configured)" >&2
}

do_check() {
    local rc=0
    backup_job pkdump-lake-nessie-backup check --now "$(now_utc)" --max-age-hours "$MAX_AGE_HOURS" || rc=$?
    case "$rc" in
    0) return 0 ;;
    3) alarm "PokeDumpster Nessie backup is STALE (${INSTANCE})" "No verified Nessie catalog backup newer than ${MAX_AGE_HOURS}h. Losing the catalog disk now strands every lake table. See journalctl --user -u pkdump-nessie-backup@${INSTANCE}.service" ;;
    4) alarm "PokeDumpster Nessie backup is CORRUPT (${INSTANCE})" "The newest Nessie backup does not verify against its manifest. See journalctl --user -u pkdump-nessie-backup-check@${INSTANCE}.service" ;;
    *) alarm "PokeDumpster Nessie backup check could not run (${INSTANCE})" "Not being able to ask is not 'fine'. See the journal of pkdump-nessie-backup-check@${INSTANCE}.service" ;;
    esac
    return "$rc"
}

case "$MODE" in
check) do_check ;;

backup)
    ID="$(date -u +%Y%m%dT%H%M%SZ)"
    echo "==> nessie-backup ${INSTANCE}: ${ID} (nessie ${NESSIE_VERSION})"
    # 1. Inventory the live catalog while it is up.
    lake_job - pkdump-lake-nessie-backup inventory --out /work/inventory.json
    # 2. Stop Nessie; the trap restarts it however this ends.
    systemctl --user stop "$UNIT"
    NESSIE_STOPPED=1
    # 3. Export. --user 0 is root IN the user namespace, i.e. the invoking user
    # on the host, so the file is readable here; the unit's `U` re-chowns the
    # data dir to Nessie's mapped user on the next start.
    podman run --rm --user 0 --network none "${SERVER_ARGS[@]}" \
        -v "${DATA_DIR}:/nessie:Z" -v "${WORK}:/out:Z" "$ADMIN_IMAGE" \
        export --path /out/export.zip
    systemctl --user start "$UNIT"
    NESSIE_STOPPED=0
    wait_answering "$CATALOG_HEALTH_URL" || die "${UNIT} did not answer after the export — it is running but unverified"
    # 4. Verify and ship it, then re-verify from the bucket.
    backup_job pkdump-lake-nessie-backup put --export /work/export.zip --inventory /work/inventory.json \
        --backup-id "$ID" --nessie-version "$NESSIE_VERSION" --keep "$KEEP"
    do_check
    ;;

drill)
    echo "==> nessie-backup drill ${INSTANCE}: restore the newest backup into a scratch Nessie"
    backup_job pkdump-lake-nessie-backup fetch --out-dir /work/fetched
    mkdir -p "${WORK}/rocks"
    # Import into an EMPTY scratch version store, never the live one.
    podman run --rm --user 0 --network none "${SERVER_ARGS[@]}" \
        -v "${WORK}/rocks:/nessie:Z" -v "${WORK}/fetched:/in:Z" "$ADMIN_IMAGE" \
        import --path /in/export.zip
    SCRATCH="pkdump-nessie-drill-${INSTANCE}-$$"
    podman run -d --name "$SCRATCH" --network "$NETWORK" --user 0 --memory=1g --memory-swap=1g \
        "${SERVER_ARGS[@]}" -v "${WORK}/rocks:/nessie:Z" "$SERVER_IMAGE" >/dev/null
    SCRATCH_URI="http://${SCRATCH}:19120/iceberg/"
    wait_answering "$(pkdump_lake_health_url "$SCRATCH_URI")" || { podman logs "$SCRATCH" 2>&1 | tail -20 >&2; die "scratch Nessie never answered"; }
    NS_ARGS=()
    for ns in ${PKDUMP_NESSIE_DRILL_NAMESPACES:-catalog}; do NS_ARGS+=(--require-namespace "$ns"); done
    # Exit 5 if ANY table cannot be read back through the restored catalog.
    lake_job "$SCRATCH_URI" pkdump-lake-nessie-backup drill --manifest /work/fetched/manifest.json "${NS_ARGS[@]}"
    ;;

*) die "unknown mode '${MODE}' (backup|check|drill)" ;;
esac
