output "bucket_name" {
  value = aws_s3_bucket.backups.bucket
}

output "bucket_region" {
  value = var.aws_region
}

output "backup_url" {
  description = "PKDUMP_NESSIE_BACKUP_URL"
  value       = "s3://${aws_s3_bucket.backups.bucket}/${local.prefix}"
}

output "credential_secret_name" {
  value = aws_secretsmanager_secret.backup.name
}
