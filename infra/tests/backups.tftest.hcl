# Plan-free assertions with a mocked provider: no credentials, no network.
mock_provider "aws" {
  mock_data "aws_iam_policy_document" {
    defaults = {
      json = "{\"Version\":\"2012-10-17\",\"Statement\":[]}"
    }
  }
}

variables {
  deploy_role_arn = "arn:aws:iam::123456789012:role/terraform-deploy"
}

override_resource {
  target          = aws_s3_bucket.backups
  override_during = plan
  values = {
    arn = "arn:aws:s3:::pkdump-lake-backups"
  }
}

run "bucket_is_private_versioned_retained" {
  command = plan

  assert {
    condition     = aws_s3_bucket.backups.bucket == "pkdump-lake-backups"
    error_message = "bucket must be pkdump-lake-backups"
  }
  assert {
    condition = alltrue([
      aws_s3_bucket_public_access_block.backups.block_public_acls,
      aws_s3_bucket_public_access_block.backups.block_public_policy,
      aws_s3_bucket_public_access_block.backups.ignore_public_acls,
      aws_s3_bucket_public_access_block.backups.restrict_public_buckets,
    ])
    error_message = "all public access must be blocked"
  }
  assert {
    condition     = aws_s3_bucket_versioning.backups.versioning_configuration[0].status == "Enabled"
    error_message = "versioning must be enabled"
  }
  assert {
    condition     = one(aws_s3_bucket_server_side_encryption_configuration.backups.rule).apply_server_side_encryption_by_default[0].sse_algorithm == "AES256"
    error_message = "SSE-S3 required"
  }
  assert {
    condition     = one(aws_s3_bucket_lifecycle_configuration.backups.rule).noncurrent_version_expiration[0].noncurrent_days == 30
    error_message = "noncurrent versions must expire after 30 days"
  }
}

run "credential_is_scoped_to_the_nessie_prefix" {
  command = plan

  assert {
    condition = toset(flatten([for s in data.aws_iam_policy_document.backup.statement : s.actions])) == toset([
      "s3:ListBucket", "s3:GetObject", "s3:PutObject", "s3:DeleteObject",
    ])
    error_message = "actions must be List/Get/Put/Delete only"
  }
  assert {
    condition = alltrue(flatten([
      for s in data.aws_iam_policy_document.backup.statement :
      [for r in s.resources : startswith(r, "arn:aws:s3:::pkdump-lake-backups")]
    ]))
    error_message = "policy resources must be this bucket only"
  }
  assert {
    condition     = aws_iam_user.backup.tags["terraform-deploy-boundary"] == "true" && aws_iam_user.backup.permissions_boundary == var.permissions_boundary_arn
    error_message = "the user must carry the deploy boundary and tag"
  }
  assert {
    condition     = output.backup_url == "s3://pkdump-lake-backups/nessie"
    error_message = "backup_url output"
  }
}
