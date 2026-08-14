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
# Container storage: rootless Podman's default store lives under $HOME, which on
# the deployment box is the same disk prod runs from. Set PKDUMP_STORE_ROOT=<dir>
# to build this instance's image and volume into an alternate store instead —
# opt-in, so prod (which never sets it) is unaffected. See deploy/store-lib.sh.
# This script scaffolds the host-config file that names that directory
# (~/.config/pkdump/store.env) but deliberately does not read it: prod is
# installed with this script, and where prod's volumes live is not something a
# host config file gets to change. deploy/ci.sh is what reads it.
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

if ! command -v podman >/dev/null 2>&1; then
    echo "ERROR: podman not found. Install it: sudo apt install podman"
    exit 1
fi

# Opt-in alternate container store (pd-fite). No-op unless PKDUMP_STORE_ROOT is
# set — and prod never sets it.
#
# An instance that already exists keeps the store it already lives in, for the
# same reason it keeps the port it already publishes (below): re-running setup.sh
# is how unit-file changes reach it, and that must not silently move its image
# and volume into whatever store the calling shell had activated, leaving the
# real ones behind with the unit no longer pointing at them (pd-9rxf). A store
# passed explicitly still wins, so moving an instance on purpose still works.
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"
pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

if ! loginctl show-user "$USER" -p Linger 2>/dev/null | grep -q "Linger=yes"; then
    echo "WARNING: linger not enabled — services stop when you log out."
    echo "  Fix: loginctl enable-linger $USER"
fi

# --- Build the image --------------------------------------------------------
# Or tag one already built: deploy/ci.sh calls this script twice per run and
# three other gates build the same content again, so it builds once up front and
# exports PKDUMP_PREBUILT_IMAGE. Unset — which is prod, always — this is the
# same `podman build` it has always been. See deploy/image-lib.sh.

# shellcheck source=deploy/image-lib.sh
. "$SCRIPT_DIR/image-lib.sh"
if [ -n "${PKDUMP_PREBUILT_IMAGE:-}" ]; then
    echo "==> Tagging ${PKDUMP_PREBUILT_IMAGE} as pkdump:${INSTANCE} (built once this run)..."
else
    echo "==> Building image pkdump:${INSTANCE}..."
fi
pkdump_image_ensure "pkdump:${INSTANCE}" "$REPO_DIR"
podman tag "pkdump:${INSTANCE}" pkdump:latest

# --- Install the unit files -------------------------------------------------
# Every Quadlet + systemd-user unit this checkout ships for the instance, in one
# place, because deploy.sh installs exactly the same set (pd-2t6u). An existing
# instance keeps the port it already publishes unless one is passed explicitly.

echo "==> Installing unit files..."
# shellcheck source=deploy/units-lib.sh
. "$SCRIPT_DIR/units-lib.sh"
pkdump_units_install "$INSTANCE" "$PORT"
pkdump_units_report

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

# Scaffold the host-wide container-store config (pd-rf7c). Which disk non-prod
# container storage belongs on is a fact about THIS box, so it is host config
# rather than a repo constant — and it is scaffolded commented-out so a new box
# has the knob visible instead of undiscoverable. Written but never read by this
# script: setup.sh honours PKDUMP_STORE_ROOT from its environment only, so a
# store.env that opts in cannot silently relocate a prod deploy.
STORE_ENV="${HOME}/.config/pkdump/store.env"
if [ ! -f "$STORE_ENV" ]; then
    mkdir -p "${HOME}/.config/pkdump"
    cat > "$STORE_ENV" <<'EOF'
# Host-wide container-store config for PokeDumpster (pd-rf7c).
#
# Rootless Podman keeps images, layers and volumes under $HOME. Where $HOME
# shares a disk with something that must not run out of space — on this
# project's deployment box, prod itself — name a directory on another
# filesystem here and non-prod container storage goes there instead.
#
# Read by deploy/ci.sh. An explicit PKDUMP_STORE_ROOT in the environment wins
# over this file, including an explicit empty one (that is how a single run opts
# back out). Left commented out, everything uses Podman's default store — which
# is what prod uses, always, on every box.
#
# To delete the store named here, follow the recipe in deploy/store-lib.sh's
# header. NEVER `podman system reset`: it is not scoped by --root/--runroot, and
# aimed at a throwaway store it took prod down (pd-rkrf).
#PKDUMP_STORE_ROOT=/big/disk/pkdump-nonprod-store
EOF
    echo "    Wrote ${STORE_ENV} (container store; commented out = Podman's default)."
fi

# Scaffold the host-wide raw-landing-zone config (pd-th42). The lake bucket is a
# fact about this AWS account, not a repo constant, so it is host config in the
# same shape as alerts.env and store.env — and it is scaffolded commented-out so
# a new box has the knob visible rather than undiscoverable. Nothing lands until
# BOTH this file names a bucket AND a run passes --land-raw (or PKDUMP_LAND_RAW=1);
# an unconfigured lake makes those commands refuse rather than land nothing.
# See deploy/LAKE.md.
LAKE_ENV="${HOME}/.config/pkdump/lake.env"
if [ ! -f "$LAKE_ENV" ]; then
    mkdir -p "${HOME}/.config/pkdump"
    cat > "$LAKE_ENV" <<'EOF'
# Host-wide raw-landing-zone config for PokeDumpster (pd-th42).
#
# `pkdump data refresh --land-raw` writes every upstream response it fetches to
# this bucket, immutably, before parsing it:
#
#   raw/source=<tcgcsv|pokemontcgio|pokemon-tcg-data>/dataset=<...>/
#       ingest_date=YYYY-MM-DD/run=<ULID>/part-NNNN.<ext>.zst  +  _manifest.json
#
# This MUST be a DIFFERENT bucket from LITESTREAM_S3_BUCKET. The backup bucket
# holds the only irreplaceable data in the system — tenant databases and the
# registry — while everything in the lake is reproducible by construction.
# Keeping them apart is what stops a lifecycle rule written for the lake from
# ever reaching the backups. Same account, same AWS_PROFILE=pkdump role path,
# so there is no new credential story: the SDK assumes the role and refreshes.
#
# There is deliberately NO lifecycle rule on raw/. See deploy/LAKE.md.
#PKDUMP_LAKE_S3_BUCKET=CHANGE_ME
#PKDUMP_LAKE_S3_REGION=us-west-2
# Optional key prefix inside the bucket; unset means raw/ sits at the root.
#PKDUMP_LAKE_S3_PREFIX=
# Optional endpoint override — how a MinIO stands in for S3 in a test.
#PKDUMP_LAKE_S3_ENDPOINT=
EOF
    chmod 600 "$LAKE_ENV"
    echo "    Wrote ${LAKE_ENV} (raw landing zone; commented out = landing off)."
fi

# --- Litestream backup sidecar config (pokedumpster-8ch.3) ------------------
# The sidecar's Quadlet unit is installed above with the rest of them; what is
# left is its per-instance config (S3 target + AWS creds). Secrets NEVER live in
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
#   <LITESTREAM_S3_PATH>/<database_id>.sqlite   (see deploy/litestream.yml)
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

# --- Tenant-zone master key (pd-ulds) --------------------------------------
# NOT created here, on purpose, and for the same reason the bootstrap key is
# not: a secret minted by a script nobody was watching is a secret nobody
# backed up — and this one has no second copy anywhere by design. So setup.sh
# prepares the DIRECTORY (already done above, 700, beside litestream.env) and
# names the step; `pkdump keys init` is the operator's deliberate act.
#
# There is exactly one master key and every tenant's key is derived from it, so
# overwriting it destroys every tenant's key at once. `keys init` refuses to
# overwrite and there is no --force.
if [ ! -f "${LS_CONF_DIR}/tenant-master.key" ]; then
    echo "    No tenant-zone master key for '${INSTANCE}' (${LS_CONF_DIR}/tenant-master.key)."
    echo "    Crypto-shredding is off until there is one. To create it:"
    echo "      1. bash deploy/keys.sh ${INSTANCE} init"
    echo "      2. bash deploy/keys.sh ${INSTANCE} backup --yes   # into your password manager,"
    echo "         the same place the [bootstrap] key above lives. There is no other copy."
    echo "    Losing that key is NOT the same event as deleting a tenant — see deploy/KEYS.md."
else
    KEY_MODE="$(stat -c '%a' "${LS_CONF_DIR}/tenant-master.key")"
    if [ "$KEY_MODE" != "600" ]; then
        echo "    WARNING: ${LS_CONF_DIR}/tenant-master.key is mode ${KEY_MODE}, not 600." >&2
        echo "             Fix it: chmod 600 ${LS_CONF_DIR}/tenant-master.key" >&2
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
echo "    Nightly lake chain: needs the lakehouse first (bash deploy/setup-lake.sh ${INSTANCE}), then"
echo "                       land -> derive -> prices -> transform, each ordered after the last:"
echo "                       systemctl --user enable --now pkdump-derive@${INSTANCE}.timer"
echo "                       systemctl --user enable --now pkdump-prices@${INSTANCE}.timer"
echo "                       systemctl --user enable --now pkdump-value-snapshots@${INSTANCE}.timer"
echo "    Backup-check (L1): edit ~/.config/pkdump/${INSTANCE}/alerts.env (ping URL), then:"
echo "                       systemctl --user enable --now pkdump-backup-check@${INSTANCE}.timer"
echo "    Disk alert (L4):   edit ~/.config/pkdump/alerts.env (Pushover), then:"
echo "                       systemctl --user enable --now pkdump-diskcheck.timer"
echo "    Restore (DR):      deploy/RESTORE.md  (off-box S3 via deploy/restore-litestream.sh)"
echo "    Litestream backup: edit ~/.config/pkdump/${INSTANCE}/{litestream.env,aws/}, then: systemctl --user start pkdump-litestream-${INSTANCE}.service"
echo "    Teardown:          bash deploy/teardown.sh ${INSTANCE}"
