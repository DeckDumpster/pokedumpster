#!/usr/bin/env bash
# Write PKDUMP_NESSIE_BACKUP_URL (and AWS_REGION) from the infra/ Terraform outputs into
# lake.env (mode 600), idempotently. Run on the prod host after `terraform apply`.
#
#   bash deploy/nessie-backup-config.sh <instance> [--with-credential]
#
# The backup job signs with the AMBIENT AWS identity — the same one as the lake job. So by
# default this writes only the URL and region, and the backup bucket must be reachable by
# that identity. --with-credential also writes the dedicated user's key pair (read from
# Secrets Manager); that overrides AWS_PROFILE for every job sourcing lake.env, so it is
# REFUSED when lake.env selects a profile or role.
set -euo pipefail

INSTANCE="${1:?usage: nessie-backup-config.sh <instance> [--with-credential]}"
WITH_CRED=0; [ "${2:-}" = "--with-credential" ] && WITH_CRED=1
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"
INFRA="${PKDUMP_INFRA_DIR:-${HERE}/../infra}"
die() { echo "nessie-backup-config: $*" >&2; exit 1; }

[ -f "$LAKE_ENV" ] || die "${LAKE_ENV} does not exist — bash deploy/setup-lake.sh ${INSTANCE} first"
URL="${PKDUMP_NESSIE_BACKUP_URL_VALUE:-$(terraform -chdir="$INFRA" output -raw backup_url)}"
REGION="${PKDUMP_NESSIE_BACKUP_REGION_VALUE:-$(terraform -chdir="$INFRA" output -raw bucket_region)}"
case "$URL" in s3://?*) ;; *) die "not an s3:// URL: ${URL}" ;; esac

# set_var NAME VALUE: replace an active assignment or append one; never touch comments.
set_var() {
    local tmp; tmp="$(mktemp "${LAKE_ENV}.XXXXXX")"
    grep -v "^${1}=" "$LAKE_ENV" >"$tmp" || true
    printf '%s=%s\n' "$1" "$2" >>"$tmp"
    chmod 600 "$tmp"; mv "$tmp" "$LAKE_ENV"
}

if [ "$WITH_CRED" = 1 ]; then
    if grep -Eq '^(AWS_PROFILE|PKDUMP_LAKE_S3_ROLE_ARN|PKDUMP_LAKE_S3_BASE_PROFILE)=' "$LAKE_ENV"; then
        die "lake.env selects an AWS profile/role; static keys would override it for the lake jobs. Grant that identity the bucket instead"
    fi
    SECRET="$(aws secretsmanager get-secret-value --secret-id "$(terraform -chdir="$INFRA" output -raw credential_secret_name)" \
        --query SecretString --output text)"
    set_var AWS_ACCESS_KEY_ID "$(jq -er .AWS_ACCESS_KEY_ID <<<"$SECRET")"
    set_var AWS_SECRET_ACCESS_KEY "$(jq -er .AWS_SECRET_ACCESS_KEY <<<"$SECRET")"
fi
set_var PKDUMP_NESSIE_BACKUP_URL "$URL"
set_var AWS_REGION "$REGION"
chmod 600 "$LAKE_ENV"
echo "nessie-backup-config: ${LAKE_ENV} now names ${URL} (${REGION})"
