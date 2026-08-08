//! `pkdump tenant` — provision the per-tenant collection databases.
//!
//! One command creates a tenant, one removes it; both are thin wrappers
//! over `pkdump_db::tenants`. `adopt` / `revert` are the migration and its
//! rollback for a data directory laid out before `tenants/` existed — see
//! `deploy/TENANTS.md` for the runbook.
//!
//! Paths come from `$PKDUMP_HOME`, so `podman exec <ctr> pkdump tenant …`
//! works against a running instance the same way `pkdump db` does.

use pkdump_db::tenants;

/// Arguments for `pkdump tenant`.
#[derive(clap::Args)]
pub struct TenantArgs {
    #[command(subcommand)]
    command: TenantCommand,
}

#[derive(clap::Subcommand)]
enum TenantCommand {
    /// Create a tenant: a new collection database under `tenants/`.
    Create(NameArgs),
    /// List every tenant on this box.
    List,
    /// Delete a tenant's collection database. Destructive.
    Remove(RemoveArgs),
    /// Move a pre-`tenants/` collection database into the tenant layout.
    Adopt(OptionalNameArgs),
    /// Roll `adopt` back: move the database out of `tenants/` again.
    Revert(OptionalNameArgs),
}

#[derive(clap::Args)]
pub struct NameArgs {
    /// Tenant name: `a-z`, `0-9`, `-` and `_`, starting with a letter or
    /// digit. It becomes a filename and an S3 replica-path component.
    name: String,
}

#[derive(clap::Args)]
pub struct RemoveArgs {
    /// Tenant to delete.
    name: String,

    /// Confirm the deletion. Without it nothing is removed.
    #[arg(long)]
    yes: bool,
}

#[derive(clap::Args)]
pub struct OptionalNameArgs {
    /// Tenant to move. Defaults to `$PKDUMP_USER` (else `collection`) —
    /// the single user a pre-tenants data directory has.
    name: Option<String>,
}

/// Execute `pkdump tenant`.
pub fn run(args: TenantArgs) -> anyhow::Result<()> {
    match args.command {
        TenantCommand::Create(a) => {
            let path = tenants::create(&a.name)?;
            println!("Created tenant {} at {}", a.name, path.display());
        }
        TenantCommand::List => {
            let names = tenants::list()?;
            if names.is_empty() {
                println!("No tenants in {}", pkdump_db::tenants_dir()?.display());
            }
            for name in names {
                println!("{name}");
            }
        }
        TenantCommand::Remove(a) => {
            if !a.yes {
                anyhow::bail!(
                    "refusing to delete tenant {}'s collection without --yes",
                    a.name
                );
            }
            let path = tenants::remove(&a.name)?;
            println!("Removed tenant {} ({})", a.name, path.display());
        }
        TenantCommand::Adopt(a) => {
            let name = a.name.unwrap_or_else(pkdump_db::current_user);
            let path = tenants::adopt(&name)?;
            println!("Adopted tenant {name} -> {}", path.display());
        }
        TenantCommand::Revert(a) => {
            let name = a.name.unwrap_or_else(pkdump_db::current_user);
            let path = tenants::revert(&name)?;
            println!("Reverted tenant {name} -> {}", path.display());
        }
    }
    Ok(())
}
