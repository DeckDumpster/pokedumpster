# Plan-free assertions with mocked providers: no credentials, no network.
mock_provider "aws" {}
mock_provider "pocketid" {}

variables {
  deploy_role_arn    = "arn:aws:iam::123456789012:role/terraform-deploy"
  pocketid_group_ids = ["group-poke-users"]
}

run "client_is_pkce_and_group_limited" {
  command = plan

  assert {
    condition     = pocketid_client.this.callback_urls == tolist(["https://poke.dumpster.cards/auth/callback"])
    error_message = "the callback must be poke's /auth/callback"
  }
  assert {
    condition     = pocketid_client.this.pkce_enabled
    error_message = "PKCE must be on"
  }
  assert {
    condition     = pocketid_client.this.allowed_user_groups == tolist(["group-poke-users"])
    error_message = "the client must be limited to the poke-users group"
  }
}

run "no_group_is_refused" {
  command = plan
  variables {
    pocketid_group_ids = []
  }
  expect_failures = [var.pocketid_group_ids]
}

run "the_health_probe_is_a_machine_client" {
  command = plan

  assert {
    condition     = pocketid_client.health_probe.callback_urls == tolist([])
    error_message = "a client_credentials client has no redirect URIs"
  }
  assert {
    condition     = !pocketid_client.health_probe.pkce_enabled && !pocketid_client.health_probe.is_public
    error_message = "the probe is a confidential client without PKCE"
  }
}
