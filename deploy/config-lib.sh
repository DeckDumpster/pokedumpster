#!/usr/bin/env bash
# Scaffold per-instance config files the Quadlet units require.
# Every function here is idempotent: it writes only when the file is absent,
# so it is safe to call on every setup AND every deploy (db-g6ku).

# Write the Cloudflare Access / multi-tenant config file for <instance> into
# <conf_dir> if it does not already exist. All active keys are commented out,
# so a fresh instance stays single-tenant until the operator opts in.
# The Quadlet unit loads this with 'EnvironmentFile=...' (no leading '-', which
# Quadlet does not support), so the file must always exist before the unit
# starts.
#
# Usage: pkdump_scaffold_access_env <instance> <conf_dir>
pkdump_scaffold_access_env() {
    local instance="$1" conf_dir="$2"
    if [ ! -f "${conf_dir}/access.env" ]; then
        mkdir -p "$conf_dir"
        cat > "${conf_dir}/access.env" <<EOF
# Cloudflare Access config for instance '${instance}'.
# Uncomment PKDUMP_MULTITENANT and the three values to enable multi-tenant
# mode. Leave them commented to stay single-tenant.
#
# Steps:
# 1. In the Cloudflare Zero Trust dashboard, open Access > Applications.
# 2. Find the PokeDumpster application and copy its AUD tag (64-char hex).
# 3. Uncomment and fill in the three values below, then uncomment PKDUMP_MULTITENANT.
# 4. Redeploy to pick up the new file: bash deploy/deploy.sh ${instance}
#
# Note: systemd EnvironmentFile does not strip trailing # comments from value
# lines. Put comments on their own lines, as shown here.
#PKDUMP_MULTITENANT=1
# Team domain — e.g. https://myteam.cloudflareaccess.com
#PKDUMP_ACCESS_TEAM_DOMAIN=CHANGE_ME
# 64-char hex AUD tag from Access > Applications
#PKDUMP_ACCESS_AUD=CHANGE_ME
# JWKS URL — e.g. https://myteam.cloudflareaccess.com/cdn-cgi/access/certs
#PKDUMP_ACCESS_JWKS_URL=CHANGE_ME
EOF
        chmod 600 "${conf_dir}/access.env"
        echo "    Wrote ${conf_dir}/access.env — fill CHANGE_ME values to enable multi-tenant mode."
        echo "    See deploy/TENANTS.md §\"Configuring Access for a deployment\" for instructions."
    fi
}

# Write the Pocket ID sign-in file for <instance> into <conf_dir> if it does not exist, every
# key commented out: unconfigured, the server signs nobody in itself (sp-eh0n7z). Any active
# PKDUMP_OIDC_* line turns native sign-in on and an incomplete set refuses to start, so the
# fill is deploy/oidc-config.sh, which writes all of them at once.
#
# Usage: pkdump_scaffold_oidc_env <instance> <conf_dir>
pkdump_scaffold_oidc_env() {
    local instance="$1" conf_dir="$2"
    if [ ! -f "${conf_dir}/oidc.env" ]; then
        mkdir -p "$conf_dir"
        cat > "${conf_dir}/oidc.env" <<EOF2
# Pocket ID sign-in for instance '${instance}'. Fill it with: bash deploy/oidc-config.sh ${instance}
# Put comments on their own lines: systemd EnvironmentFile keeps a trailing # comment as part
# of the value.
#PKDUMP_OIDC_ISSUER=CHANGE_ME
#PKDUMP_OIDC_CLIENT_ID=CHANGE_ME
#PKDUMP_OIDC_CLIENT_SECRET=CHANGE_ME
#PKDUMP_OIDC_REDIRECT_URL=CHANGE_ME
#PKDUMP_OIDC_REQUIRED_GROUP=poke-users
#PKDUMP_OIDC_COOKIE_KEY=CHANGE_ME
EOF2
        chmod 600 "${conf_dir}/oidc.env"
        echo "    Wrote ${conf_dir}/oidc.env — commented out; bash deploy/oidc-config.sh ${instance} fills it."
    fi
}
