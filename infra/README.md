# infra/ — the Nessie backup bucket (sp-3mtowz)

Terraform for `pkdump-lake-backups`: private, SSE-S3, versioned, noncurrent versions expire
after 30 days, TLS-only, and one IAM user (`pkdump-lake-backup`, deploy boundary + tag)
limited to put/get/list/delete under `nessie/`. Its key pair is stored in Secrets Manager as
`pkdump/lake-backup`. Same account and conventions as `cubby-prod/infra`.

```bash
cd infra
terraform init -backend-config="bucket=<state-bucket>" -backend-config="role_arn=<deploy-role>"
terraform test                       # hermetic, mocked provider
terraform apply -var deploy_role_arn=<DeployRoleArn>   # MFA session
bash ../deploy/nessie-backup-config.sh prod            # writes lake.env on the host
bash ../deploy/nessie-backup.sh prod backup && bash ../deploy/nessie-backup.sh prod drill
```

The `pkdump` prefix must be in the terraform-deploy role's managed-prefix list
(ephemeral-ci `TerraformBootstrap`) for the apply to be allowed to create these names.

## infra/oidc/ — native sign-in for poke.dumpster.cards (sp-eh0n7z)

Own state (`pokedumpster/oidc/terraform.tfstate`). Makes the Pocket ID client `pokedumpster-prod`
(callback `https://poke.dumpster.cards/auth/callback`, PKCE, limited to `poke-users`) and stores its
id and secret in Secrets Manager `pkdump/prod/oidc`; no output carries a secret. Needs
`POCKETID_API_TOKEN` in the environment and `pocketid_group_ids` = the id of `poke-users`.

```bash
cd infra/oidc && terraform init -backend-config=... && terraform apply -var deploy_role_arn=... -var 'pocketid_group_ids=["<id>"]'
bash deploy/oidc-config.sh prod <ssh-host>   # run HERE (state + deploy role), not on the prod host:
                                    # writes <host>:~/.config/pkdump/prod/oidc.env (600), keeps its cookie key,
                                    # and probe.env (600): the host health-probe machine client (sp-396d3g)
ssh <ssh-host> 'cd ~/pokedumpster && bash deploy/deploy.sh prod'   # native sign-in on
```
