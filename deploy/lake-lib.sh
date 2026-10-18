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
# needs nothing the run does not already have: the job image is Python and
# urllib is in its standard library.

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
# the failure text would be reading 40 identical copies of it.
pkdump_lake_catalog_answering() {
    podman run --rm --network "$1" "$2" python -c \
        'import sys, urllib.request; urllib.request.urlopen(sys.argv[1], timeout=5).read()' \
        "$3" >/dev/null 2>&1
}

# --- The job image ----------------------------------------------------------
#
# The lake jobs' runtime: PyIceberg and nothing else. Nessie is the only JVM in
# this design and it is somebody else's image.
#
# This is here rather than inline in deploy/setup-lake.sh because of what
# pd-rn4c cost. The app image had a deploy path (deploy/deploy.sh) and the job
# image had only an INSTALL path (deploy/setup-lake.sh), so a change under
# lake/ reached a box only if an operator remembered a second command. On
# 2026-08-26 prod ran a value-snapshots transform six hours older than its own
# checkout: `catalog.sealed_prices` was never written and no dimension='sealed'
# row was recorded, so the chart reported $10,636.81 of cards with $10,351.47
# of sealed product invisible beside it — at exit 0, over "every registered
# tenant snapshotted", with a plausible number on the page. Same shape as
# pd-2t6u: a deploy that ships some of what the checkout says it ships.

# The image the wrappers run, honouring the same override they read — so a gate
# that redirects the image redirects what gets built into it.
pkdump_lake_job_image() {
    printf '%s' "${PKDUMP_LAKE_JOB_IMAGE:-localhost/pkdump-lake:${1}}"
}

# Is a lakehouse installed for this instance at all? The Nessie Quadlet unit is
# the marker, deliberately, and not the image: `setup-lake.sh --remove` deletes
# the unit and KEEPS the image, so asking about the image would go on rebuilding
# one for an instance whose lakehouse was uninstalled. A box with no lakehouse
# is the ordinary case — nothing on the serving path touches one.
pkdump_lake_installed() {
    [ -f "${HOME}/.config/containers/systemd/pkdump-nessie-${1}.container" ]
}

# Build it. The caller has already adopted and activated the instance's store:
# the lakehouse lives in whatever store the INSTANCE lives in (pd-yfev), never
# in whatever store.env says.
pkdump_lake_job_image_build() {
    pkdump_image_build_collecting -t "$(pkdump_lake_job_image "$1")" \
        -f "${2}/lake/Containerfile" "${2}/lake"
}

# Leave <tag> naming the lake job image. With PKDUMP_PREBUILT_LAKE_IMAGE set
# (deploy/ci.sh builds it once per run, sp-4akezo) <tag> becomes another name for
# that image and nothing is built; otherwise it is built from <repo_dir>/lake.
# Same contract as pkdump_image_ensure, for the same reason: four lake gates each
# built an identical image.
pkdump_lake_job_image_ensure() {
    local tag="$1" repo_dir="$2" prebuilt="${PKDUMP_PREBUILT_LAKE_IMAGE:-}"
    if [ -z "$prebuilt" ]; then
        pkdump_image_build_collecting -t "$tag" \
            -f "${repo_dir}/lake/Containerfile" "${repo_dir}/lake"
        return
    fi
    if ! podman image exists "$prebuilt"; then
        echo "ERROR: PKDUMP_PREBUILT_LAKE_IMAGE=${prebuilt} names no image in this store." >&2
        return 1
    fi
    podman tag "$prebuilt" "$tag"
}

# --- Which implementation the nightly jobs run (sp-utga27) ------------------
#
# build-prices, prices-age and value-snapshots exist twice: the Python jobs in the
# lake image (`localhost/pkdump-lake:<inst>`) and their Rust twins in the APP image
# (`localhost/pkdump:<inst>`), certified row for row against prod on 2026-10-17
# (sp-4k7mxi.4/.5/.6). The Rust jobs are the default. `PKDUMP_LAKE_JOBS=python` in
# ~/.config/pkdump/lake.env is the whole rollback: the Python image stays built and
# the wrappers still use it for the catalog-answering probe either way. nessie-backup
# (backup, check, drill) runs the Rust job too (sp-jz1ws9); the roundtrip stays on Python.
pkdump_lake_jobs_runtime() {
    case "${PKDUMP_LAKE_JOBS:-rust}" in
    rust | python) printf '%s' "${PKDUMP_LAKE_JOBS:-rust}" ;;
    *)
        echo "PKDUMP_LAKE_JOBS=${PKDUMP_LAKE_JOBS} — expected 'rust' or 'python' (lake.env)" >&2
        return 1
        ;;
    esac
}

# The image the Rust twins run from: the app image, which ships them.
pkdump_lake_rust_image() {
    printf '%s' "${PKDUMP_LAKE_RUST_IMAGE:-localhost/pkdump:${1}}"
}

# The Rust jobs read `raw/` through the AWS SDK's default chain, which knows
# AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY and AWS_PROFILE but not the
# PKDUMP_LAKE_S3_* static keys the Python jobs and the Iceberg catalog take. Where
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

# pkdump_lake_job_select <instance> <python-name> <rust-name>
# Sets RUN_IMAGE, RUN_EP (podman args placing the binary) and RUN_CMD (the
# program words that follow the image). The app image's ENTRYPOINT is
# `pkdump serve`, so the Rust binary is named with --entrypoint.
pkdump_lake_job_select() {
    local rt
    rt="$(pkdump_lake_jobs_runtime)" || return 1
    if [ "$rt" = rust ]; then
        RUN_IMAGE="$(pkdump_lake_rust_image "$1")"
        RUN_EP=(--entrypoint "$3")
        RUN_CMD=()
        pkdump_lake_rust_credential_env
        if ! podman image exists "$RUN_IMAGE" 2>/dev/null; then
            echo "no app image ${RUN_IMAGE} to run ${3} from; deploy it (deploy/deploy.sh ${1}) or set PKDUMP_LAKE_JOBS=python in lake.env" >&2
            return 1
        fi
    else
        RUN_IMAGE="${JOB_IMAGE}"
        RUN_EP=()
        RUN_CMD=("$2")
    fi
}
