//! `pkdump keys` — the operator surface over tenant-zone key custody
//! (`pd-ulds`).
//!
//! ```text
//! pkdump keys init                          # mint the master key, mode 600
//! pkdump keys status                        # where it is, what mode, which key
//! pkdump keys backup --yes                  # THE BACKUP PATH: print it
//! pkdump keys backup -o <file>              #   ...or write a 600 copy
//! pkdump keys restore [-i <file>]           # put a backed-up key back
//! pkdump keys register <database-id>        # this tenant's key may be derived
//! pkdump keys list                          # every database_id and its key state
//! pkdump keys derive <database-id>          # prove derivation, print a fingerprint
//! pkdump keys tombstone <database-id> --yes # THE DESTRUCTION PATH: revoke it
//! ```
//!
//! **`backup` and `tombstone` are different paths and this file keeps them
//! that way.** They share no argument type, no confirmation flag, no output
//! shape and no helper here — `backup` reaches `pkdump_keys::backup`, which
//! cannot see the registry, and `tombstone` reaches `pkdump_keys::destroy`,
//! which cannot see the key file. The reason is the property the whole item
//! turns on: a lost key and a deleted tenant are cryptographically identical,
//! so the operational distinction has to be structural. See
//! `crates/pkdump-keys/tests/separation.rs`.
//!
//! Two things are deliberately awkward:
//!
//! * `backup --yes` is required to put key material on a terminal. Printing a
//!   secret is a decision, not a default.
//! * `derive` prints a **fingerprint**, never the key. Determinism is
//!   checkable on a real box without the key ever reaching a screen, a shell
//!   history or a CI log.
//!
//! Paths come from `$PKDUMP_HOME` and `$PKDUMP_MASTER_KEY_FILE`, so
//! `podman exec <ctr> pkdump keys …` works against a running instance exactly
//! as `pkdump tenant` does. `deploy/keys.sh <instance>` is the wrapper that
//! knows which per-instance file that is.

use pkdump_keys::{KeyState, backup, derive, destroy, master, state};

/// Arguments for `pkdump keys`.
#[derive(clap::Args)]
pub struct KeysArgs {
    #[command(subcommand)]
    command: KeysCommand,
}

#[derive(clap::Subcommand)]
enum KeysCommand {
    /// Generate this box's master key. Refuses to overwrite an existing one.
    Init,
    /// Where the master key is, what mode it carries, and which key it is.
    Status,
    /// THE BACKUP PATH — emit the master key for your password manager.
    Backup(BackupArgs),
    /// Put a backed-up master key back. Refuses over a live key.
    Restore(RestoreArgs),
    /// Record that a database's key may be derived.
    Register(DatabaseIdArgs),
    /// Every database_id this box holds key state for.
    List,
    /// Derive a tenant key and print its fingerprint. Never the key.
    Derive(DatabaseIdArgs),
    /// THE DESTRUCTION PATH — revoke a database's key, permanently.
    Tombstone(TombstoneArgs),
}

#[derive(clap::Args)]
pub struct BackupArgs {
    /// Write a mode-600 copy here instead of printing. Refuses to overwrite.
    #[arg(short = 'o', long)]
    out: Option<std::path::PathBuf>,

    /// Confirm printing secret key material to this terminal. Required
    /// unless `--out` is given: a secret reaching a screen is a decision.
    #[arg(long)]
    yes: bool,
}

#[derive(clap::Args)]
pub struct RestoreArgs {
    /// Read the backed-up key from here. Omit to read it from stdin.
    #[arg(short = 'i', long)]
    input: Option<std::path::PathBuf>,
}

#[derive(clap::Args)]
pub struct DatabaseIdArgs {
    /// The `database_id` — as printed by `pkdump tenant list`. Not a handle:
    /// key state is keyed on the thing that never changes.
    database_id: String,
}

#[derive(clap::Args)]
pub struct TombstoneArgs {
    /// The `database_id` whose key is being destroyed. Not a handle: a
    /// revocation is not something to reach by typo.
    database_id: String,

    /// Why. Recorded with the tombstone, for the audit trail.
    #[arg(long)]
    reason: Option<String>,

    /// Confirm. Without it nothing is revoked.
    #[arg(long)]
    yes: bool,
}

/// Execute `pkdump keys`.
pub fn run(args: KeysArgs) -> anyhow::Result<()> {
    match args.command {
        KeysCommand::Init => init(),
        KeysCommand::Status => status(),
        KeysCommand::Backup(a) => run_backup(a),
        KeysCommand::Restore(a) => run_restore(a),
        KeysCommand::Register(a) => register(a),
        KeysCommand::List => list(),
        KeysCommand::Derive(a) => derive_one(a),
        KeysCommand::Tombstone(a) => run_tombstone(a),
    }
}

fn init() -> anyhow::Result<()> {
    let (path, fingerprint) = master::create()?;
    println!("Master key written to {}", path.display());
    println!("  mode        {:o}", master::mode_of(&path)?);
    println!("  fingerprint {fingerprint}");
    println!();
    println!("BACK THIS UP NOW, the same way the Litestream bootstrap key is backed up:");
    println!("  pkdump keys backup --yes    # then paste it into your password manager");
    println!();
    println!("There is one master key and every tenant's key is derived from it. Losing it");
    println!("makes every tenant's data unreadable at once — with nothing revoked and nobody");
    println!("deleted. That is NOT the same event as a deletion, and must never be treated");
    println!("as one. See deploy/KEYS.md.");
    Ok(())
}

fn status() -> anyhow::Result<()> {
    let Some(path) = master::key_path() else {
        anyhow::bail!(
            "cannot tell where the master key would be: neither {} nor HOME is set",
            master::KEY_ENV_FILE
        );
    };
    println!("Master key file  {}", path.display());

    if !path.exists() {
        println!("  status         ABSENT");
        println!();
        println!("No master key on this box. That is not a statement about any tenant — see");
        println!("deploy/KEYS.md. Mint one with `pkdump keys init`, or put the backed-up one");
        println!("back with `pkdump keys restore`.");
        return Ok(());
    }

    println!("  mode           {:o}", master::mode_of(&path)?);
    match derive::master_fingerprint() {
        Ok(fingerprint) => println!("  fingerprint    {fingerprint}"),
        Err(e) => println!("  status         UNUSABLE: {e}"),
    }

    // Key state is a different question about a different object, so it is
    // read separately and a failure here says nothing about the key above.
    match state::open().and_then(|c| state::list(&c)) {
        Ok(rows) => {
            let active = rows.iter().filter(|r| r.state == KeyState::Active).count();
            let tombstoned = rows.len() - active;
            println!("Key state        {active} active, {tombstoned} tombstoned");
        }
        Err(e) => println!("Key state        UNREADABLE: {e}"),
    }
    Ok(())
}

/// THE BACKUP PATH. Nothing here touches the key-state registry.
fn run_backup(a: BackupArgs) -> anyhow::Result<()> {
    if let Some(dest) = a.out {
        let written = backup::export_to_file(&dest)?;
        println!("Master key copied to {} (mode 600)", written.display());
        println!("Move it into your password manager and delete the copy.");
        return Ok(());
    }
    if !a.yes {
        anyhow::bail!(
            "refusing to print the master key without --yes.\n\
             This writes secret key material to your terminal, and from there to your shell's \
             scrollback. Either confirm with --yes, or write a mode-600 copy with -o <file>."
        );
    }
    print!("{}", backup::export()?.as_str());
    Ok(())
}

fn run_restore(a: RestoreArgs) -> anyhow::Result<()> {
    let material = match &a.input {
        Some(p) => std::fs::read_to_string(p)?,
        None => std::io::read_to_string(std::io::stdin())?,
    };
    let (path, fingerprint) = backup::restore(&material)?;
    println!("Master key restored to {}", path.display());
    println!("  mode        {:o}", master::mode_of(&path)?);
    println!("  fingerprint {fingerprint}");
    println!();
    println!("Check that fingerprint against the one you recorded when the key was minted.");
    println!("A restored key derives exactly the keys it did before — and does NOT lift any");
    println!("tombstone: revocation lives in the registry, not in the key.");
    Ok(())
}

fn register(a: DatabaseIdArgs) -> anyhow::Result<()> {
    let conn = state::open()?;
    let row = state::register(&conn, &a.database_id)?;
    println!("{} key state: {}", row.database_id, row.state.as_str());
    Ok(())
}

fn list() -> anyhow::Result<()> {
    let conn = state::open()?;
    let rows = state::list(&conn)?;
    if rows.is_empty() {
        println!("No key state recorded on this box.");
        println!();
        println!("Nothing is derivable until it is registered — absence is not permission. If a");
        println!("registry was just restored and you expected rows here, STOP: a registry");
        println!("missing its rows is also missing its tombstones (deploy/KEYS.md).");
        return Ok(());
    }
    println!(
        "{:<28} {:<11} {:<26} REASON",
        "DATABASE ID", "STATE", "TOMBSTONED"
    );
    for r in rows {
        println!(
            "{:<28} {:<11} {:<26} {}",
            r.database_id,
            r.state.as_str(),
            r.tombstoned_at.as_deref().unwrap_or("-"),
            r.reason.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn derive_one(a: DatabaseIdArgs) -> anyhow::Result<()> {
    let conn = state::open()?;
    let key = derive::tenant_key(&conn, &a.database_id)?;
    println!(
        "{} key fingerprint {}",
        key.database_id(),
        key.fingerprint()
    );
    Ok(())
}

/// THE DESTRUCTION PATH. Nothing here touches the master key file.
fn run_tombstone(a: TombstoneArgs) -> anyhow::Result<()> {
    if !a.yes {
        anyhow::bail!(
            "refusing to revoke the key for {} without --yes.\n\
             A tombstone is permanent: it is never lifted, and no restore of any backup \
             reverses it.",
            a.database_id
        );
    }
    let conn = state::open()?;
    let row = destroy::tombstone(&conn, &a.database_id, a.reason.as_deref())?;
    println!(
        "{} key REVOKED at {}",
        row.database_id,
        row.tombstoned_at.as_deref().unwrap_or("(unrecorded)")
    );
    if let Some(reason) = &row.reason {
        println!("  reason {reason}");
    }
    println!();
    println!("Derivation for this database_id now refuses, permanently — whether or not the");
    println!("master key is present. The master key itself was not touched: every other");
    println!("tenant is unaffected.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        command: KeysCommand,
    }

    fn parse(argv: &[&str]) -> KeysCommand {
        let mut full = vec!["pkdump-keys-test"];
        full.extend_from_slice(argv);
        Harness::parse_from(full).command
    }

    /// Printing a secret is a decision. Without `--yes` and without `--out`,
    /// nothing is emitted.
    #[test]
    fn backup_refuses_to_print_without_yes() {
        let KeysCommand::Backup(a) = parse(&["backup"]) else {
            panic!("expected backup")
        };
        assert!(!a.yes);
        let err = run_backup(a).unwrap_err().to_string();
        assert!(err.contains("--yes"), "{err}");
        assert!(err.contains("secret key material"), "{err}");
    }

    /// A revocation is never implicit either — and the refusal says the thing
    /// an operator most needs to know before typing `--yes`.
    #[test]
    fn tombstone_refuses_without_yes() {
        let KeysCommand::Tombstone(a) = parse(&["tombstone", "01J0000000000000000000000A"]) else {
            panic!("expected tombstone")
        };
        assert!(!a.yes);
        let err = run_tombstone(a).unwrap_err().to_string();
        assert!(err.contains("--yes"), "{err}");
        assert!(err.contains("permanent"), "{err}");
    }

    /// The two paths do not share a confirmation flag by accident: `backup`
    /// has an escape hatch that writes a file instead of printing, and
    /// `tombstone` has none, because there is no safer way to revoke.
    #[test]
    fn the_two_paths_take_different_arguments() {
        let KeysCommand::Backup(b) = parse(&["backup", "-o", "/tmp/k"]) else {
            panic!("expected backup")
        };
        assert_eq!(b.out.unwrap(), std::path::PathBuf::from("/tmp/k"));

        let KeysCommand::Tombstone(t) = parse(&[
            "tombstone",
            "01J0000000000000000000000A",
            "--yes",
            "--reason",
            "closed",
        ]) else {
            panic!("expected tombstone")
        };
        assert!(t.yes);
        assert_eq!(t.reason.as_deref(), Some("closed"));
    }

    /// `derive` takes a database_id, and a handle is not one — the same rule
    /// the registry itself holds every writer to.
    #[test]
    fn derive_refuses_a_handle() {
        let KeysCommand::Derive(a) = parse(&["derive", "alice"]) else {
            panic!("expected derive")
        };
        assert_eq!(a.database_id, "alice");
    }
}
