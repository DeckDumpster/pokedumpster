# Names and ids only: nothing sensitive is an output.
output "pocketid_client_id" {
  value = pocketid_client.this.id
}

output "secret_name" {
  value = aws_secretsmanager_secret.oidc.name
}

output "redirect_url" {
  value = "https://${var.hostname}/auth/callback"
}
