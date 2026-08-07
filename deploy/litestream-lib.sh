#!/usr/bin/env bash
#
# Shared Litestream addressing for the deploy scripts (pd-fof4).
#
# deploy/litestream.yml runs the sidecar in DIRECTORY MODE: it discovers every
# tenants/*.sqlite itself and DERIVES each replica path from the filename. That
# is what makes adding a tenant free and cross-tenant prefix collisions
# unreachable — but it also means the config names no database, so
# `restore -config <that file> <db path>` cannot resolve one. Point operations
# (restore, freshness checks) address the replica by URL and have to derive that
# URL exactly the way the sidecar derived it.
#
# This file is that derivation, in ONE place. tests/litestream/run.sh asserts
# what `tenant_replica_url` produces against what the sidecar actually wrote into
# the bucket — for a NON-FIRST tenant, so an off-by-one in the derivation fails
# the gate instead of quietly restoring somebody else's collection.
#
# Sourced, not executed. Callers must have the instance's litestream.env in the
# environment (LITESTREAM_S3_BUCKET / _PATH / _REGION, optionally _ENDPOINT).

# Mirrors pkdump-db's validate_tenant_name (crates/pkdump-db/src/paths.rs):
# a tenant name is a filename AND an S3 path component at the same time, so the
# same narrow alphabet applies on this side of the fence. Enforced here too
# because these scripts take the name from an operator's command line, where a
# stray `/` would silently retarget the whole S3 prefix.
validate_tenant_name() {
    local name="${1-}"
    case "$name" in
        "") echo "ERROR: tenant name is empty" >&2; return 1 ;;
        [a-z0-9]*) ;;
        *) echo "ERROR: tenant name '${name}' must start with a lowercase letter or digit" >&2; return 1 ;;
    esac
    if [ "${#name}" -gt 32 ]; then
        echo "ERROR: tenant name '${name}' is longer than 32 characters" >&2
        return 1
    fi
    case "$name" in
        *[!a-z0-9_-]*) echo "ERROR: tenant name '${name}' has a character outside a-z 0-9 - _" >&2; return 1 ;;
    esac
}

# tenant_db_path <tenant> — the database path inside the container / data volume.
tenant_db_path() {
    validate_tenant_name "$1" || return 1
    printf '/data/tenants/%s.sqlite' "$1"
}

# tenant_replica_url <tenant> — the S3 replica URL the sidecar replicates to.
#
# Litestream derives `<replica path>/<file name>`, so the tenant's prefix is
# "${LITESTREAM_S3_PATH}/<tenant>.sqlite". `region` rides in the query string for
# the same reason it is pinned in the YAML: without it Litestream calls
# s3:GetBucketLocation, which the backup role does not grant.
tenant_replica_url() {
    validate_tenant_name "$1" || return 1
    : "${LITESTREAM_S3_BUCKET:?missing in litestream.env}"
    : "${LITESTREAM_S3_PATH:?missing in litestream.env}"
    : "${LITESTREAM_S3_REGION:?missing in litestream.env}"

    local base="${LITESTREAM_S3_PATH%/}"
    local url="s3://${LITESTREAM_S3_BUCKET}/${base}/${1}.sqlite?region=${LITESTREAM_S3_REGION}"
    if [ -n "${LITESTREAM_S3_ENDPOINT:-}" ]; then
        url="${url}&endpoint=${LITESTREAM_S3_ENDPOINT}"
    fi
    printf '%s' "$url"
}

# tenants_on_volume <mountpoint> — every tenant that has a database on the data
# volume, one per line. The set Litestream's `dir:` entry is replicating, read
# off the same directory by the same glob.
tenants_on_volume() {
    local mountpoint="$1" db name
    for db in "${mountpoint}"/tenants/*.sqlite; do
        [ -e "$db" ] || continue
        name="$(basename "$db" .sqlite)"
        printf '%s\n' "$name"
    done
}
