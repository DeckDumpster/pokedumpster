terraform {
  required_version = ">= 1.10"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "6.69.1"
    }
    # Same community provider dumpstar's edge uses; Pocket ID ships none of its own.
    pocketid = {
      source  = "Trozz/pocketid"
      version = "2.4.2"
    }
  }

  # Its own state, apart from ../ (the backup bucket): a sign-in change must never plan
  # against the lake's backups. Bucket and role come from -backend-config (see ../README.md).
  backend "s3" {
    key          = "pokedumpster/oidc/terraform.tfstate"
    region       = "us-west-2"
    encrypt      = true
    use_lockfile = true
  }
}

provider "aws" {
  region = var.aws_region

  assume_role {
    role_arn = var.deploy_role_arn
  }
}

# POCKETID_API_TOKEN (a Pocket ID admin API key) comes from the environment, never a file in git.
provider "pocketid" {
  base_url = var.pocketid_url
}
