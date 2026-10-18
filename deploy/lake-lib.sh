#!/usr/bin/env bash
#
# The lake network, from outside a job (pd-p39v). Sourced, never executed.
#
# deploy/prices.sh and deploy/value-snapshots.sh are two wrappers around one
# network: the same Nessie, reached by the same name, over the same user-defined
# bridge. What they have to agree about lives here rather than being spelled out
# twice — the second copy is where a URL drifts.
#
#   pkdump_lake_catalog_uri <instance>          the Iceberg REST endpoint
#   pkdump_lake_health_url <catalog-uri>        that service's config document
#   pkdump_lake_catalog_answering <network> <image> <url>
#                                               does it answer, asked from where
#                                               the job will ask
#
# WHY A READINESS PROBE IS PART OF THE DEPLOY SURFACE AT ALL.
#
# `pkdump_store_netns_ensure` repairs a wedged rootless netns by dropping the
# namespace and restarting what was on it — for the lake network, Nessie. But
# `systemctl --user restart` returns when the CONTAINER is running, and Nessie is
# a JVM that will not answer for another 30-40 seconds. The repair used to return
# there, the job started immediately, and it died on a connection error to a
# service that was coming up fine. By the next run everything was healthy, so the
# unit paged for a condition that had already fixed itself. These three functions
# are what let the repair wait for ANSWERING instead of RESTARTED.
#
# The probe runs where the job runs — a throwaway container ON the network,
# resolving the catalog by name. A host-side curl would take the published-port
# path instead, and can say yes about a network the job itself cannot use. It
# needs nothing the run does not already have: the app image ships
# pkdump-lake-probe, which makes the request.

# The builder wrapper that collects what the previous build orphaned (pd-h3wy).
# Both images this checkout ships leak the same way, so both go through it.
# shellcheck source=deploy/image-lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/image-lib.sh"

# The Iceberg REST endpoint the jobs talk to. PKDUMP_LAKE_NESSIE_URI is how
# lake.env (and every test substrate) redirects it; the default is the name
# deploy/setup-lake.sh gives the container.
pkdump_lake_catalog_uri() {
    printf '%s' "${PKDUMP_LAKE_NESSIE_URI:-http://pkdump-nessie-${1}:19120/iceberg/}"
}

# The same service's own config document — always present, needs no catalog, no
# warehouse and no credentials, and is the cheapest thing that proves the JVM is
# serving rather than merely running. DERIVED from the catalog URI, so a test
# that redirects the catalog redirects the health check with it and cannot end up
# waiting on a Nessie that is not the one the job will use.
pkdump_lake_health_url() {
    printf '%s' "${PKDUMP_LAKE_HEALTH_URL:-${1%%/iceberg/*}/api/v2/config}"
}

# Exits 0 when the catalog answers. Quiet: it is polled, and a caller that wanted
# the failure text would be reading 40 identical copies of it. <image> is the app
# image, which ships the probe.
pkdump_lake_catalog_answering() {
    podman run --rm --network "$1" --entrypoint pkdump-lake-probe "$2" get "$3" >/dev/null 2>&1
}

# Is a lakehouse installed for this instance at all? The Nessie Quadlet unit is
# the marker, deliberately, and not an image: a box with no lakehouse is the
# ordinary case — nothing on the serving path touches one.
pkdump_lake_installed() {
    [ -f "${HOME}/.config/containers/systemd/pkdump-nessie-${1}.container" ]
}

# --- The lake jobs ---------------------------------------------------------
#
# Every lake job is a Rust binary in the APP image (`localhost/pkdump:<inst>`).
# The Python lake and its image were deleted in sp-4k7mxi.7; there is no
# PKDUMP_LAKE_JOBS switch any more, and a lake.env that still sets it is ignored.

# The image the Rust twins run from: the app image, which ships them.
pkdump_lake_rust_image() {
    printf '%s' "${PKDUMP_LAKE_RUST_IMAGE:-localhost/pkdump:${1}}"
}

# The Rust jobs read `raw/` through the AWS SDK's default chain, which knows
# AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY and AWS_PROFILE but not the
# PKDUMP_LAKE_S3_* static keys the Iceberg catalog takes. Where
# lake.env gives static keys (the MinIO substrate), forward them under the SDK's
# names too, or the raw read dies with "no credentials found in chain". Prod uses
# the mounted role profile and sets no static keys, so this adds nothing there.
# Appends to ENV_ARGS; an explicit AWS_ACCESS_KEY_ID in lake.env wins.
pkdump_lake_rust_credential_env() {
    if [ -n "${PKDUMP_LAKE_S3_ACCESS_KEY_ID:-}" ] && [ -z "${AWS_ACCESS_KEY_ID:-}" ]; then
        ENV_ARGS+=(-e "AWS_ACCESS_KEY_ID=${PKDUMP_LAKE_S3_ACCESS_KEY_ID}")
    fi
    if [ -n "${PKDUMP_LAKE_S3_SECRET_ACCESS_KEY:-}" ] && [ -z "${AWS_SECRET_ACCESS_KEY:-}" ]; then
        ENV_ARGS+=(-e "AWS_SECRET_ACCESS_KEY=${PKDUMP_LAKE_S3_SECRET_ACCESS_KEY}")
    fi
}

# pkdump_lake_job_select <instance> <rust-name>
# Sets RUN_IMAGE, RUN_EP (podman args placing the binary) and RUN_CMD (the
# program words that follow the image; always empty). The app image's ENTRYPOINT
# is `pkdump serve`, so the binary is named with --entrypoint.
pkdump_lake_job_select() {
    RUN_IMAGE="$(pkdump_lake_rust_image "$1")"
    RUN_EP=(--entrypoint "$2")
    RUN_CMD=()
    pkdump_lake_rust_credential_env
    if ! podman image exists "$RUN_IMAGE" 2>/dev/null; then
        echo "no app image ${RUN_IMAGE} to run ${2} from; deploy it (deploy/deploy.sh ${1})" >&2
        return 1
    fi
}
