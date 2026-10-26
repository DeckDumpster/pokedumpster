variable "aws_region" {
  type    = string
  default = "us-west-2"
}

variable "deploy_role_arn" {
  type        = string
  description = "DeployRoleArn output of TerraformBootstrap (role terraform-deploy)."
}

variable "hostname" {
  type    = string
  default = "poke.dumpster.cards"
}

variable "pocketid_url" {
  type    = string
  default = "https://id.gantt.pub"
}

variable "pocketid_group_ids" {
  type        = list(string)
  description = "Pocket ID group ids allowed on the client: the id of group poke-users."

  validation {
    condition     = length(var.pocketid_group_ids) > 0
    error_message = "a client with no allowed group admits every Pocket ID user"
  }
}

variable "secret_name" {
  type    = string
  default = "pkdump/prod/oidc"
}
