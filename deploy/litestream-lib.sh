#!/usr/bin/env bash
#
# Shared Litestream addressing for the deploy scripts (pd-fof4, pd-nd6w).
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
# the charset a HANDLE is held to where it is issued. A handle is no longer a
# filename — `pd-fci1` split the two, and what names a file is a `database_id`
# (validate_database_id, below) — but a handle-named database from before
# `pkdump tenant migrate` still exists on real volumes, so a stem may legally be
# either shape and both have to be checked before anything is concatenated into
# a path. Enforced here as well as in Rust because these scripts take the stem
# from an operator's command line, where a stray `/` would silently retarget the
# whole S3 prefix.
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

# Mirrors pkdump-db's validate_database_id (crates/pkdump-db/src/paths.rs):
# 26 characters of UPPERCASE Crockford base32. pd-zr9n settled the case question
# deliberately — `01J8…` and `01j8…` are one file on a case-insensitive
# filesystem but two S3 prefixes, which is the same disagreement the handle
# validator above exists to prevent — so this side has to agree, uppercase and
# all. The alphabet has no `.`, `/` or `\`, so a value that passes cannot address
# anything but a sibling under the tenants prefix.
validate_database_id() {
    local id="${1-}"
    if [ "${#id}" -ne 26 ]; then
        echo "ERROR: database id '${id}' is not 26 characters" >&2; return 1
    fi
    case "$id" in
        *[!0-9A-HJKMNPQRSTVWXYZ]*)
            echo "ERROR: database id '${id}' is not Crockford base32 (0-9 A-Z less I L O U)" >&2
            return 1 ;;
    esac
}

# validate_db_stem <stem> — what may legally be the filename stem of a database
# under tenants/, and therefore a replica path component.
#
# EITHER shape, because during the migration onto opaque ids (pd-hqee) a data
# volume legitimately holds both: databases minted since pd-zr9n are named by an
# uppercase ULID `database_id`, and ones predating it are still named by their
# handle — `tenants/collection.sqlite` is on the prod box right now. A restore
# that accepted only one of the two would refuse to recover real databases, and
# which one it refused would change under the operator mid-migration.
#
# It is NOT a widening of the handle rule. A handle still has to satisfy
# validate_tenant_name wherever a handle is what is being validated; this is the
# separate question of what may name a FILE, and the two answers stopped being
# the same string the moment the epic split them.
validate_db_stem() {
    local stem="${1-}"
    validate_tenant_name "$stem" 2>/dev/null && return 0
    validate_database_id "$stem" 2>/dev/null && return 0
    echo "ERROR: '${stem}' is neither a tenant name ([a-z0-9][a-z0-9_-]{0,31}) nor a 26-character uppercase Crockford database id" >&2
    return 1
}

# tenant_db_path <stem> — the database path inside the container / data volume.
tenant_db_path() {
    validate_db_stem "$1" || return 1
    printf '/data/tenants/%s.sqlite' "$1"
}

# tenant_replica_url <tenant> — the S3 replica URL the sidecar replicates to.
#
# Litestream derives `<replica path>/<file name>`, so the tenant's prefix is
# "${LITESTREAM_S3_PATH}/<stem>.sqlite" — the stem, not the person. `region`
# rides in the query string for
# the same reason it is pinned in the YAML: without it Litestream calls
# s3:GetBucketLocation, which the backup role does not grant.
tenant_replica_url() {
    validate_db_stem "$1" || return 1
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

# ── THE USER REGISTRY (pd-nd6w) ─────────────────────────────────────────────
# handle -> database_id, at the data ROOT rather than under tenants/. It is not
# a tenant, so it is addressed by its own pair of helpers rather than by passing
# a fake tenant name through the ones above — a name like "registry" would be a
# valid tenant name, and the two would then be one typo apart.

# registry_db_path — the registry inside the container / data volume.
registry_db_path() {
    printf '/data/registry.sqlite'
}

# registry_replica_url — the S3 replica URL the sidecar replicates the registry
# to.
#
# NOT derived from tenant_replica_url's prefix. The registry's `dbs:` entry is a
# `path:` one, and Litestream uses a `path:` replica prefix VERBATIM — it does
# not append the file name the way directory mode does. So the whole prefix,
# `.sqlite` included, is LITESTREAM_S3_REGISTRY_PATH, and it names one database.
#
# The `:?` on it is load-bearing rather than defensive. An unset replica prefix
# is the one misconfiguration Litestream does not shout about: it replicates to
# the bucket root in silence (measured, v0.5.16). Everything on this side of the
# fence refuses to guess.
registry_replica_url() {
    : "${LITESTREAM_S3_BUCKET:?missing in litestream.env}"
    : "${LITESTREAM_S3_REGISTRY_PATH:?missing in litestream.env — run deploy/setup.sh <instance> to backfill it}"
    : "${LITESTREAM_S3_REGION:?missing in litestream.env}"

    local url="s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_REGISTRY_PATH#/}?region=${LITESTREAM_S3_REGION}"
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

# ── THE IMAGE, PINNED, IN ONE PLACE (pd-pfxf) ───────────────────────────────
#
# Every sidecar, every restore, every freshness check and every gate runs THIS
# image. It was `litestream:latest` in eight places, and on 2026-08-31 that tag
# moved from 0.5.16 to 0.5.17. Nothing in the tree changed; three container
# gates went red together the next morning and read as networking flakiness.
#
# WHAT ACTUALLY MOVED. 0.5.17 carries "fix(logging): downgrade replica sync
# messages from INFO to DEBUG". deploy/backup-check.sh reads a tenant's two
# TXIDs out of exactly that message (sidecar_position — see the long note there
# for why the CLI cannot answer instead), so at the default level the line it
# parses stopped existing: every tenant fell to "not judged" inside the grace
# window and would have gone permanently STALE after it. Prod's own backup
# verification would have stopped verifying and then started paging, and the
# only warning anybody got was three test gates.
#
# So this is pd-pejn's rule — A MOVING TAG IS NOT A PIN — applied to the other
# image this repo runs. The Containerfile's bases name their Debian release;
# this names its Litestream version. Bumping it is a deliberate edit, and
# tests/litestream/image_pin_test.sh is what makes forgetting one of the copies
# a failure in under a second instead of a red container tier in twenty minutes.
#
# The message this pin protects is emitted at DEBUG from 0.5.17 on, which is why
# deploy/litestream.yml carries a `logging:` block asking for it by name. Move
# this version and re-read that block: the two are one contract.
#
# Callers may override for a one-off experiment (`LITESTREAM_IMAGE=… bash
# tests/litestream/run.sh`); nothing under deploy/ or tests/ may HARDCODE
# another value, which is what the gate asserts.
PKDUMP_LITESTREAM_VERSION="0.5.17"
PKDUMP_LITESTREAM_IMAGE="docker.io/litestream/litestream:${PKDUMP_LITESTREAM_VERSION}"
