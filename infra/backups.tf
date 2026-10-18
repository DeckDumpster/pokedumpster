# The Nessie (lake catalog) backup bucket. deploy/nessie-backup.sh writes
# nessie/<id>/export.zip and nessie/<id>/manifest.json here. It is a different bucket from
# the lake and from the Litestream bucket, and the job refuses a URL in the lake bucket.
locals {
  prefix = "nessie"
}

resource "aws_s3_bucket" "backups" {
  bucket = "pkdump-lake-backups"

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_public_access_block" "backups" {
  bucket                  = aws_s3_bucket.backups.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "backups" {
  bucket = aws_s3_bucket.backups.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_versioning" "backups" {
  bucket = aws_s3_bucket.backups.id

  versioning_configuration {
    status = "Enabled"
  }
}

# The job itself keeps the newest 14 backups (PKDUMP_NESSIE_BACKUP_KEEP) by deleting the
# rest; with versioning on, that delete leaves a noncurrent version, which this expires.
resource "aws_s3_bucket_lifecycle_configuration" "backups" {
  bucket = aws_s3_bucket.backups.id

  rule {
    id     = "expire-noncurrent"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.noncurrent_days
    }

    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }

  depends_on = [aws_s3_bucket_versioning.backups]
}

data "aws_iam_policy_document" "tls_only" {
  statement {
    sid       = "DenyInsecureTransport"
    effect    = "Deny"
    actions   = ["s3:*"]
    resources = [aws_s3_bucket.backups.arn, "${aws_s3_bucket.backups.arn}/*"]

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "backups" {
  bucket     = aws_s3_bucket.backups.id
  policy     = data.aws_iam_policy_document.tls_only.json
  depends_on = [aws_s3_bucket_public_access_block.backups]
}

resource "aws_iam_user" "backup" {
  name                 = "pkdump-lake-backup"
  permissions_boundary = var.permissions_boundary_arn
  tags                 = { terraform-deploy-boundary = "true" }
}

# Put/get/list/delete under nessie/ only: the job writes, reads back to verify, prunes to
# the newest 14, and the drill/check read. No bucket-level or lifecycle actions.
data "aws_iam_policy_document" "backup" {
  statement {
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.backups.arn]

    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = ["${local.prefix}/*", local.prefix]
    }
  }

  statement {
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.backups.arn}/${local.prefix}/*"]
  }
}

resource "aws_iam_user_policy" "backup" {
  name   = "pkdump-lake-backups-rw"
  user   = aws_iam_user.backup.name
  policy = data.aws_iam_policy_document.backup.json
}

resource "aws_iam_access_key" "backup" {
  user = aws_iam_user.backup.name
}

resource "aws_secretsmanager_secret" "backup" {
  name        = "pkdump/lake-backup"
  description = "S3 credential for pkdump-lake-backups (deploy/nessie-backup-config.sh reads it)"
}

resource "aws_secretsmanager_secret_version" "backup" {
  secret_id = aws_secretsmanager_secret.backup.id

  secret_string = jsonencode({
    AWS_ACCESS_KEY_ID     = aws_iam_access_key.backup.id
    AWS_SECRET_ACCESS_KEY = aws_iam_access_key.backup.secret
  })
}
