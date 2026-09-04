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
#   bash deploy/setup-lake.sh prod --arm-shipper  # enable the nightly shipment
#
# It reloads systemd itself; starting the catalog is the operator's call:
#   systemctl --user start pkdump-nessie-prod
#
# ── --arm-shipper: THE ONE PLACE ARMING IS A DELIBERATE ACT (pd-0h2p) ───────
# `pkdump-ship@<instance>.timer` is installed for every instance and enabled for
# none, and three things must be true before it is enabled. Those three were
# written down in four places and CHECKED in none, which is how a box ends up
# with a tenant zone that looks populated and is not — the shipper ships the
# OUTBOX, and an existing collection's outbox starts empty (pd-whsw), so armed
# early it faithfully ships every change made from tonight and nothing anybody
# already owns. Nothing reports that afterwards: the transform values whatever
# the zone holds and prints a plausible number.
#
# So the three preconditions are the mode, not its documentation:
#
#   1. PKDUMP_TENANT_AWS_PROFILE is set in lake.env, and is NOT the catalog's
#      profile. One profile for both zones is not a narrow policy, it is no
#      boundary at all, and it looks exactly like a correct one.
#   2. The master key exists, at mode 600. Every object in the zone is
#      encrypted under a key derived from it.
#   3. Every registered tenant has a completed full backfill on record —
#      asked of the collections themselves, through `pkdump outbox status
#      --require-backfill`, which answers with an exit status rather than a
#      sentence a script would have to parse.
#
# The fourth thing cannot be checked from here and is therefore SAID rather
# than asserted: that the master key has been BACKED UP. A lost key is
# indistinguishable from a deleted tenant by design (deploy/KEYS.md), and
# nothing on this box can know whether a copy reached the password manager.
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
# ...and where the job image is named and built, shared with deploy/deploy.sh so
# that installing a lakehouse and shipping a change to one build the same image
# the same way (pd-rn4c).
# shellcheck source=deploy/lake-lib.sh
. "$SCRIPT_DIR/lake-lib.sh"

INSTANCE="${1:-}"
[ -n "$INSTANCE" ] || {
    echo "usage: bash deploy/setup-lake.sh <instance> [--port N] [--remove] [--arm-shipper]" >&2
    exit 2
}
shift

REMOVE=""
ARM_SHIPPER=""
NESSIE_PORT=""
while [ $# -gt 0 ]; do
    case "$1" in
    --remove) REMOVE=1 ;;
    --arm-shipper) ARM_SHIPPER=1 ;;
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

# Uninstalling and arming are opposite acts. Asked for together, neither is
# what was meant.
if [ -n "$REMOVE" ] && [ -n "$ARM_SHIPPER" ]; then
    echo "--remove and --arm-shipper are opposite acts; pick one." >&2
    exit 2
fi

LAKE_ENV="${HOME}/.config/pkdump/lake.env"
QUADLET_DIR="${HOME}/.config/containers/systemd"
NESSIE_UNIT="${QUADLET_DIR}/pkdump-nessie-${INSTANCE}.container"
NETWORK="pkdump-lake-${INSTANCE}"
JOB_IMAGE="$(pkdump_lake_job_image "$INSTANCE")"

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

# --- --arm-shipper ----------------------------------------------------------
#
# Its own mode, and it exits here: arming is not a step of installing a
# lakehouse, it is the act that comes AFTER the backfill, days or weeks later.
# It deliberately does not require PKDUMP_LAKE_NESSIE_DATA — the shipper writes
# Parquet under `tenant/` and never opens the catalog — so the four-variable
# loop below is the installer's, not this mode's.

if [ -n "$ARM_SHIPPER" ]; then
    # Resolved exactly as deploy/ship.sh resolves them, seams included: this
    # mode's whole job is to decide whether THAT script may run nightly, and
    # two spellings of "which key file" is how an arming check ends up
    # inspecting a different file from the one the shipper opens.
    CONF_DIR="${PKDUMP_KEYS_CONF_DIR:-${HOME}/.config/pkdump/${INSTANCE}}"
    KEY_FILE="${CONF_DIR}/tenant-master.key"
    IMAGE="${PKDUMP_SHIP_IMAGE:-localhost/pkdump:${INSTANCE}}"
    DATA="${PKDUMP_SHIP_DATA:-pkdump-${INSTANCE}-data}"
    TIMER="pkdump-ship@${INSTANCE}.timer"
    UNIT_FILE="${HOME}/.config/systemd/user/pkdump-ship@.timer"

    echo "==> Arming ${TIMER}: the preconditions first"

    # 1. The credential boundary. Checked in this order because an unset
    #    profile and a profile equal to the catalog's are different mistakes
    #    with different repairs, and `TenantZoneConfig` refuses each by name
    #    at runtime — which is far too late to be the first time anyone hears
    #    about it, since by then the timer is armed and the failure arrives at
    #    07:00 as a unit that pages.
    if [ -z "${PKDUMP_TENANT_AWS_PROFILE:-}" ]; then
        cat >&2 <<EOF
ERROR: ${LAKE_ENV} does not set PKDUMP_TENANT_AWS_PROFILE. NOT armed.

The tenant zone shares the lake's bucket and is separated from it by a prefix,
and a prefix boundary is only a policy — so the credential that reaches
\`tenant/\` is a DIFFERENT one from the catalog's. There is no default: falling
back to the catalog's profile would erase the boundary silently.

    PKDUMP_TENANT_AWS_PROFILE=<profile>   # reaches tenant/ and nothing else

See deploy/TENANT_ZONE.md §4.
EOF
        exit 1
    fi
    if [ "$PKDUMP_TENANT_AWS_PROFILE" = "${AWS_PROFILE:-}" ]; then
        cat >&2 <<EOF
ERROR: PKDUMP_TENANT_AWS_PROFILE and AWS_PROFILE are both
       '${PKDUMP_TENANT_AWS_PROFILE}' in ${LAKE_ENV}. NOT armed.

One profile for both zones is not a narrow policy — it is no boundary, and a
zone governed by nothing looks exactly like a zone governed correctly until
someone reads a tenant's holdings with the catalog's role.

See deploy/TENANT_ZONE.md §4.
EOF
        exit 1
    fi

    # 2. The master key. Its mode is asserted on the HOST rather than trusted
    #    from the code that wrote it, the same claim deploy/keys.sh makes after
    #    an `init`: "the code sets 600" and "the file is 600" are different
    #    claims and only the second one protects anything.
    if [ ! -f "$KEY_FILE" ]; then
        cat >&2 <<EOF
ERROR: no master key at ${KEY_FILE}. NOT armed.

Every object in the tenant zone is encrypted under a key derived from it, so an
armed shipper without one derives nothing and skips every tenant nightly.

    bash deploy/keys.sh ${INSTANCE} init      # once, and BACK IT UP

See deploy/KEYS.md.
EOF
        exit 1
    fi
    KEY_MODE="$(stat -c '%a' "$KEY_FILE")"
    if [ "$KEY_MODE" != "600" ]; then
        echo "ERROR: ${KEY_FILE} is mode ${KEY_MODE}, not 600. NOT armed." >&2
        echo "  Fix it now: chmod 600 ${KEY_FILE}" >&2
        exit 1
    fi

    # The store the INSTANCE lives in — the same rule the rest of this file
    # follows, and what makes the two questions below ask about the right box.
    pkdump_store_adopt_instance "$INSTANCE"
    pkdump_store_activate

    if [ ! -f "$UNIT_FILE" ]; then
        echo "ERROR: ${UNIT_FILE} is not installed. NOT armed." >&2
        echo "  The unit templates are installed by the app's own setup, not by this" >&2
        echo "  script: bash deploy/setup.sh ${INSTANCE}" >&2
        exit 1
    fi
    if ! podman image exists "$IMAGE" 2>/dev/null; then
        echo "ERROR: no image ${IMAGE}. NOT armed." >&2
        echo "  '${INSTANCE}' has not been built on this box: bash deploy/setup.sh ${INSTANCE}" >&2
        exit 1
    fi

    # 3. The backfill, asked of the collections themselves. This is the
    #    precondition that has no other witness: an un-backfilled collection
    #    ships cleanly, exits 0, and reports nothing wrong for as long as it
    #    runs. The question is answered by an exit status, never by reading
    #    the report's text — a run that died in its container prints exactly
    #    what a clean collection prints (pd-cxq4).
    echo "==> Has every registered tenant been backfilled?"
    if ! podman run --rm --pull=never \
        -v "${DATA}:/data:Z" \
        -e PKDUMP_HOME=/data \
        --entrypoint pkdump \
        "$IMAGE" outbox status --all-tenants --require-backfill; then
        echo "ERROR: the backfill precondition is not met. NOT armed." >&2
        echo "  Arming now would ship tonight's changes and nothing anybody already owns." >&2
        echo "  See deploy/TENANT_ZONE.md §7 and pd-whsw." >&2
        exit 1
    fi

    systemctl --user enable --now "$TIMER"

    cat <<EOF

==> Armed. ${TIMER}
    Nightly at 07:00: deploy/ship.sh ${INSTANCE} — \`pkdump-ship run\` out to the
    zone, then \`pkdump-ship holdings\` back into each tenant's zone_holdings,
    which since pd-i08u is the only thing the transform values a collection
    from. Arm that one too if it is not already:
                systemctl --user enable --now pkdump-value-snapshots@${INSTANCE}.timer

    Run one now:  bash deploy/ship.sh ${INSTANCE}
    Watch it:     journalctl --user -u pkdump-ship@${INSTANCE}.service -f

    ONE precondition was not checked, because nothing on this box can check
    it: that ${KEY_FILE}
    is BACKED UP, in the operator's password manager, exactly like the
    Litestream bootstrap key. A lost key is indistinguishable from a deleted
    tenant BY DESIGN, and from tonight this timer writes data that only that
    key can open. deploy/KEYS.md §1, deploy/RESTORE.md Scenario C.
EOF
    exit 0
fi

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
pkdump_lake_job_image_build "$INSTANCE" "$REPO_DIR"

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

    Then arm the nightly chain. Both halves, in this order — the transform
    values collections from the table the price build writes, so arming only
    the transform is correct arithmetic over prices that never advance
    (pd-up36), and arming neither is a hole in every tenant's chart (pd-8m5c):
                systemctl --user enable --now pkdump-prices@${INSTANCE}.timer
                systemctl --user enable --now pkdump-value-snapshots@${INSTANCE}.timer
                bash deploy/prices.sh ${INSTANCE}            # or run one now
                bash deploy/value-snapshots.sh ${INSTANCE}

    The ownership shipment (pd-dxn3) is installed too and is NOT armed here.
    Arm it when the three preconditions hold, and let this script check them:
                bash deploy/setup-lake.sh ${INSTANCE} --arm-shipper

    Those three are a master key (bash deploy/keys.sh ${INSTANCE} init, and
    BACKED UP — the one thing --arm-shipper cannot check), the tenant zone's
    own PKDUMP_TENANT_AWS_PROFILE in lake.env, and the backfill: pkdump outbox
    emit --all --all-tenants (pd-385w). The shipper ships the OUTBOX, and an
    existing collection's outbox starts empty (pd-whsw): armed before the
    backfill has run, it faithfully ships every change made from tonight and
    nothing anybody already owns, which is a zone that looks populated and is
    not.

    The catalog has NO authentication (Nessie says so in its own startup log),
    which is why it publishes on 127.0.0.1 only and jobs reach it over the
    pkdump-lake-${INSTANCE} network. Do not publish it on 0.0.0.0.
EOF
