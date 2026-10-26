#!/usr/bin/env bash
# Fill <conf>/oidc.env with the Pocket ID client Terraform made (infra/oidc, sp-eh0n7z), so
# `pkdump serve` signs users in itself. Run on the prod host after `terraform apply`, then
# `bash deploy/deploy.sh <instance>`.
#
#   bash deploy/oidc-config.sh <instance>
#
# The client id and secret are read from Secrets Manager; the cookie key is generated here
# the first time and KEPT thereafter (rotating it signs everybody out). Nothing is printed.
set -euo pipefail

INSTANCE="${1:?usage: oidc-config.sh <instance>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONF_DIR="${PKDUMP_CONF_DIR:-${HOME}/.config/pkdump/${INSTANCE}}"
INFRA="${PKDUMP_OIDC_INFRA_DIR:-${HERE}/../infra/oidc}"
ENV_FILE="${CONF_DIR}/oidc.env"
ISSUER="${PKDUMP_OIDC_ISSUER_VALUE:-https://id.gantt.pub}"
GROUP="${PKDUMP_OIDC_REQUIRED_GROUP_VALUE:-poke-users}"
die() { echo "oidc-config: $*" >&2; exit 1; }

SECRET_ID="${PKDUMP_OIDC_SECRET_NAME:-$(terraform -chdir="$INFRA" output -raw secret_name)}"
REDIRECT="${PKDUMP_OIDC_REDIRECT_VALUE:-$(terraform -chdir="$INFRA" output -raw redirect_url)}"
SECRET="$(aws secretsmanager get-secret-value --secret-id "$SECRET_ID" --query SecretString --output text)"
CLIENT_ID="$(jq -er .PKDUMP_OIDC_CLIENT_ID <<<"$SECRET")" || die "${SECRET_ID} has no client id"
CLIENT_SECRET="$(jq -er .PKDUMP_OIDC_CLIENT_SECRET <<<"$SECRET")" || die "${SECRET_ID} has no client secret"
[ -n "$CLIENT_ID" ] && [ -n "$CLIENT_SECRET" ] || die "${SECRET_ID} carries an empty value"

COOKIE_KEY=""
if [ -f "$ENV_FILE" ]; then
    COOKIE_KEY="$(sed -n 's/^PKDUMP_OIDC_COOKIE_KEY=//p' "$ENV_FILE" | head -n1)"
fi
[ -n "$COOKIE_KEY" ] && [ "$COOKIE_KEY" != CHANGE_ME ] || COOKIE_KEY="$(openssl rand -hex 32)"

mkdir -p "$CONF_DIR"
tmp="$(mktemp "${ENV_FILE}.XXXXXX")"
umask 077
{
    echo "# Pocket ID sign-in for instance '${INSTANCE}', written by deploy/oidc-config.sh."
    echo "PKDUMP_OIDC_ISSUER=${ISSUER}"
    echo "PKDUMP_OIDC_CLIENT_ID=${CLIENT_ID}"
    echo "PKDUMP_OIDC_CLIENT_SECRET=${CLIENT_SECRET}"
    echo "PKDUMP_OIDC_REDIRECT_URL=${REDIRECT}"
    echo "PKDUMP_OIDC_REQUIRED_GROUP=${GROUP}"
    echo "PKDUMP_OIDC_COOKIE_KEY=${COOKIE_KEY}"
} >"$tmp"
chmod 600 "$tmp"
mv "$tmp" "$ENV_FILE"
echo "oidc-config: ${ENV_FILE} written (client ${CLIENT_ID}); deploy to turn sign-in on: bash deploy/deploy.sh ${INSTANCE}"
