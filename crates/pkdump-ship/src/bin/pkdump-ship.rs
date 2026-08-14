//! `pkdump-ship` — the ownership outbox into the tenant zone (`pd-dxn3`).
//!
//! An **offline** job, like `pkdump-lake-derive` and the transform tier
//! beside it. Nothing that serves a request runs this, and nothing that
//! serves a request holds the credentials it needs: the tenant zone's profile
//! reaches `tenant/` and the master key derives everybody's key, and an
//! always-on web server holding either would be the coupling the whole zone
//! split exists to prevent.
//!
//! ```text
//! pkdump-ship run                      # every registered tenant
//! pkdump-ship run --tenant alice       # one, by handle or by database id
//! pkdump-ship status                   # what is pending, and any gaps
//! pkdump-ship decrypt --key tenant/…   # read one part back
//! ```
//!
//! Exit status is the run's, and there are four of them — see
//! [`pkdump_ship::run`]. `deploy/ship.sh` is what turns them into a journal
//! line and, for two of them, an alarm.

use std::io::Write;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use pkdump_ship::run::{DEFAULT_MAX_ROWS, Outcome};

#[derive(Parser)]
#[command(
    name = "pkdump-ship",
    about = "Ship the ownership outbox into the tenant zone",
    long_about = None,
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ship every registered tenant's outbox.
    Run(RunArgs),
    /// Report what is unshipped, and every gap ever recorded.
    Status(ScopeArgs),
    /// Read one shipped part back, decrypting it under its tenant's key.
    Decrypt(DecryptArgs),
}

#[derive(Args)]
struct ScopeArgs {
    /// The PokeDumpster data directory. Defaults to `$PKDUMP_HOME`.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// One tenant, by handle or by database id. Defaults to all of them.
    #[arg(long, value_name = "TENANT")]
    tenant: Option<String>,
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    scope: ScopeArgs,

    /// Outbox rows per part.
    ///
    /// This is part of how an object is ADDRESSED, not a tuning knob: a part
    /// is named for the sequence range it carries, so changing this changes
    /// which ranges exist. Re-shipping an already-shipped stretch under a
    /// different value writes new objects holding the same events rather than
    /// landing on the old ones. Harmless — a reader keys on `seq` — but not
    /// free, so it is spelled out rather than fiddled with.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_ROWS)]
    max_rows: usize,
}

#[derive(Args)]
struct DecryptArgs {
    /// The object key, exactly as it is in the zone. The `database_id=`
    /// component is what says whose key to derive — there is no flag for it,
    /// because a part that decrypted under an id other than the one in its
    /// own path would be a part in the wrong tenant's partition.
    #[arg(long, value_name = "KEY")]
    key: String,

    /// Write the decrypted Parquet here instead of summarising it.
    #[arg(short, long, value_name = "FILE")]
    out: Option<PathBuf>,

    /// Print the events as JSON lines.
    #[arg(long)]
    json: bool,

    /// The PokeDumpster data directory. Defaults to `$PKDUMP_HOME`.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pkdump-ship: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Run(a) => {
            adopt_data_dir(a.scope.data_dir.as_deref());
            do_run(&a)
        }
        Command::Status(a) => {
            adopt_data_dir(a.data_dir.as_deref());
            do_status(&a)
        }
        Command::Decrypt(a) => {
            adopt_data_dir(a.data_dir.as_deref());
            do_decrypt(&a)
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

fn tenants_in_scope(scope: &ScopeArgs) -> anyhow::Result<Vec<pkdump_db::tenants::Tenant>> {
    let all = pkdump_db::tenants::list()?;
    let Some(wanted) = &scope.tenant else {
        return Ok(all);
    };
    let found: Vec<_> = all
        .into_iter()
        .filter(|t| &t.user.handle == wanted || &t.user.database_id == wanted)
        .collect();
    if found.is_empty() {
        anyhow::bail!(
            "no registered tenant is called {wanted:?}. `pkdump tenant list` says who there is; \
             a tenant that is not in the registry has no database_id, and the tenant zone is \
             partitioned by database_id."
        );
    }
    Ok(found)
}

fn do_run(a: &RunArgs) -> anyhow::Result<i32> {
    anyhow::ensure!(a.max_rows > 0, "--max-rows must be at least 1");

    let tenants = tenants_in_scope(&a.scope)?;
    if tenants.is_empty() {
        // Not a failure: a box with no tenants has nothing to ship and
        // nothing is wrong with it. It is said out loud because a registry
        // that came back empty from a restore looks exactly like this.
        println!("No registered tenants. Nothing to ship.");
        return Ok(0);
    }

    let (zone, config) = pkdump_lake::open_tenant_zone()?;
    let registry = pkdump_keys::state::open()?;

    println!(
        "==> shipping {} tenant(s) to {} as {}",
        tenants.len(),
        zone.describe(),
        config.profile
    );
    let _ = std::io::stdout().flush();

    let report = pkdump_ship::ship_all(zone.as_ref(), &config, &registry, &tenants, a.max_rows);
    let outcome = report.outcome();

    println!();
    match outcome {
        Outcome::Clean => println!(
            "OK — {} event(s) in {} part(s) across {} tenant(s)",
            report.events(),
            report.parts(),
            report.tenants.len()
        ),
        Outcome::Partial => {
            eprintln!(
                "PARTIAL — {} of {} tenant(s) skipped; the rest shipped {} event(s)",
                report.skipped().len(),
                report.tenants.len(),
                report.events()
            );
        }
        Outcome::Gap => {
            eprintln!(
                "SEQUENCE GAP — {} range(s) of the outbox were LOST and are recorded in each \
                 collection's ownership_outbox_gap. The tenant zone is INCOMPLETE for:",
                report.gaps().len()
            );
            for (database_id, gap) in report.gaps() {
                eprintln!("  {database_id}: {gap} ({} event(s))", gap.events());
            }
            eprintln!(
                "The rows that were still there have been shipped — the loss already happened, \
                 and withholding what remains would only add to it."
            );
        }
        Outcome::Failed => {
            eprintln!(
                "FAILED — nothing shipped. Every one of the {} registered tenant(s) was skipped, \
                 which is a fault of the run rather than of any tenant:",
                report.tenants.len()
            );
            for (database_id, why) in report.skipped() {
                eprintln!("  {database_id}: {why}");
            }
        }
    }
    Ok(outcome.code())
}

fn do_status(a: &ScopeArgs) -> anyhow::Result<i32> {
    let tenants = tenants_in_scope(a)?;
    if tenants.is_empty() {
        println!("No registered tenants.");
        return Ok(0);
    }
    println!(
        "{:<28} {:<14} {:>10} {:>8}  GAPS",
        "DATABASE ID", "HANDLE", "SHIPPED", "PENDING"
    );
    let mut any_gap = false;
    for tenant in &tenants {
        if !tenant.path.exists() {
            println!(
                "{:<28} {:<14} {:>10} {:>8}  (no database on this box)",
                tenant.user.database_id, tenant.user.handle, "-", "-"
            );
            continue;
        }
        let conn = pkdump_db::open_user(&tenant.path)?;
        let gaps = pkdump_ship::cursor::gaps(&conn)?;
        any_gap |= !gaps.is_empty();
        println!(
            "{:<28} {:<14} {:>10} {:>8}  {}",
            tenant.user.database_id,
            tenant.user.handle,
            pkdump_ship::cursor::shipped_thru(&conn)?,
            pkdump_ship::cursor::pending(&conn)?,
            if gaps.is_empty() {
                "-".to_string()
            } else {
                gaps.iter()
                    .map(|g| format!("{}..{}", g.from_seq, g.to_seq))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        );
    }
    if any_gap {
        println!();
        println!(
            "A gap is a stretch of outbox events that was LOST before it reached the zone. It \
             stays in the ledger until an operator clears it — the shipper never does, because \
             the shipper is not what reconciles it."
        );
    }
    Ok(0)
}

fn do_decrypt(a: &DecryptArgs) -> anyhow::Result<i32> {
    let database_id = database_id_of(&a.key)?;
    let registry = pkdump_keys::state::open()?;
    let key = pkdump_keys::tenant_key(&registry, &database_id)?;

    let (source, config) = pkdump_lake::open_tenant_zone_reader()?;
    let object_key = config.rooted(a.key.clone());
    let sealed = source.get(&object_key)?;
    let parquet = pkdump_ship::cipher::open(&key, &object_key, &sealed)?;

    if let Some(path) = &a.out {
        std::fs::write(path, &parquet)?;
        println!(
            "{} bytes of Parquet written to {}",
            parquet.len(),
            path.display()
        );
        return Ok(0);
    }

    let events = pkdump_ship::encode::decode(parquet)?;
    if a.json {
        let mut out = std::io::stdout().lock();
        for e in &events {
            writeln!(
                out,
                "{}",
                serde_json::json!({
                    "seq": e.seq,
                    "occurred_at": e.occurred_at,
                    "source_table": e.source_table,
                    "op": e.op,
                    "row_id": e.row_id,
                    "payload": serde_json::from_str::<serde_json::Value>(&e.payload)
                        .unwrap_or(serde_json::Value::String(e.payload.clone())),
                    // Every field the part carries, including provenance:
                    // "did last night's redrive actually reach the zone" is
                    // a question this view exists to answer.
                    "source": e.source,
                })
            )?;
        }
        return Ok(0);
    }

    match (events.first(), events.last()) {
        (Some(first), Some(last)) => println!(
            "{} event(s), seq {}..{}, {} .. {}",
            events.len(),
            first.seq,
            last.seq,
            first.occurred_at,
            last.occurred_at
        ),
        _ => println!("0 events"),
    }
    Ok(0)
}

/// The `database_id=` component of a tenant-zone key.
fn database_id_of(key: &str) -> anyhow::Result<String> {
    key.split('/')
        .find_map(|c| c.strip_prefix("database_id="))
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{key:?} has no database_id= component, so there is no way to know whose key \
                 would decrypt it. Every object in the tenant zone is partitioned by \
                 database_id first — see pkdump_lake::tenant."
            )
        })
}
