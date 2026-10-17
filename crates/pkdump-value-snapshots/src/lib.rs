//! `pkdump-value-snapshots` — the per-tenant collection value snapshot.
//!
//! The Rust form of `pkdump_lake.value_snapshots` (sp-4k7mxi.5): the same
//! contract, the same SQL, the same exit statuses, reading `catalog.prices` and
//! `catalog.sealed_prices` through `dumpster-kit-iceberg`'s catalog client at a
//! commit pinned once for the whole run. The Python job stays in place until a
//! run on prod's own lake has been compared against it and the timer is
//! switched; `tests/lake/value_snapshots.sh` §5d holds the two row-identical.
//!
//! Tenant data never enters the lake: prices come OUT of Iceberg and the
//! snapshot is written to the tenant's own SQLite file. Nothing here writes to
//! the lake at all.

pub mod lake;
pub mod snapshot;

use std::fmt;
use std::path::{Path, PathBuf};

use dumpster_kit_iceberg::LakeCatalogConfig;

use crate::snapshot::{Tenant, tenants};

pub const DEFAULT_TABLE: &str = "catalog.prices";
pub const DEFAULT_SEALED_TABLE: &str = "catalog.sealed_prices";
pub const HOME_ENV: &str = "PKDUMP_HOME";
pub const DEFAULT_HOME: &str = "~/.pkdump";

/// All tenants snapshotted.
pub const EXIT_OK: i32 = 0;
/// The run could not start, or snapshotted nobody at all.
pub const EXIT_FAILED: i32 = 1;
/// The run completed and at least one tenant was skipped.
pub const EXIT_PARTIAL: i32 = 2;

/// Something that makes a run (or one tenant) unable to proceed. Not the same
/// thing as one tenant failing: the caller decides which it is.
#[derive(Debug)]
pub struct Refusal(pub String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal(e.to_string())
    }
}

pub struct Outcome {
    pub tenant: Tenant,
    pub error: Option<String>,
}

/// The pkdump data directory: `--data-dir`, `$PKDUMP_HOME`, or the default.
pub fn data_dir(explicit: Option<&str>) -> PathBuf {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| std::env::var(HOME_ENV).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| DEFAULT_HOME.to_string());
    match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(raw),
    }
}

pub struct Args {
    pub date: String,
    pub root: PathBuf,
    pub table: String,
    pub sealed_table: String,
    pub reference: Option<String>,
    pub only: Option<Vec<String>>,
    pub dry_run: bool,
}

/// Snapshot every registered tenant for `args.date`. One tenant's failure is
/// not the run's.
pub async fn run(args: &Args) -> Result<Vec<Outcome>, Refusal> {
    let shared = args.root.join("shared.sqlite");
    if !shared.exists() {
        return Err(Refusal(format!(
            "no catalog at {} — run `pkdump setup` first",
            shared.display()
        )));
    }
    let registered = tenants(&args.root, args.only.as_deref())?;
    if registered.is_empty() {
        return Err(Refusal(format!(
            "the registry at {} holds no active user — `pkdump tenant create <handle>` provisions one",
            args.root.join(snapshot::REGISTRY_FILE).display()
        )));
    }

    let mut config = LakeCatalogConfig::from_env().map_err(|e| Refusal(e.to_string()))?;
    let lake_ref = lake::pin(&config, args.reference.as_deref()).await?;
    config.reference = lake_ref.clone();
    println!(
        "==> lake {} + {} at {lake_ref}",
        args.table, args.sealed_table
    );
    println!(
        "==> data directory {}, {} active tenant(s)",
        args.root.display(),
        registered.len()
    );
    let cat = dumpster_kit_iceberg::open(&config)
        .await
        .map_err(|e| Refusal(format!("opening the lake: {e}")))?;

    let prices = lake::market_prices(&cat, &args.table, &args.date).await?;
    if prices.is_empty() {
        return Err(Refusal(format!(
            "{} holds no {} price at or before {} at {lake_ref} — there is nothing to value a \
             collection with",
            args.table,
            lake::PRICE_TYPE,
            args.date
        )));
    }
    // Not refused when empty: a lake can honestly hold no sealed price for a
    // date. The table being absent altogether is still a refusal.
    let sealed = lake::sealed_market_prices(&cat, &args.sealed_table, &args.date).await?;

    let mut outcomes = Vec::new();
    for tenant in registered {
        println!("==> {} ({})", tenant.handle, tenant.database_id);
        let error = snapshot::snapshot(
            &tenant,
            &shared,
            &prices,
            &sealed,
            &args.date,
            &args.table,
            &lake_ref,
            args.dry_run,
        )
        .err()
        .map(|e| {
            // Logged and skipped, deliberately: the next tenant does not
            // depend on this one.
            println!("    SKIPPED: {e}");
            e.to_string()
        });
        outcomes.push(Outcome { tenant, error });
    }
    Ok(outcomes)
}

/// Print the per-tenant tally and return the process exit status.
pub fn report(outcomes: &[Outcome], date: &str) -> i32 {
    let done = outcomes.iter().filter(|o| o.error.is_none()).count();
    let skipped: Vec<&Outcome> = outcomes.iter().filter(|o| o.error.is_some()).collect();
    println!();
    println!(
        "==> {date}: {done} tenant(s) snapshotted, {} skipped",
        skipped.len()
    );
    for o in &skipped {
        println!(
            "    skipped {}: {}",
            o.tenant.handle,
            o.error.as_deref().unwrap_or("")
        );
    }
    if done == 0 && !skipped.is_empty() {
        println!("==> exiting {EXIT_FAILED}: nobody was snapshotted at all");
        return EXIT_FAILED;
    }
    if !skipped.is_empty() {
        println!("==> exiting {EXIT_PARTIAL} (partial): a run that half-completes says so");
        return EXIT_PARTIAL;
    }
    EXIT_OK
}

/// Where the databases live; kept as a function of a path so tests can use it.
pub fn shared_path(root: &Path) -> PathBuf {
    root.join("shared.sqlite")
}
