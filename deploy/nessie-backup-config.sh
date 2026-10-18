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
# --with-credential-file <path> [--profile NAME] records PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE
# and PKDUMP_NESSIE_BACKUP_PROFILE (default pkdump-lake-backup) instead: the backup signs
# with that file alone and nothing else sourcing lake.env changes identity (sp-77eatm).
set -euo pipefail

INSTANCE="${1:?usage: nessie-backup-config.sh <instance> [--with-credential | --with-credential-file <path> [--profile NAME]]}"
shift
WITH_CRED=0; CRED_FILE=""; PROFILE="pkdump-lake-backup"
while [ $# -gt 0 ]; do
    case "$1" in
    --with-credential) WITH_CRED=1 ;;
    --with-credential-file) CRED_FILE="${2:?--with-credential-file needs a path}"; shift ;;
    --profile) PROFILE="${2:?--profile needs a name}"; shift ;;
    *) echo "nessie-backup-config: unknown argument '$1'" >&2; exit 1 ;;
    esac
    shift
done
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
if [ -n "$CRED_FILE" ]; then
    [ "$WITH_CRED" = 0 ] || die "--with-credential and --with-credential-file are exclusive"
    [ -r "$CRED_FILE" ] || die "${CRED_FILE} is not a readable file"
    grep -q "^\[${PROFILE}\]" "$CRED_FILE" || die "${CRED_FILE} has no [${PROFILE}] profile"
    set_var PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE "$CRED_FILE"
    set_var PKDUMP_NESSIE_BACKUP_PROFILE "$PROFILE"
fi
set_var PKDUMP_NESSIE_BACKUP_URL "$URL"
set_var AWS_REGION "$REGION"
chmod 600 "$LAKE_ENV"
echo "nessie-backup-config: ${LAKE_ENV} now names ${URL} (${REGION})"
