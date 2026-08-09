//! `pkdump tenant` — provision the per-tenant collection databases.
//!
//! One command creates a tenant, one removes it; both are thin wrappers
//! over `pkdump_db::tenants`. `adopt` / `revert` are the migration and its
//! rollback for a data directory laid out before `tenants/` existed — see
//! `deploy/TENANTS.md` for the runbook.
//!
//! `list` is also the schema-drift report: every tenant carries its own
//! `PRAGMA user_version` and they can legitimately differ, so the listing
//! names each one's version and where it stands relative to this build
//! (pd-enje).
//!
//! Paths come from `$PKDUMP_HOME`, so `podman exec <ctr> pkdump tenant …`
//! works against a running instance the same way `pkdump db` does.

use pkdump_db::schema_version::{Database, SchemaState};
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
        TenantCommand::List => list()?,
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

/// `pkdump tenant list` — every tenant, with the schema version its own
/// collection database carries (pd-enje).
///
/// The version is a column rather than a separate command because drift is
/// only visible in comparison: one database per tenant means they can
/// legitimately differ, and "which of my N databases are behind" is a
/// question about the whole list. Each row still starts with the name, so
/// the old one-name-per-line output is a prefix of this one.
///
/// A tenant this build would refuse to OPEN is listed like any other — it
/// is the row an operator whose server will not start is here to find.
fn list() -> anyhow::Result<()> {
    let tenants = tenants::versions()?;
    if tenants.is_empty() {
        println!("No tenants in {}", pkdump_db::tenants_dir()?.display());
        return Ok(());
    }

    let known = Database::User.version();
    // Sized to the longest name so the columns line up without wrapping the
    // 32-character maximum a tenant name can reach.
    let width = tenants
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(0)
        .max(4);
    println!("{:<width$}  SCHEMA  STATUS", "NAME");
    for t in &tenants {
        let status = match t.state() {
            SchemaState::Current => "current".to_string(),
            SchemaState::Behind => {
                format!("behind this build's {known} — adopted on its next open")
            }
            SchemaState::Ahead => {
                format!("ahead of this build's {known} — this build refuses to open it")
            }
        };
        println!("{:<width$}  {:>6}  {status}", t.name, t.version);
    }
    Ok(())
}
