#!/usr/bin/env bash
#
# Disaster recovery: restore ONE database — a tenant's collection, or the user
# registry (--registry) — from its Litestream S3 replica (pokedumpster-8ch.5,
# multi-tenant since pd-fof4, registry since pd-nd6w).
#
# ── RUNBOOK ─────────────────────────────────────────────────────────────────
# WHEN TO USE
#   A tenant's collection DB on the data volume is lost or corrupt and you want
#   to recover it from the off-box S3 replica written by the Litestream sidecar
#   (deploy/pkdump-litestream.container). This is the only backup now; the local
#   snapshot scripts were removed (S3-only). See deploy/RESTORE.md.
#
# WHAT IT DOES
#   1. Stops the app service + the Litestream sidecar (so nothing writes/replicates).
#   2. Runs `litestream restore` from the tenant's S3 replica onto the data
#      volume (temp-then-rename).
#   3. Restarts the app + sidecar.
#   4. Verifies the restored row count.
#   5. Verifies both services actually came back — a restored collection that is
#      no longer replicating is a half-finished recovery, and it fails silently.
#
#   Only the named tenant is touched. Every other tenant keeps replicating from
#   its own prefix and its files are not read or written here.
#
#   The shared catalog (shared.sqlite) is NOT restored — it is reproducible:
#   rebuild it with `deploy/seed.sh <instance>`.
#
# ADDRESSING
#   The sidecar runs in Litestream's directory mode, so deploy/litestream.yml
#   names no database and `restore -config` cannot resolve one. The tenant's
#   replica is addressed by URL, derived by deploy/litestream-lib.sh from the
#   same LITESTREAM_S3_PATH the sidecar replicates under. The derivation is
#   asserted against a real bucket in tests/litestream/run.sh.
#
# CREDENTIALS
#   Assume-role profile in ~/.config/pkdump/<instance>/aws/config + the bootstrap
#   key from podman secret pkdump-<instance>-s3-bootstrap (same as the sidecar) —
#   auto-refreshing temporary credentials via role assumption, never static keys.
#
# THE REGISTRY (--registry, pd-nd6w)
#   `--registry` restores the user registry (handle -> database_id) instead of a
#   tenant. It is the same operation against a different file and a different
#   prefix, deliberately in this script rather than a second one: the two are
#   never independent. In a total loss the registry is restored FIRST, because
#   it is what says which database belongs to whom — restore the tenants first
#   and you have a directory of opaque ids and no way to attribute them.
#   See deploy/RESTORE.md, scenario C.
#
# THE ATTRIBUTION GATE (pd-7f46)
#   "Restore the registry FIRST" was documented and drilled, and nothing
#   enforced it. Restoring the tenant databases on their own SUCCEEDS: every
#   byte comes back, `integrity_check` says ok, and what you are holding is a
#   directory of files called 01K2C7HQ8N….sqlite with nobody's name on any of
#   them. It reads as a completed recovery. That is the same shape as the
#   failure this whole epic is scar tissue from — every backup-shaped signal
#   green while recovery was impossible (pd-1717) — so it is a FAILURE here:
#
#     restoring a database named by an opaque database_id requires the volume's
#     registry to name that id.
#
#   Absent registry, or a registry silent about the id, and the restore refuses
#   BEFORE it stops a service or writes a byte, naming `--registry` as the
#   thing to do first. A handle-named database from before the migration is
#   exempt: its filename is its attribution.
#
#   `--unattributed` is the way past it, for the one case that is legitimately
#   on the far side: recovering a PURGED database, whose registry row is gone
#   by design (RESTORE.md scenario B2). It has to be typed, and it says in the
#   output that what came back has no owner on this box.
#
# USAGE
#   bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] [--unattributed] <instance> [tenant|database-id]
#   bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] --registry <instance>
#     --yes, -y        skip the confirmation prompt (scripted use)
#     --at=<time>      point-in-time restore, e.g. --at=2026-06-01T12:00:00Z
#                      (default: latest; within the 6-month retention window)
#     --registry       restore the user registry rather than a tenant database
#     --unattributed   restore a database the registry does not name (a purged
#                      one). Without it, that is an error — see above.
#   Examples:
#     bash deploy/restore-litestream.sh prod
#     bash deploy/restore-litestream.sh prod alice
#     bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod alice
#     bash deploy/restore-litestream.sh prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC
#     bash deploy/restore-litestream.sh --registry prod
#     bash deploy/restore-litestream.sh --unattributed prod <a purged database id>
# ────────────────────────────────────────────────────────────────────────────
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

YES=false
AT=""
REGISTRY=false
UNATTRIBUTED=false
POSITIONAL=()
for arg in "$@"; do
    case $arg in
        --yes|-y) YES=true ;;
        --at=*) AT="${arg#--at=}" ;;
        --registry) REGISTRY=true ;;
        --unattributed) UNATTRIBUTED=true ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] [--unattributed] <instance> [tenant|database-id]"
    echo "       bash deploy/restore-litestream.sh [--yes] [--at=<RFC3339>] --registry <instance>"
    exit 1
fi

INSTANCE="${POSITIONAL[0]}"
TENANT="${POSITIONAL[1]:-collection}"
# A tenant name alongside --registry is not a narrower request, it is a
# contradiction — and silently ignoring it would restore something the operator
# did not ask for. There is exactly one registry.
if [ "$REGISTRY" = true ] && [ ${#POSITIONAL[@]} -gt 1 ]; then
    echo "ERROR: --registry restores the registry; it takes no tenant name (got '${POSITIONAL[1]}')."
    exit 1
fi
# The registry is what attributes everything else; there is nothing to attribute
# it against, and no gate for --unattributed to open.
if [ "$REGISTRY" = true ] && [ "$UNATTRIBUTED" = true ]; then
    echo "ERROR: --unattributed applies to a tenant database, not to the registry itself."
    exit 1
fi
SERVICE_NAME="pkdump-${INSTANCE}"
SIDECAR="pkdump-litestream-${INSTANCE}"
VOLUME="pkdump-${INSTANCE}-data"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LS_IMG="docker.io/litestream/litestream:latest"

# shellcheck source=deploy/litestream-lib.sh
. "${SCRIPT_DIR}/litestream-lib.sh"

echo "==> PokeDumpster Litestream restore"
if [ "$REGISTRY" = true ]; then
    echo "    Instance: ${INSTANCE}   Target: the user registry   Volume: ${VOLUME}"
else
    echo "    Instance: ${INSTANCE}   Tenant: ${TENANT}   Volume: ${VOLUME}"
fi
[ -n "$AT" ] && echo "    Point-in-time: ${AT}"

# Restore into the store the instance actually lives in (pd-fite). Prod's unit
# carries no store flags, so this is a no-op for prod.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

# --- Validate --------------------------------------------------------------
command -v sqlite3 >/dev/null 2>&1 || { echo "ERROR: sqlite3 not found on host. Install: sudo apt install sqlite3"; exit 1; }
[ -f "${CONF_DIR}/litestream.env" ] || { echo "ERROR: ${CONF_DIR}/litestream.env not found — is the sidecar configured?"; exit 1; }
[ -f "${CONF_DIR}/aws/config" ] || { echo "ERROR: ${CONF_DIR}/aws/config not found — is the sidecar configured?"; exit 1; }
podman secret inspect "pkdump-${INSTANCE}-s3-bootstrap" >/dev/null 2>&1 || { echo "ERROR: podman secret 'pkdump-${INSTANCE}-s3-bootstrap' not found."; exit 1; }
podman volume exists "$VOLUME" 2>/dev/null || { echo "ERROR: data volume '${VOLUME}' not found."; exit 1; }

# Read before anything is stopped: the attribution gate below needs the volume's
# registry, and a check that runs after the app has been taken down is a check
# that costs an outage to fail.
MOUNTPOINT="$(podman volume inspect -f '{{.Mountpoint}}' "$VOLUME")"

# S3 target comes from the instance's env file (bucket / path / region / AWS_PROFILE).
set -a; . "${CONF_DIR}/litestream.env"; set +a

# The one file this restore writes, and the one replica URL it reads. Both come
# out of litestream-lib.sh so they cannot drift from what the sidecar wrote —
# and for a tenant that derivation also REJECTS a malformed stem before it can
# retarget the S3 prefix. The positional argument is the database's FILENAME
# STEM, not the person: a 26-character uppercase database id for anything minted
# since pd-zr9n, or the old handle for a database that predates it. Both live
# under tenants/ during the migration, so both are accepted.
#
# RELPATH is relative to the data volume root: the registry sits at the root,
# tenants one level down under tenants/. That difference is the whole difference
# between the two modes.
if [ "$REGISTRY" = true ]; then
    REPLICA_URL="$(registry_replica_url)"
    RELPATH="registry.sqlite"
    BLAST="no tenant database is read or written"
else
    REPLICA_URL="$(tenant_replica_url "$TENANT")"
    RELPATH="tenants/${TENANT}.sqlite"
    BLAST="no other tenant is read or written"
fi

# --- The attribution gate (pd-7f46) ----------------------------------------
# Restoring a database named by an opaque id, with no registry to say whose it
# is, is not a recovery — it is a file. See the header. The gate runs here, on
# purpose: before the confirmation prompt, and long before anything is stopped.
#
# Only for opaque ids. `validate_database_id` failing means the stem is a
# handle-named database from before `pkdump tenant migrate`, and its filename
# already answers the question the registry would.
REGISTRY_FILE="${MOUNTPOINT}/registry.sqlite"
if [ "$REGISTRY" = false ] && validate_database_id "$TENANT" 2>/dev/null; then
    # Read-WRITE open, deliberately: the registry is a WAL database, and a
    # `mode=ro` connection cannot create the -shm it needs when one is not
    # already there. The statement is a SELECT; the timeout is there because
    # the sidecar and the app are both still running at this point.
    # sqlite3's own errors are NOT swallowed: a registry that is present and
    # unreadable is a third outcome, and under `set -e` a silenced failure here
    # would kill the script with no message at all — the one way a gate against
    # silent failure must not fail.
    registry_says() {
        sqlite3 -cmd '.timeout 5000' "$REGISTRY_FILE" \
            "SELECT handle || ' (' || state || ')' FROM user WHERE database_id = '${TENANT}';"
    }
    refuse() {
        echo ""
        echo "ERROR: refusing to restore ${TENANT} — nothing on this box says whose it is."
        echo "       $1"
        echo ""
        echo "       A collection is stored under an opaque database_id. Restored without"
        echo "       the registry it comes back complete, healthy, and anonymous — a file"
        echo "       called ${TENANT}.sqlite that no handle resolves to. That is not a"
        echo "       finished recovery, and it looks exactly like one."
        echo ""
        echo "       Restore the registry FIRST, then re-run this command:"
        echo "         bash deploy/restore-litestream.sh --registry ${INSTANCE}"
        echo ""
        echo "       If the database is one you PURGED, its row is gone by design and"
        echo "       there is nothing to wait for (deploy/RESTORE.md, scenario B2):"
        echo "         bash deploy/restore-litestream.sh --unattributed ${INSTANCE} ${TENANT}"
        exit 1
    }

    if [ "$UNATTRIBUTED" = true ]; then
        echo "    Attribution: NOT CHECKED (--unattributed). What comes back has no owner"
        echo "                 on this box; ${RELPATH} will show up in \`pkdump tenant list\`"
        echo "                 as a database no registered user claims until you say whose."
    elif [ ! -e "$REGISTRY_FILE" ]; then
        refuse "There is no registry.sqlite on '${VOLUME}'."
    else
        OWNER="$(registry_says)" \
            || refuse "registry.sqlite on '${VOLUME}' is there but could not be read (above)."
        [ -n "$OWNER" ] || refuse "registry.sqlite on '${VOLUME}' has no row for that database id."
        echo "    Attribution: ${TENANT} belongs to ${OWNER}"
    fi
fi

# --- Confirm ---------------------------------------------------------------
if [ "$YES" = false ]; then
    echo ""
    echo "WARNING: overwrites ${RELPATH} on '${VOLUME}' from"
    echo "         ${REPLICA_URL%%\?*}"
    echo "         (${BLAST})"
    read -r -p "Continue? [y/N] " response
    [[ "$response" =~ ^[Yy]$ ]] || { echo "Restore cancelled."; exit 0; }
fi

# --- Stop writers ----------------------------------------------------------
echo "==> Stopping ${SERVICE_NAME} + ${SIDECAR}..."
systemctl --user stop "$SERVICE_NAME" 2>/dev/null || true
systemctl --user stop "${SIDECAR}.service" 2>/dev/null || true
sleep 2

# Tenant collection DBs live under tenants/ (deploy/TENANTS.md). Restoring onto
# a bare volume — the disaster case — reaches a data dir that has no tenants/
# yet, and litestream will not create the parent of its -o target. (The registry
# is at the root, whose parent is the volume itself, so this is a no-op for it.)
mkdir -p "${MOUNTPOINT}/$(dirname "$RELPATH")"
DEST="${MOUNTPOINT}/${RELPATH}"
TMP="${DEST}.restore-tmp"

# --- Restore from S3 -------------------------------------------------------
# litestream restore refuses to overwrite an existing file, so restore to a temp
# path on the volume, then atomically rename into place.
echo "==> Restoring ${RELPATH} from S3 via litestream..."
rm -f "$TMP"
RESTORE_FLAGS=(restore -integrity-check full)
[ -n "$AT" ] && RESTORE_FLAGS+=(-timestamp "$AT")     # point-in-time restore
RESTORE_FLAGS+=(-o "/data/${RELPATH}.restore-tmp" "$REPLICA_URL")
podman run --rm --user 0:0 \
    -v "${VOLUME}:/data" \
    -v "${CONF_DIR}/aws/config:/aws/config:ro" \
    --secret "pkdump-${INSTANCE}-s3-bootstrap,type=mount,target=/aws/credentials" \
    -e AWS_CONFIG_FILE=/aws/config \
    -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
    -e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
    "$LS_IMG" "${RESTORE_FLAGS[@]}"

# --- Atomic swap -----------------------------------------------------------
rm -f "${DEST}-wal" "${DEST}-shm"
mv -f "$TMP" "$DEST"

# --- Restart ---------------------------------------------------------------
# reset-failed before start: the sidecar unit is rate-limited (StartLimitBurst=5
# per 300s) so a crash-loop pages instead of thrashing. Restoring several tenants
# in a row — exactly what a real incident looks like — trips that limit, and then
# `start` fails. Observed in the pd-v8zf DR drill.
echo "==> Starting ${SERVICE_NAME} (+ sidecar)..."
systemctl --user reset-failed "$SERVICE_NAME" "${SIDECAR}.service" 2>/dev/null || true
systemctl --user start "$SERVICE_NAME" 2>/dev/null || true
systemctl --user start "${SIDECAR}.service" 2>/dev/null || true

# --- Verify ----------------------------------------------------------------
if [ "$REGISTRY" = true ]; then
    ROWS="$(sqlite3 "file:${DEST}?mode=ro" "SELECT count(*) FROM user WHERE state = 'active';" 2>&1 || echo '?')"
    echo "==> Restore complete — ${RELPATH} has ${ROWS} active user(s)."
    echo "    Their databases: sqlite3 \"file:${DEST}?mode=ro\" \\"
    echo "        \"SELECT handle, database_id FROM user WHERE state='active';\""
    echo "    Restore each one:  bash deploy/restore-litestream.sh ${INSTANCE} <database_id>"
else
    ROWS="$(sqlite3 "file:${DEST}?mode=ro" 'SELECT count(*) FROM collection;' 2>&1 || echo '?')"
    echo "==> Restore complete — ${RELPATH} has ${ROWS} collection rows."
fi
echo "    Shared catalog not restored (reproducible): bash deploy/seed.sh ${INSTANCE}"

# A restored collection that is no longer being backed up is a half-finished
# recovery, and the failure is silent: replication stopping produces no error
# anywhere and Layer 1 would not notice for 36 hours. So the services this script
# stopped are checked, not merely started.
DOWN=()
for UNIT in "$SERVICE_NAME" "${SIDECAR}.service"; do
    # Units that are not installed on this box (a bare instance with no app, a
    # box with backups deliberately unconfigured) are not "down".
    systemctl --user cat "$UNIT" >/dev/null 2>&1 || continue
    # A few seconds of grace: the app container pulls itself up behind systemd.
    for _ in $(seq 10); do
        [ "$(systemctl --user is-active "$UNIT")" = active ] && break
        sleep 1
    done
    [ "$(systemctl --user is-active "$UNIT")" = active ] || DOWN+=("$UNIT")
done
if [ ${#DOWN[@]} -gt 0 ]; then
    echo ""
    echo "ERROR: the restore succeeded but these units did NOT come back up:"
    printf '         %s\n' "${DOWN[@]}"
    echo "       Backups are OFF until the sidecar runs. Diagnose and start it:"
    echo "         systemctl --user status ${SIDECAR}.service"
    echo "         systemctl --user reset-failed ${SIDECAR}.service && systemctl --user start ${SIDECAR}.service"
    exit 1
fi
