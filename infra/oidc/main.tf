# Native sign-in for poke.dumpster.cards (sp-eh0n7z): a Pocket ID OIDC client, ported from
# dumpstar's infra/edge. There is deliberately no Cloudflare resource here: the Access app in
# front of poke is the identity repo's, and stays until it flips it to service-only.

resource "pocketid_client" "this" {
  name                 = "pokedumpster-prod"
  callback_urls        = ["https://${var.hostname}/auth/callback"]
  logout_callback_urls = ["https://${var.hostname}/"]
  allowed_user_groups  = var.pocketid_group_ids
  pkce_enabled         = true
}

# The client id and secret go to Secrets Manager and are never an output, so no plan or apply
# log carries them. deploy/oidc-config.sh reads them into oidc.env on the prod host; the cookie
# key is generated there and never leaves it.
resource "aws_secretsmanager_secret" "oidc" {
  name = var.secret_name
}

resource "aws_secretsmanager_secret_version" "oidc" {
  secret_id = aws_secretsmanager_secret.oidc.id
  secret_string = jsonencode({
    PKDUMP_OIDC_CLIENT_ID     = pocketid_client.this.id
    PKDUMP_OIDC_CLIENT_SECRET = pocketid_client.this.client_secret
  })

  # client_id is an optional input of pocketid_client, null when unset: the generated
  # identifier is .id. An empty value must stop the apply here, not a deploy.
  lifecycle {
    precondition {
      condition = alltrue([for v in [
        pocketid_client.this.id,
        pocketid_client.this.client_secret,
      ] : v != null && v != ""])
      error_message = "the OIDC secret would carry an empty client id or client secret"
    }
  }
}
