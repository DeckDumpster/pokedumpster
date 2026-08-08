//! `pkdump tenant` — the operator surface over the user registry.
//!
//! A user is two facts joined by a row: the `handle` they are named by, and
//! the opaque `database_id` their collection lives under. These commands are
//! the four things an operator does to that row —
//!
//! ```text
//! pkdump tenant create <handle>            # mint an id, write the database
//! pkdump tenant list                       # who is registered, and where
//! pkdump tenant rename <from> <to>         # one column; nothing on disk moves
//! pkdump tenant detach <handle> --yes      # release the handle, KEEP the data
//! pkdump tenant purge <database-id> --yes  # destroy a detached collection
//! ```
//!
//! **`remove` is now an alias for `detach`, and no longer deletes anything.**
//! It used to unlink the database. It now releases the handle and leaves the
//! file and its replica in place; destroying them is the separate, explicit
//! `purge`, addressed by `database_id` so it cannot be reached by mistyping a
//! live person's name. See `deploy/TENANTS.md`.
//!
//! `list` is not garnish. Once databases are named by ULID a directory
//! listing tells you only how many collections exist — this is the only thing
//! that says whose they are.
//!
//! `adopt` / `revert` are the migration and its rollback for a data directory
//! laid out before `tenants/` existed — see `deploy/TENANTS.md` for the
//! runbook.
//!
//! Paths come from `$PKDUMP_HOME`, so `podman exec <ctr> pkdump tenant …`
//! works against a running instance the same way `pkdump db` does.

use pkdump_db::tenants::{self, Tenant};

/// Arguments for `pkdump tenant`.
#[derive(clap::Args)]
pub struct TenantArgs {
    #[command(subcommand)]
    command: TenantCommand,
}

#[derive(clap::Subcommand)]
enum TenantCommand {
    /// Register a user and provision their collection database.
    Create(NameArgs),
    /// List every registered user and the database they map to.
    List,
    /// Rename a user. Their database, replica and history are untouched.
    Rename(RenameArgs),
    /// Release a handle, keeping the collection and its replica.
    #[command(alias = "remove")]
    Detach(DetachArgs),
    /// Destroy a detached user's collection database. Irreversible.
    Purge(PurgeArgs),
    /// Move a pre-`tenants/` collection database into the tenant layout.
    Adopt(OptionalNameArgs),
    /// Roll `adopt` back: move the database out of `tenants/` again.
    Revert(OptionalNameArgs),
}

#[derive(clap::Args)]
pub struct NameArgs {
    /// Handle: `a-z`, `0-9`, `-` and `_`, starting with a letter or digit.
    /// It names the user; it does NOT name their file.
    handle: String,
}

#[derive(clap::Args)]
pub struct RenameArgs {
    /// The handle as it is now.
    from: String,
    /// The handle to move it to. Must be free.
    to: String,
}

#[derive(clap::Args)]
pub struct DetachArgs {
    /// User to release.
    handle: String,

    /// Confirm. Without it nothing is detached.
    #[arg(long)]
    yes: bool,
}

#[derive(clap::Args)]
pub struct PurgeArgs {
    /// The `database_id` to destroy — as printed by `pkdump tenant list`.
    /// Not a handle: a purge is not something to reach by typo.
    database_id: String,

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
            let t = tenants::create(&a.handle)?;
            println!(
                "Created user {} -> database {} at {}",
                t.user.handle,
                t.user.database_id,
                t.path.display()
            );
        }
        TenantCommand::List => list()?,
        TenantCommand::Rename(a) => {
            let t = tenants::rename(&a.from, &a.to)?;
            println!(
                "Renamed {} -> {}; database {} unchanged at {}",
                a.from,
                t.user.handle,
                t.user.database_id,
                t.path.display()
            );
        }
        TenantCommand::Detach(a) => {
            if !a.yes {
                anyhow::bail!("refusing to release the handle {} without --yes", a.handle);
            }
            let t = tenants::detach(&a.handle)?;
            println!(
                "Detached {}. The handle is free; the collection was KEPT.\n\
                 \x20 database {} at {}\n\
                 \x20 to destroy it: pkdump tenant purge {} --yes",
                a.handle,
                t.user.database_id,
                t.path.display(),
                t.user.database_id
            );
        }
        TenantCommand::Purge(a) => {
            if !a.yes {
                anyhow::bail!(
                    "refusing to destroy the collection in database {} without --yes",
                    a.database_id
                );
            }
            let user = tenants::purge(&a.database_id)?;
            println!(
                "Purged database {} (last held by {}). \
                 Its S3 replica outlives it until retention expires.",
                user.database_id, user.handle
            );
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

/// `pkdump tenant list` — the registry as a table, plus anything on disk the
/// registry cannot account for.
fn list() -> anyhow::Result<()> {
    let tenants = tenants::list()?;
    let unregistered = tenants::unregistered()?;

    if tenants.is_empty() {
        println!(
            "No users registered in {}",
            pkdump_db::registry_db_path()?.display()
        );
    } else {
        // Measured, not guessed: a retired handle carries the id it was
        // retired with, and a timestamp is as long as the registry wrote it.
        let widest = |header: &str, of: fn(&Tenant) -> &str| {
            tenants
                .iter()
                .map(|t| of(t).len())
                .max()
                .unwrap_or(0)
                .max(header.len())
        };
        let handle = widest("HANDLE", |t| &t.user.handle);
        let id = widest("DATABASE ID", |t| &t.user.database_id);
        let created = widest("CREATED", |t| &t.user.created_at);
        println!(
            "{:<handle$}  {:<id$}  {:<created$}  STATE",
            "HANDLE", "DATABASE ID", "CREATED"
        );
        for t in &tenants {
            println!(
                "{:<handle$}  {:<id$}  {:<created$}  {}{}",
                t.user.handle,
                t.user.database_id,
                t.user.created_at,
                t.user.state.as_str(),
                if t.present {
                    ""
                } else {
                    "  (DATABASE MISSING)"
                }
            );
        }
    }

    if !unregistered.is_empty() {
        println!(
            "\n{} database(s) under {} that no registered user claims:",
            unregistered.len(),
            pkdump_db::tenants_dir()?.display()
        );
        for stem in unregistered {
            println!("  {stem}.sqlite");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Just enough of the command tree to parse `tenant …` argv.
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Only,
    }

    #[derive(clap::Subcommand)]
    enum Only {
        Tenant(TenantArgs),
    }

    fn parse(argv: &[&str]) -> TenantCommand {
        let mut full = vec!["pkdump", "tenant"];
        full.extend_from_slice(argv);
        let Only::Tenant(args) = Cli::parse_from(full).command;
        args.command
    }

    /// The compatibility promise, and the trap: an operator's muscle memory
    /// and every script that says `tenant remove <name> --yes` still parses
    /// — and now detaches instead of deleting.
    #[test]
    fn remove_is_an_alias_for_detach() {
        let TenantCommand::Detach(a) = parse(&["remove", "alice", "--yes"]) else {
            panic!("`tenant remove` must still parse, as a detach");
        };
        assert_eq!(a.handle, "alice");
        assert!(a.yes);

        let TenantCommand::Detach(b) = parse(&["detach", "alice", "--yes"]) else {
            panic!("`tenant detach` must parse");
        };
        assert_eq!(b.handle, "alice");
    }

    /// Purge takes a `database_id`, not a handle. Naming a person is how a
    /// destructive command gets reached by typo.
    #[test]
    fn purge_takes_a_database_id() {
        let TenantCommand::Purge(a) = parse(&["purge", "01J0000000000000000000000Z", "--yes"])
        else {
            panic!("`tenant purge` must parse");
        };
        assert_eq!(a.database_id, "01J0000000000000000000000Z");
        assert!(a.yes);
    }

    #[test]
    fn rename_takes_two_handles() {
        let TenantCommand::Rename(a) = parse(&["rename", "alice", "alicia"]) else {
            panic!("`tenant rename` must parse");
        };
        assert_eq!((a.from.as_str(), a.to.as_str()), ("alice", "alicia"));
    }
}
