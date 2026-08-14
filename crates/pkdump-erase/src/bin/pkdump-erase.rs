//! `pkdump-erase` — deleting an account from the tenant zone (`pd-qbrf`).
//!
//! An **offline** job, like `pkdump-ship` and `pkdump-lake-derive` beside it.
//! Nothing that serves a request runs this and nothing that serves a request
//! holds what it needs: the tenant profile reaches `tenant/` and the master
//! key derives everybody's key, and an always-on web server holding either
//! would be the coupling the whole zone split exists to prevent.
//!
//! ```text
//! pkdump-erase delete --tenant alice --yes      # tombstone, drop, prove
//! pkdump-erase verify --tenant alice            # prove only; changes nothing
//! pkdump-erase list --tenant alice              # what is in the zone for them
//! ```
//!
//! ## Exit status
//!
//! ```text
//! 0  deleted, and PROVEN unreadable on every path
//! 1  the run could not proceed — bad argument, no credentials, no such tenant
//! 4  it ran, and the deletion is NOT PROVEN
//! ```
//!
//! **4 is its own status and it is not 1.** A deletion that ran and cannot be
//! proven is a different event from one that never started: the data may well
//! be gone, and what is missing is the evidence. It pages either way, and the
//! two need different first questions asked of them — see
//! `deploy/DELETION.md`.
//!
//! ## `--yes`, and why `verify` does not need one
//!
//! `delete` is irreversible in both halves: a tombstone is never lifted and
//! the objects do not come back. `verify` reads. So one takes a confirmation
//! and the other does not, the same asymmetry `pkdump keys` draws between its
//! two paths.

use std::io::Write;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use pkdump_erase::verify::Check;

/// The run finished and the deletion is not proven. See the module docs.
const EXIT_NOT_PROVEN: i32 = 4;

#[derive(Parser)]
#[command(
    name = "pkdump-erase",
    about = "Delete an account from the tenant zone, and prove it",
    long_about = None,
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Tombstone the key, drop the partition, and prove it unreadable.
    Delete(DeleteArgs),
    /// Attempt every read path and report which are closed. Changes nothing.
    Verify(VerifyArgs),
    /// What this tenant still has in the zone.
    List(ScopeArgs),
}

#[derive(Args)]
struct ScopeArgs {
    /// The tenant, by handle or by `database_id`.
    ///
    /// A handle is resolved through the registry; a `database_id` is taken as
    /// given, because deletion must not depend on provisioning having been
    /// tidy — an id whose registry row is already gone still has a partition.
    #[arg(long, value_name = "TENANT")]
    tenant: String,

    /// The PokeDumpster data directory. Defaults to `$PKDUMP_HOME`.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
}

#[derive(Args)]
struct StrayArgs {
    /// A copy of one of this tenant's objects, taken BEFORE the deletion, to
    /// be proven unopenable after it.
    ///
    /// This is the check the design is really about: the partition drop has
    /// to find every copy, and crypto-shredding does not. A copy that
    /// survived somewhere — a compacted file, an older snapshot, a bucket
    /// version — should be ciphertext nobody holds a key for, and this is
    /// where that is checked against real bytes rather than asserted.
    #[arg(long, value_name = "FILE", requires = "stray_key")]
    stray: Option<PathBuf>,

    /// The object key the `--stray` copy was taken from.
    ///
    /// Required with it, and deliberately not guessed: the object key is the
    /// sealed envelope's associated data, so a copy that failed to open under
    /// a key nobody checked would be a proof of nothing.
    #[arg(long, value_name = "KEY")]
    stray_key: Option<String>,
}

#[derive(Args)]
struct DeleteArgs {
    #[command(flatten)]
    scope: ScopeArgs,

    #[command(flatten)]
    stray: StrayArgs,

    /// Why. Recorded with the tombstone, for the audit trail.
    #[arg(long)]
    reason: Option<String>,

    /// Confirm. Without it nothing is deleted.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct VerifyArgs {
    #[command(flatten)]
    scope: ScopeArgs,

    #[command(flatten)]
    stray: StrayArgs,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pkdump-erase: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Delete(a) => {
            adopt_data_dir(a.scope.data_dir.as_deref());
            do_delete(&a)
        }
        Command::Verify(a) => {
            adopt_data_dir(a.scope.data_dir.as_deref());
            do_verify(&a)
        }
        Command::List(a) => {
            adopt_data_dir(a.data_dir.as_deref());
            do_list(&a)
        }
    }
}

/// `--data-dir` is `$PKDUMP_HOME` said on the command line, which is what
/// every containerised job here does (`--data-dir /data`).
fn adopt_data_dir(dir: Option<&std::path::Path>) {
    if let Some(dir) = dir {
        // SAFETY: argument parsing, before any thread exists.
        unsafe { std::env::set_var("PKDUMP_HOME", dir) };
    }
}

/// A handle or a `database_id`, resolved to a `database_id`.
///
/// The registry answers first, then the id is taken at face value — but only
/// if it could be one. A name that is neither is [`EraseError::NotATenant`]
/// rather than a successful deletion of nothing, because a deletion asked for
/// by a typo is far more likely than one asked for by a name that never
/// existed.
fn resolve(name: &str) -> anyhow::Result<String> {
    let registry = pkdump_db::registry::open()?;
    if let Some(user) = pkdump_db::registry::lookup(&registry, name)? {
        return Ok(user.database_id);
    }
    if pkdump_db::validate_database_id(name).is_ok() {
        return Ok(name.to_string());
    }
    Err(pkdump_erase::EraseError::NotATenant {
        name: name.to_string(),
    }
    .into())
}

fn stray_of(a: &StrayArgs) -> anyhow::Result<Option<pkdump_erase::StrayCopy>> {
    let (Some(path), Some(key)) = (&a.stray, &a.stray_key) else {
        return Ok(None);
    };
    Ok(Some(pkdump_erase::StrayCopy::read(key, path)?))
}

fn do_delete(a: &DeleteArgs) -> anyhow::Result<i32> {
    let database_id = resolve(&a.scope.tenant)?;
    if !a.yes {
        anyhow::bail!(
            "refusing to delete {database_id} without --yes.\n\
             This is irreversible in both halves. The tombstone is never lifted — no restore \
             of any backup reverses it — and the objects under \
             tenant/database_id={database_id}/ do not come back."
        );
    }

    let stray = stray_of(&a.stray)?;
    let (zone, config) = pkdump_lake::open_tenant_zone_purge()?;
    let registry = pkdump_keys::state::open()?;

    println!("==> deleting {database_id} from {}", zone.describe());
    let _ = std::io::stdout().flush();

    let done = pkdump_erase::delete(
        zone.as_ref(),
        &config,
        &registry,
        &database_id,
        a.reason.as_deref(),
        stray.as_ref(),
    )?;

    println!(
        "    1. key REVOKED at {}{}",
        done.tombstoned_at,
        if done.tombstone_was_already_there {
            " (already tombstoned; the first record is kept)"
        } else {
            ""
        }
    );
    println!(
        "    2. partition {} — {} object(s) removed",
        done.dropped.prefix,
        done.dropped.count()
    );
    for key in &done.dropped.keys {
        println!("         {key}");
    }
    println!("    3. verification:");
    report(&done.verdict);

    if !done.verdict.proven() {
        eprintln!();
        eprintln!(
            "NOT PROVEN — the deletion ran and the evidence is incomplete. That is not the same \
             as a run that never started, and the first question is which of the checks above \
             failed. See deploy/DELETION.md."
        );
        return Ok(EXIT_NOT_PROVEN);
    }
    println!();
    println!("DELETED — {database_id} is unreachable by every path checked above.");
    println!();
    println!(
        "The online half is a different command: `pkdump tenant detach {}` releases the handle \
         and `pkdump tenant purge {database_id} --yes` removes the collection database and its \
         replica. See deploy/TENANTS.md.",
        a.scope.tenant
    );
    Ok(0)
}

fn do_verify(a: &VerifyArgs) -> anyhow::Result<i32> {
    let database_id = resolve(&a.scope.tenant)?;
    let stray = stray_of(&a.stray)?;
    let (zone, config) = pkdump_lake::open_tenant_zone_purge()?;
    let registry = pkdump_keys::state::open()?;

    println!("==> verifying {database_id} against {}", zone.describe());
    let verdict = pkdump_erase::verify(
        zone.as_ref(),
        &config,
        &registry,
        &database_id,
        stray.as_ref(),
    )?;
    report(&verdict);

    if verdict.proven() {
        println!();
        println!("PROVEN — no path reaches {database_id}'s holdings or valuations.");
        return Ok(0);
    }
    eprintln!();
    eprintln!(
        "NOT PROVEN — {} of {} checks did not establish that the data is unreachable.",
        verdict.failures().len(),
        verdict.proofs.len()
    );
    Ok(EXIT_NOT_PROVEN)
}

fn do_list(a: &ScopeArgs) -> anyhow::Result<i32> {
    let database_id = resolve(&a.tenant)?;
    let (zone, config) = pkdump_lake::open_tenant_zone_purge()?;
    let sweep = pkdump_erase::Sweep::new(zone.as_ref(), &config, &database_id)?;
    let keys = sweep.list()?;
    if keys.is_empty() {
        println!("{} holds no objects.", sweep.prefix());
        println!();
        println!(
            "An empty prefix is not by itself a deletion — `pkdump-erase verify --tenant {}` \
             is what asks whether the key still derives.",
            a.tenant
        );
        return Ok(0);
    }
    println!("{} object(s) under {}:", keys.len(), sweep.prefix());
    for key in &keys {
        println!("  {key}");
    }
    Ok(0)
}

/// One line per check. The detail is the whole point — a bare PASS/FAIL would
/// be exactly the "asserted" this item exists to replace.
fn report(verdict: &pkdump_erase::Verdict) {
    for proof in &verdict.proofs {
        let mark = match (&proof.check, proof.held) {
            (Check::Machinery, true) => "ok   ",
            (_, true) => "CLOSED",
            (_, false) => "OPEN  ",
        };
        println!(
            "      {mark} {:<20} {}",
            proof.check.to_string(),
            proof.detail
        );
    }
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        let mut full = vec!["pkdump-erase"];
        full.extend_from_slice(argv);
        Cli::try_parse_from(full)
    }

    /// Irreversible things are never implicit, and the refusal says what is
    /// irreversible about this one before an operator types `--yes`.
    #[test]
    fn delete_refuses_without_yes() {
        let Ok(Cli {
            command: Command::Delete(a),
        }) = parse(&["delete", "--tenant", "01J0000000000000000000000A"])
        else {
            panic!("expected delete")
        };
        assert!(!a.yes);
        // Resolution happens first and needs a registry, so the flag itself is
        // what is asserted here; the refusal text is exercised in
        // tests/deletion.rs against a real data directory.
        assert!(a.reason.is_none());
    }

    /// A stray copy without its object key cannot prove anything, so clap
    /// refuses the pair rather than the command discovering it later.
    #[test]
    fn a_stray_copy_without_its_object_key_is_refused() {
        let Err(err) = parse(&["verify", "--tenant", "alice", "--stray", "/tmp/copy.enc"]) else {
            panic!("a stray copy with no object key must not parse")
        };
        assert!(
            err.to_string().contains("stray-key"),
            "the refusal must name the missing half: {err}"
        );
    }

    /// `verify` takes no `--yes`: it reads, and a read that needed confirming
    /// would be a read nobody ran.
    #[test]
    fn verify_takes_no_confirmation() {
        assert!(parse(&["verify", "--tenant", "alice", "--yes"]).is_err());
        assert!(parse(&["verify", "--tenant", "alice"]).is_ok());
    }

    /// 4 is not 1. See the module docs.
    #[test]
    fn not_proven_has_its_own_exit_status() {
        assert_eq!(EXIT_NOT_PROVEN, 4);
        assert_ne!(EXIT_NOT_PROVEN, 1);
    }
}
