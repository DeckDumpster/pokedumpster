#!/usr/bin/env bash
# Fill <conf>/oidc.env and probe.env on the prod host with the Pocket ID clients Terraform made
# (infra/oidc, sp-eh0n7z), so `pkdump serve` signs users in itself. Run it where the Terraform
# state and the deploy role's credentials are (the operator box), NOT on the prod host: the host
# has no Terraform and, by design, no access to the pkdump secrets (sp-v5f004). The two files
# are written to the host over ssh; then `bash deploy/deploy.sh <instance>` there.
#
#   bash deploy/oidc-config.sh <instance> <ssh-host>
#
# The client ids and secrets are read from Secrets Manager here; the cookie key is read from the
# host's existing oidc.env and KEPT (rotating it signs everybody out), generated only the first
# time. Files land 0600 via a temp file and rename. Nothing secret is printed.
set -euo pipefail

INSTANCE="${1:?usage: oidc-config.sh <instance> <ssh-host>}"
HOST="${2:?usage: oidc-config.sh <instance> <ssh-host>}"
case "$INSTANCE" in *[!A-Za-z0-9_-]*|"") echo "oidc-config: bad instance name" >&2; exit 1 ;; esac
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Expanded by the REMOTE shell, so the default is the host's own home.
CONF_DIR="${PKDUMP_REMOTE_CONF_DIR:-\$HOME/.config/pkdump/${INSTANCE}}"
INFRA="${PKDUMP_OIDC_INFRA_DIR:-${HERE}/../infra/oidc}"
ENV_FILE="${CONF_DIR}/oidc.env"
PROBE_FILE="${CONF_DIR}/probe.env"
ISSUER="${PKDUMP_OIDC_ISSUER_VALUE:-https://id.gantt.pub}"
GROUP="${PKDUMP_OIDC_REQUIRED_GROUP_VALUE:-poke-users}"
die() { echo "oidc-config: $*" >&2; exit 1; }
remote() { ssh -o BatchMode=yes "$HOST" "$@"; }
SECRET_ID="${PKDUMP_OIDC_SECRET_NAME:-$(terraform -chdir="$INFRA" output -raw secret_name)}"
REDIRECT="${PKDUMP_OIDC_REDIRECT_VALUE:-$(terraform -chdir="$INFRA" output -raw redirect_url)}"
SECRET="$(aws secretsmanager get-secret-value --secret-id "$SECRET_ID" --query SecretString --output text)"
CLIENT_ID="$(jq -er .PKDUMP_OIDC_CLIENT_ID <<<"$SECRET")" || die "${SECRET_ID} has no client id"
CLIENT_SECRET="$(jq -er .PKDUMP_OIDC_CLIENT_SECRET <<<"$SECRET")" || die "${SECRET_ID} has no client secret"
[ -n "$CLIENT_ID" ] && [ -n "$CLIENT_SECRET" ] || die "${SECRET_ID} carries an empty value"

PROBE_SECRET_ID="${PKDUMP_PROBE_SECRET_NAME:-$(terraform -chdir="$INFRA" output -raw probe_secret_name)}"
PROBE="$(aws secretsmanager get-secret-value --secret-id "$PROBE_SECRET_ID" --query SecretString --output text)"
PROBE_ID="$(jq -er .PKDUMP_PROBE_CLIENT_ID <<<"$PROBE")" || die "${PROBE_SECRET_ID} has no client id"
PROBE_SECRET="$(jq -er .PKDUMP_PROBE_CLIENT_SECRET <<<"$PROBE")" || die "${PROBE_SECRET_ID} has no client secret"
[ -n "$PROBE_ID" ] && [ -n "$PROBE_SECRET" ] || die "${PROBE_SECRET_ID} carries an empty value"

COOKIE_KEY=""
existing="$(remote "cat \"${ENV_FILE}\" 2>/dev/null || true")" || die "cannot reach ${HOST}"
COOKIE_KEY="$(sed -n 's/^PKDUMP_OIDC_COOKIE_KEY=//p' <<<"$existing" | head -n1)"
[ -n "$COOKIE_KEY" ] && [ "$COOKIE_KEY" != CHANGE_ME ] || COOKIE_KEY="$(openssl rand -hex 32)"

# put <path>: stdin -> <path> on the host, 0600, via a temp file in the same directory + rename.
put() {
    remote "umask 077; mkdir -p \"${CONF_DIR}\" && tmp=\$(mktemp \"$1.XXXXXX\") && cat >\"\$tmp\" && chmod 600 \"\$tmp\" && mv \"\$tmp\" \"$1\""
}

{
    echo "# Pocket ID sign-in for instance '${INSTANCE}', written by deploy/oidc-config.sh."
    echo "PKDUMP_OIDC_ISSUER=${ISSUER}"
    echo "PKDUMP_OIDC_CLIENT_ID=${CLIENT_ID}"
    echo "PKDUMP_OIDC_CLIENT_SECRET=${CLIENT_SECRET}"
    echo "PKDUMP_OIDC_REDIRECT_URL=${REDIRECT}"
    echo "PKDUMP_OIDC_REQUIRED_GROUP=${GROUP}"
    echo "PKDUMP_OIDC_COOKIE_KEY=${COOKIE_KEY}"
    # The host's probe client may reach /health with a bearer, and nothing else (sp-396d3g).
    echo "PKDUMP_OIDC_BEARER_CLIENT_IDS=${PROBE_ID}"
    echo "PKDUMP_OIDC_BEARER_AUDIENCE=${PROBE_ID}"
} | put "$ENV_FILE"

# The probe's own credentials live in a separate 0600 file: oidc.env is loaded into the app
# container, and the app must never hold the secret that proves it is up.
{
    echo "# Host health-probe client for instance '${INSTANCE}', written by deploy/oidc-config.sh."
    echo "PKDUMP_PROBE_CLIENT_ID=${PROBE_ID}"
    echo "PKDUMP_PROBE_CLIENT_SECRET=${PROBE_SECRET}"
    echo "PKDUMP_PROBE_TOKEN_URL=${ISSUER}/api/oidc/token"
    echo "PKDUMP_PROBE_AUDIENCE=${PROBE_ID}"
} | put "$PROBE_FILE"
echo "oidc-config: ${HOST}:${PROBE_FILE} written (probe client ${PROBE_ID})"
echo "oidc-config: ${HOST}:${ENV_FILE} written (client ${CLIENT_ID}); on the host, turn sign-in on: bash deploy/deploy.sh ${INSTANCE}"
