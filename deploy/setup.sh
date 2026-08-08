#!/usr/bin/env bash
#
# Set up a PokeDumpster container instance (rootless Podman). No sudo.
# Run from within the repo clone.
#
# Usage:
#   bash deploy/setup.sh <instance> [port] [--init] [--test]
#
# Examples:
#   bash deploy/setup.sh prod 8080         # explicit host port
#   bash deploy/setup.sh feature-xyz       # auto-assigned host port
#   bash deploy/setup.sh ci --test         # seed data volume from committed
#                                          #   fixture (no network, ~seconds)
#   bash deploy/setup.sh demo --init       # clone the pre-built seed volume
#
# Modes:
#   (plain)  Build the image + install the Quadlet. Catalog stays empty —
#            populate it later with deploy/seed.sh.
#   --test   Same, plus seed the new data volume from the committed fixture
#            tests/ui/fixtures/{shared.sqlite,collection.sqlite}. Fully
#            offline — copies the files in via a one-off container.
#   --init   Same, plus clone the reusable `pkdump-seed-data` volume (built
#            once with `deploy/seed.sh --volume`). Falls back to a full
#            `pkdump setup` if no seed volume exists.
#
# After plain setup, populate the catalog with deploy/seed.sh and start the
# service with: systemctl --user start pkdump-<instance>
#
set -euo pipefail

# systemctl --user needs XDG_RUNTIME_DIR; non-interactive shells often lack it.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

# --- Parse arguments --------------------------------------------------------

INIT=false
TEST=false
POSITIONAL=()
for arg in "$@"; do
    case $arg in
        --init) INIT=true ;;
        --test) TEST=true ;;
        *) POSITIONAL+=("$arg") ;;
    esac
done

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: bash deploy/setup.sh <instance> [port] [--init] [--test]"
    exit 1
fi

INSTANCE="${POSITIONAL[0]}"
PORT="${POSITIONAL[1]:-0}"
SERVICE_NAME="pkdump-${INSTANCE}"
VOLUME="pkdump-${INSTANCE}-data"
IMAGE="localhost/pkdump:${INSTANCE}"
SEED_VOLUME="pkdump-seed-data"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
QUADLET_DIR="$HOME/.config/containers/systemd"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"

if ! command -v podman >/dev/null 2>&1; then
    echo "ERROR: podman not found. Install it: sudo apt install podman"
    exit 1
fi

if ! loginctl show-user "$USER" -p Linger 2>/dev/null | grep -q "Linger=yes"; then
    echo "WARNING: linger not enabled — services stop when you log out."
    echo "  Fix: loginctl enable-linger $USER"
fi

# --- Build the image --------------------------------------------------------

echo "==> Building image pkdump:${INSTANCE}..."
podman build -t pkdump:latest -f "$REPO_DIR/Containerfile" "$REPO_DIR"
podman tag pkdump:latest "pkdump:${INSTANCE}"

# --- Install the Quadlet unit ----------------------------------------------

echo "==> Installing Quadlet unit..."
mkdir -p "$QUADLET_DIR"
# PORT=0 -> ":8080" lets Podman pick a free host port.
if [ "$PORT" = "0" ]; then
    PORT_MAPPING=":8080"
else
    PORT_MAPPING="${PORT}:8080"
fi
sed \
    -e "s|{{INSTANCE}}|${INSTANCE}|g" \
    -e "s|{{PORT}}:8080|${PORT_MAPPING}|g" \
    "$REPO_DIR/deploy/pkdump.container" \
    > "${QUADLET_DIR}/${SERVICE_NAME}.container"

# --- Install per-instance timer units --------------------------------------
# %i-templated units installed under a concrete instance name so several
# instances can run side by side. Enable them after setup if desired.

mkdir -p "$SYSTEMD_USER_DIR"
# Substitute {{REPO_DIR}} with this checkout so ExecStart resolves
# (single-clone deployment — the clone is not named per-instance).
for EXT in service timer; do
    sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
        "$REPO_DIR/deploy/pkdump-refresh.${EXT}" \
        > "${SYSTEMD_USER_DIR}/pkdump-refresh@.${EXT}"
done

# --- Install the backup-failure alarming units (pokedumpster-ivq) -----------
# Layer 1: backup-freshness dead-man's switch (per-instance @ template).
# Layer 2: OnFailure -> Pushover journal-tail push (instance-by-failed-unit).
# Layer 4: host-wide low-disk check (not per-instance).
echo "==> Installing backup-failure alarming units..."
for EXT in service timer; do
    sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
        "$REPO_DIR/deploy/pkdump-backup-check.${EXT}" \
        > "${SYSTEMD_USER_DIR}/pkdump-backup-check@.${EXT}"
done
sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
    "$REPO_DIR/deploy/pkdump-alert@.service" \
    > "${SYSTEMD_USER_DIR}/pkdump-alert@.service"
for EXT in service timer; do
    sed -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
        "$REPO_DIR/deploy/pkdump-diskcheck.${EXT}" \
        > "${SYSTEMD_USER_DIR}/pkdump-diskcheck.${EXT}"
done

# Scaffold the host-wide alert config (Pushover creds + disk threshold). Secrets
# NEVER live in the repo; this only writes a template and never clobbers it.
ALERTS_ENV="${HOME}/.config/pkdump/alerts.env"
if [ ! -f "$ALERTS_ENV" ]; then
    mkdir -p "${HOME}/.config/pkdump"
    cat > "$ALERTS_ENV" <<'EOF'
# Host-wide alert config for PokeDumpster (pokedumpster-ivq). Fill CHANGE_ME.
# Pushover push channel (Layers 2 + 4, and Layer 1's fast detail push).
# Create an app at https://pushover.net/apps/build for the token.
PUSHOVER_TOKEN=CHANGE_ME
PUSHOVER_USER=CHANGE_ME
# Low-disk alert (Layer 4): percent-used trigger + filesystem to watch.
PKDUMP_DISK_THRESHOLD=90
PKDUMP_DISK_PATH=
EOF
    chmod 600 "$ALERTS_ENV"
    echo "    Wrote ${ALERTS_ENV} (fill PUSHOVER_TOKEN/USER)."
fi

# --- Install the Litestream backup sidecar (pokedumpster-8ch.3) -------------
# Quadlet sidecar that continuously replicates the collection DB to S3. Instance
# + repo path are sed-substituted (single-clone deployment).
echo "==> Installing Litestream backup sidecar..."
sed -e "s|{{INSTANCE}}|${INSTANCE}|g" \
    -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
    "$REPO_DIR/deploy/pkdump-litestream.container" \
    > "${QUADLET_DIR}/pkdump-litestream-${INSTANCE}.container"

# Scaffold the per-instance config (S3 target + AWS creds). Secrets NEVER live in
# the repo; this only writes a template and never clobbers existing config.
LS_CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
mkdir -p "${LS_CONF_DIR}/aws"
chmod 700 "${LS_CONF_DIR}" "${LS_CONF_DIR}/aws"
if [ ! -f "${LS_CONF_DIR}/litestream.env" ]; then
    cat > "${LS_CONF_DIR}/litestream.env" <<EOF
# Litestream S3 target + AWS profile for instance '${INSTANCE}'. Fill CHANGE_ME.
LITESTREAM_S3_BUCKET=CHANGE_ME
LITESTREAM_S3_REGION=us-west-2
# The sidecar replicates EVERY tenants/*.sqlite and derives each tenant's prefix
# from its filename, so this is the parent prefix, not one database's:
#   <LITESTREAM_S3_PATH>/<tenant>.sqlite   (see deploy/litestream.yml)
LITESTREAM_S3_PATH=${INSTANCE}/tenants
LITESTREAM_TENANTS_DIR=/data/tenants
# The user registry (handle -> database_id) is replicated by the same sidecar as
# ITSELF, not as a tenant: it lives at the data root, so the glob above cannot
# see it. A \`path:\` replica prefix is used verbatim (the file name is NOT
# appended, unlike directory mode), so this names one database, .sqlite and all.
# It sits BESIDE <LITESTREAM_S3_PATH>, never inside it — the total-loss
# procedure lists that parent prefix to enumerate tenants.
LITESTREAM_REGISTRY_DB=/data/registry.sqlite
LITESTREAM_S3_REGISTRY_PATH=${INSTANCE}/registry.sqlite
# Empty = real S3, resolved from the pinned region. Only tests point this
# elsewhere (tests/litestream/run.sh at a throwaway MinIO).
LITESTREAM_S3_ENDPOINT=
AWS_PROFILE=pkdump
EOF
    chmod 600 "${LS_CONF_DIR}/litestream.env"
    # NOTE: aws/config + the bootstrap key are NOT created here. aws/config gates
    # the sidecar auto-start (so it won't crash-loop unconfigured), and the key is
    # the only secret — it lives in a podman secret, never a plaintext dotfile.
    echo "    Wrote ${LS_CONF_DIR}/litestream.env (fill CHANGE_ME)."
    echo "    To ENABLE backups (assume-role; the bootstrap key never touches the repo):"
    echo "      1. Fill litestream.env (bucket/path)."
    echo "      2. Create ${LS_CONF_DIR}/aws/config (role profile — gates auto-start):"
    echo "           [profile pkdump]"
    echo "           role_arn = arn:aws:iam::ACCOUNT:role/your-backup-role"
    echo "           source_profile = bootstrap"
    echo "           region = us-west-2"
    echo "      3. Store the bootstrap key in a podman secret (NOT a file):"
    echo "           printf '[bootstrap]\\naws_access_key_id=K\\naws_secret_access_key=S\\n' | podman secret create pkdump-${INSTANCE}-s3-bootstrap -"
    echo "    Sidecar auto-starts once aws/config exists + the secret is present."
fi

# Backfill keys an OLDER litestream.env predates (pd-nd6w: the registry entry).
# The block above deliberately never clobbers an operator's config, which is
# also why it cannot introduce a new key into one — and deploy/litestream.yml
# now references two. An unset db path is fatal to the sidecar at startup
# (measured: "must specify either 'path' or 'dir'"), so an un-backfilled
# instance does not silently drop the registry from the replicated set; it
# refuses to run. Re-running this script is the fix, so make it BE the fix.
#
# Append-only, and only for keys that are absent: nothing here rewrites a value
# the operator chose. The defaults match the template above.
if [ -f "${LS_CONF_DIR}/litestream.env" ]; then
    ADDED=()
    grep -q '^LITESTREAM_REGISTRY_DB=' "${LS_CONF_DIR}/litestream.env" || ADDED+=("LITESTREAM_REGISTRY_DB=/data/registry.sqlite")
    grep -q '^LITESTREAM_S3_REGISTRY_PATH=' "${LS_CONF_DIR}/litestream.env" || ADDED+=("LITESTREAM_S3_REGISTRY_PATH=${INSTANCE}/registry.sqlite")
    if [ ${#ADDED[@]} -gt 0 ]; then
        {
            echo ""
            echo "# Added by deploy/setup.sh (pd-nd6w): the user registry joins the replicated"
            echo "# set. It is replicated as itself, beside LITESTREAM_S3_PATH — not inside it."
            printf '%s\n' "${ADDED[@]}"
        } >> "${LS_CONF_DIR}/litestream.env"
        echo "==> Backfilled ${#ADDED[@]} registry key(s) into ${LS_CONF_DIR}/litestream.env:"
        printf '      %s\n' "${ADDED[@]}"
        echo "    Restart the sidecar to pick them up:"
        echo "      systemctl --user restart pkdump-litestream-${INSTANCE}.service"
    fi
fi

# Per-instance alert config: the healthchecks.io ping URL for THIS instance's
# backup-freshness dead-man's switch (pokedumpster-ivq.2). Empty = Layer 1 off.
if [ ! -f "${LS_CONF_DIR}/alerts.env" ]; then
    cat > "${LS_CONF_DIR}/alerts.env" <<EOF
# Per-instance backup-alarming config for '${INSTANCE}'. Empty PING_URL = Layer 1
# disabled. Create a check at https://healthchecks.io (period ~6h, grace ~3h),
# wire its Pushover integration, and paste its ping URL here.
PKDUMP_BACKUP_PING_URL=
# Alert when the newest S3 snapshot is older than this (hours; daily snapshots).
PKDUMP_BACKUP_MAX_AGE_HOURS=36
EOF
    chmod 600 "${LS_CONF_DIR}/alerts.env"
    echo "    Wrote ${LS_CONF_DIR}/alerts.env (paste the healthchecks.io ping URL to arm Layer 1)."
fi

systemctl --user daemon-reload

# --- Optional data volume seeding ------------------------------------------

seed_from_fixture() {
    # Copy the committed fixture sqlite files onto the instance's data volume
    # via a one-off container. No network — purely a local file copy.
    local fixture_dir="$REPO_DIR/tests/ui/fixtures"
    local shared="${fixture_dir}/shared.sqlite"
    local collection="${fixture_dir}/collection.sqlite"

    if [ ! -f "$shared" ] || [ ! -f "$collection" ]; then
        echo "ERROR: fixture files not found under ${fixture_dir}"
        echo "    Expected shared.sqlite and collection.sqlite."
        exit 1
    fi

    echo "==> Seeding ${VOLUME} from committed fixture (offline)..."
    podman volume exists "$VOLUME" 2>/dev/null || podman volume create "$VOLUME" >/dev/null

    local temp="pkdump-fixture-$$"
    podman run -d --name "$temp" \
        -v "${VOLUME}:/data:Z" \
        --entrypoint sleep \
        "$IMAGE" infinity >/dev/null
    # shellcheck disable=SC2064
    trap "podman rm -f '$temp' >/dev/null 2>&1 || true" RETURN

    # Tenant collection DBs live under /data/tenants (deploy/TENANTS.md); the
    # catalog stays at the root of the data dir, shared by every tenant.
    podman exec "$temp" mkdir -p /data/tenants
    podman cp "$shared"     "${temp}:/data/shared.sqlite"
    podman cp "$collection" "${temp}:/data/tenants/collection.sqlite"
    podman rm -f "$temp" >/dev/null
    trap - RETURN
    echo "    Fixture data installed."
}

seed_from_volume() {
    # Clone the reusable pkdump-seed-data volume, or fall back to a full
    # `pkdump setup` if it has not been built yet.
    if podman volume exists "$SEED_VOLUME" 2>/dev/null; then
        echo "==> Cloning seed volume into ${VOLUME}..."
        podman volume exists "$VOLUME" 2>/dev/null || podman volume create "$VOLUME" >/dev/null
        podman volume export "$SEED_VOLUME" | podman volume import "$VOLUME" -
        echo "    Done (cloned from ${SEED_VOLUME})."
    else
        echo "==> No seed volume found — running full 'pkdump setup' (slow)..."
        echo "    TIP: build a reusable seed volume once: bash deploy/seed.sh --volume"
        bash "$SCRIPT_DIR/seed.sh" "$INSTANCE"
    fi
}

if [ "$TEST" = true ]; then
    seed_from_fixture
elif [ "$INIT" = true ]; then
    seed_from_volume
fi

# --- Done -------------------------------------------------------------------

echo ""
echo "==> Setup complete."
if [ "$TEST" = true ] || [ "$INIT" = true ]; then
    echo "    Data volume seeded."
else
    echo "    Populate catalog:  bash deploy/seed.sh ${INSTANCE}"
fi
echo "    Start:             systemctl --user start ${SERVICE_NAME}"
echo "    Port:              podman port systemd-${SERVICE_NAME}"
echo "    Logs:              journalctl --user -u ${SERVICE_NAME} -f"
echo "    Refresh timer:     systemctl --user enable --now pkdump-refresh@${INSTANCE}.timer"
echo "    Backup-check (L1): edit ~/.config/pkdump/${INSTANCE}/alerts.env (ping URL), then:"
echo "                       systemctl --user enable --now pkdump-backup-check@${INSTANCE}.timer"
echo "    Disk alert (L4):   edit ~/.config/pkdump/alerts.env (Pushover), then:"
echo "                       systemctl --user enable --now pkdump-diskcheck.timer"
echo "    Restore (DR):      deploy/RESTORE.md  (off-box S3 via deploy/restore-litestream.sh)"
echo "    Litestream backup: edit ~/.config/pkdump/${INSTANCE}/{litestream.env,aws/}, then: systemctl --user start pkdump-litestream-${INSTANCE}.service"
echo "    Teardown:          bash deploy/teardown.sh ${INSTANCE}"
