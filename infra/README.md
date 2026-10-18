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
