#!/usr/bin/env bash
#
# Install the offline lakehouse for an instance (pd-fzeb): the Nessie catalog's
# Quadlet unit, its podman network, and the PyIceberg job image.
#
# Deliberately NOT part of deploy/setup.sh. The lakehouse is offline
# infrastructure — nothing on the serving path touches it — and a box can run
# the app perfectly well without it. Folding it into the app's setup would mean
# every instance, including the `--test` ones CI throws away, standing up a JVM.
#
#   bash deploy/setup-lake.sh prod              # install + build the job image
#   bash deploy/setup-lake.sh prod --port 19120 # pin the loopback debug port
#   bash deploy/setup-lake.sh prod --remove     # uninstall (keeps the data dir)
#
# It reloads systemd itself; starting the catalog is the operator's call:
#   systemctl --user start pkdump-nessie-prod
#
# ── WHAT THIS SCRIPT WILL NOT DO ────────────────────────────────────────────
# Invent a bucket name. The lake bucket is SEPARATE from the Litestream backup
# bucket — same account, same AWS_PROFILE=pkdump role path, different bucket —
# because the backup bucket holds the only irreplaceable data in the system and
# a lifecycle rule written for the lake must never be able to reach it. Its name
# is host config in ~/.config/pkdump/lake.env, created by the operator who
# created the bucket. Absent that file, this script says so and stops.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# The lakehouse lives in whatever store the INSTANCE already lives in — not in
# whatever store.env says, and deliberately not read from store.env at all (the
# same rule setup.sh follows, so prod never opts in). Two reasons, and the
# second one is not obvious: podman 4.9 gives each store its own rootless-netns
# file but ONE shared scaffolding directory, and whichever store cleans up last
# deletes it, leaving the other unable to start any container on a user-defined
# network. The Nessie unit is on a user-defined network. Splitting it from its
# instance's store would arm that trap (pd-yfev).
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"

INSTANCE="${1:-}"
[ -n "$INSTANCE" ] || {
    echo "usage: bash deploy/setup-lake.sh <instance> [--port N] [--remove]" >&2
    exit 2
}
shift

REMOVE=""
NESSIE_PORT=""
while [ $# -gt 0 ]; do
    case "$1" in
    --remove) REMOVE=1 ;;
    --port)
        shift
        NESSIE_PORT="${1:-}"
        ;;
    *)
        echo "unknown argument: $1" >&2
        exit 2
        ;;
    esac
    shift
done

LAKE_ENV="${HOME}/.config/pkdump/lake.env"
QUADLET_DIR="${HOME}/.config/containers/systemd"
NESSIE_UNIT="${QUADLET_DIR}/pkdump-nessie-${INSTANCE}.container"
NETWORK="pkdump-lake-${INSTANCE}"
JOB_IMAGE="localhost/pkdump-lake:${INSTANCE}"

if [ -n "$REMOVE" ]; then
    echo "==> Removing the lakehouse units for '${INSTANCE}'"
    systemctl --user stop "pkdump-nessie-${INSTANCE}" 2>/dev/null || true
    rm -f "$NESSIE_UNIT"
    systemctl --user daemon-reload
    pkdump_store_adopt_instance "$INSTANCE"
    pkdump_store_activate
    podman network rm "$NETWORK" 2>/dev/null || true
    echo "    Unit and network removed. The version store directory and the bucket are"
    echo "    UNTOUCHED — removing an instance must never delete the catalog's state."
    exit 0
fi

# --- The host's answer, or nothing ------------------------------------------

if [ ! -f "$LAKE_ENV" ]; then
    cat >&2 <<EOF
ERROR: ${LAKE_ENV} does not exist.

The lake bucket's name is host configuration, like alerts.env, litestream.env
and store.env beside it — this script will not guess one. Create the bucket
(separate from the Litestream backup bucket), then write:

    # The lake bucket. NOT the Litestream backup bucket.
    PKDUMP_LAKE_S3_BUCKET=<bucket>
    PKDUMP_LAKE_S3_REGION=us-west-2
    # The profile in ~/.config/pkdump/<instance>/aws/config to assume. Same
    # role path Litestream uses — auto-refreshing temporary credentials, never
    # long-lived static keys.
    AWS_PROFILE=pkdump
    # Nessie's version store. On this box that must be on /workspaces — the
    # disk \$HOME lives on is the one prod runs from.
    PKDUMP_LAKE_NESSIE_DATA=/workspaces/pkdump-lake/nessie

and run this script again.
EOF
    exit 1
fi

# shellcheck disable=SC1090
. "$LAKE_ENV"

for var in PKDUMP_LAKE_S3_BUCKET PKDUMP_LAKE_S3_REGION PKDUMP_LAKE_NESSIE_DATA AWS_PROFILE; do
    if [ -z "${!var:-}" ]; then
        echo "ERROR: ${var} is not set in ${LAKE_ENV}." >&2
        exit 1
    fi
done

# The one guard worth spending a line on: the lake and the backups must not
# share a bucket. Everything in the lake is reproducible by construction;
# everything in the backup bucket is not.
LITESTREAM_ENV="${HOME}/.config/pkdump/${INSTANCE}/litestream.env"
if [ -f "$LITESTREAM_ENV" ]; then
    BACKUP_BUCKET="$(sed -n 's/^LITESTREAM_S3_BUCKET=//p' "$LITESTREAM_ENV" | tail -1)"
    if [ -n "$BACKUP_BUCKET" ] && [ "$BACKUP_BUCKET" = "$PKDUMP_LAKE_S3_BUCKET" ]; then
        echo "ERROR: the lake bucket and the Litestream backup bucket are both" >&2
        echo "       '${PKDUMP_LAKE_S3_BUCKET}'. They must be separate — see deploy/README.md." >&2
        exit 1
    fi
fi

# --- Install ----------------------------------------------------------------

echo "==> Lakehouse for '${INSTANCE}'"
echo "    warehouse:     s3://${PKDUMP_LAKE_S3_BUCKET}/lake"
echo "    version store: ${PKDUMP_LAKE_NESSIE_DATA}"

mkdir -p "$PKDUMP_LAKE_NESSIE_DATA" "$QUADLET_DIR"

# The role profile is mounted into Nessie, which runs as the `nessie` user mapped
# into the rootless subuid range — so a 0600 file created by the invoking user
# arrives inside the container as root:root 0600 and is unreadable. Nessie then
# fails with "Unable to load credentials from any of the providers in the chain",
# which reads like a credential problem and is a permissions one. The file holds a
# role_arn, a source_profile and a region: no secret material. The actual key is
# the podman secret mounted at /aws/credentials, which is unaffected.
AWS_PROFILE_CONF="${HOME}/.config/pkdump/${INSTANCE}/aws/config"
if [ -f "$AWS_PROFILE_CONF" ]; then
    chmod 0644 "$AWS_PROFILE_CONF"
fi

pkdump_store_adopt_instance "$INSTANCE"
pkdump_store_activate

echo "==> Building the PyIceberg job image"
# The lake jobs' runtime: PyIceberg and nothing else. Nessie is the only JVM in
# this design and it is somebody else's image.
podman build -t "$JOB_IMAGE" -f "${REPO_DIR}/lake/Containerfile" "${REPO_DIR}/lake"

echo "==> Installing the Quadlet unit"
# The network is created here rather than as a .network unit, on purpose: a
# Quadlet unit would be generated into whatever store systemd's generator finds,
# while `podman network create` runs in the store this script just adopted from
# the instance. Getting those two apart is how a container ends up on a network
# its own store cannot see.
podman network exists "$NETWORK" || podman network create "$NETWORK" >/dev/null
sed -e "s|{{INSTANCE}}|${INSTANCE}|g" \
    -e "s|{{PORT}}|${NESSIE_PORT}|g" \
    -e "s|{{DATA_DIR}}|${PKDUMP_LAKE_NESSIE_DATA}|g" \
    -e "s|{{WAREHOUSE}}|s3://${PKDUMP_LAKE_S3_BUCKET}/lake|g" \
    -e "s|{{REGION}}|${PKDUMP_LAKE_S3_REGION}|g" \
    "$REPO_DIR/deploy/pkdump-nessie.container" >"$NESSIE_UNIT"
pkdump_store_stamp_unit "$NESSIE_UNIT"

systemctl --user daemon-reload

cat <<EOF

==> Installed.
    ${NESSIE_UNIT}
    network ${NETWORK}
    ${JOB_IMAGE}

    Start it:   systemctl --user start pkdump-nessie-${INSTANCE}
    Watch it:   journalctl --user -u pkdump-nessie-${INSTANCE} -f
    Prove it:   podman run --rm --network pkdump-lake-${INSTANCE} \\
                  -e PKDUMP_LAKE_NESSIE_URI=http://pkdump-nessie-${INSTANCE}:19120/iceberg/ \\
                  ${JOB_IMAGE} pkdump-lake-roundtrip

    Then arm the nightly transform (pd-8m5c) — nothing else records today's
    value for any tenant, and a day it does not run is a hole in every chart:
                systemctl --user enable --now pkdump-value-snapshots@${INSTANCE}.timer
                bash deploy/value-snapshots.sh ${INSTANCE}   # or run one now

    The ownership shipment (pd-dxn3) is installed too and is NOT armed here:
                systemctl --user enable --now pkdump-ship@${INSTANCE}.timer

    Arming it needs two things first — a master key (bash deploy/keys.sh
    ${INSTANCE} init, BACKED UP) and PKDUMP_TENANT_AWS_PROFILE in lake.env —
    and it should wait for the backfill: pkdump outbox emit --all
    --all-tenants (pd-385w). The shipper ships the OUTBOX, and an existing
    collection's outbox starts empty (pd-whsw): armed before the backfill has
    run, it faithfully ships every change made from tonight and nothing
    anybody already owns, which is a zone that looks populated and is not.

    The catalog has NO authentication (Nessie says so in its own startup log),
    which is why it publishes on 127.0.0.1 only and jobs reach it over the
    pkdump-lake-${INSTANCE} network. Do not publish it on 0.0.0.0.
EOF
