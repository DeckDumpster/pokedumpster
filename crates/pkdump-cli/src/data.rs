//! `pkdump data refresh` — nightly incremental catalog update.
//!
//! Unlike `pkdump setup`, this skips the `pokemon-tcg-data` bulk import. It
//! re-fetches the tail of newest sets from the pokemontcg.io API, re-imports
//! TCGCSV groups, sealed products, and prices, then re-applies the dirty-data
//! overrides. Each run appends a fresh price snapshot (a new `observed_at`)
//! to the `prices` table — the source for a future price-history chart.
//!
//! The pipeline it runs is `pkdump-derive`, shared with the offline
//! `pkdump-lake-derive shared` job. This file is what is left once the
//! derivation moved out: argument parsing, the database to open, and the
//! clock. **Nothing here reads `raw/`** — that is the boundary the lakehouse
//! epic turns on, and it is why the offline job is a different binary in a
//! different crate rather than a `--from-raw` flag on this one.

use std::path::PathBuf;

use pkdump_ingest::{coverage, overrides, pokemon_tcg_data, symbols};

/// The `pkdump data` subcommand group.
#[derive(clap::Args)]
pub struct DataArgs {
    #[command(subcommand)]
    command: DataCommand,
}

#[derive(clap::Subcommand)]
enum DataCommand {
    /// Incrementally refresh the shared catalog: newest sets, prices, and
    /// dirty-data overrides.
    Refresh(RefreshCmdArgs),
    /// Trim and resize set symbol glyphs in isolation — useful for
    /// migrating an existing catalog without paying for a full TCGCSV
    /// import.
    NormalizeSymbols(RefreshArgs),
    /// Reconcile variant + sub_type maps from JSON and re-run variant
    /// expansion against existing TCGCSV products. No network — useful
    /// after editing data/variants.json or data/tcgcsv_sub_type_variants
    /// .json, or after a migration that adds a new bridge.
    Expand(RefreshArgs),
    /// Re-apply `data/overrides/upstream_card_corrections.json` to cards
    /// already in the catalog. `upsert_card` only corrects rows as they
    /// are ingested and `refresh` skips sets it already has, so a
    /// correction added (or edited) after the fact needs this pass to
    /// reach the existing row. No network; idempotent.
    ApplyCorrections(ApplyCorrectionsArgs),
    /// One-time: reconstruct collection value history from `shared.prices`
    /// × each copy's acquisition + status history, into the user DB's
    /// `collection_value_snapshot` table. `pkdump-lake-value-snapshots`
    /// records today going forward, for every tenant; this seeds the past
    /// for the one `$PKDUMP_USER` names. Idempotent.
    BackfillValueHistory(RefreshArgs),
}

/// Arguments shared by the `pkdump data` subcommands that only need a
/// database.
#[derive(clap::Args)]
pub struct RefreshArgs {
    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

/// Arguments for `pkdump data refresh`.
#[derive(clap::Args)]
pub struct RefreshCmdArgs {
    #[command(flatten)]
    common: RefreshArgs,

    /// Land every upstream response in the raw landing zone before parsing
    /// it, under `raw/source=.../ingest_date=.../run=<ULID>/`.
    ///
    /// Requires ~/.config/pkdump/lake.env to name the bucket; the command
    /// refuses to start without it rather than landing nothing quietly.
    /// Card art and set symbols are never landed. Also settable as
    /// PKDUMP_LAND_RAW=1, which is how the containerised nightly refresh
    /// turns it on.
    #[arg(long)]
    land_raw: bool,
}

/// Arguments for `pkdump data apply-corrections`.
#[derive(clap::Args)]
pub struct ApplyCorrectionsArgs {
    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Report the rows that would change without writing them.
    #[arg(long)]
    dry_run: bool,
}

/// Execute `pkdump data`.
pub fn run(args: DataArgs) -> anyhow::Result<()> {
    match args.command {
        DataCommand::Refresh(args) => refresh(args),
        DataCommand::NormalizeSymbols(args) => normalize_symbols(args),
        DataCommand::Expand(args) => expand_only(args),
        DataCommand::ApplyCorrections(args) => apply_corrections(args),
        DataCommand::BackfillValueHistory(args) => backfill_value_history(args),
    }
}

/// Execute `pkdump data apply-corrections` — heal already-ingested rows
/// against the upstream-correction registry.
fn apply_corrections(args: ApplyCorrectionsArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let conn = pkdump_db::open_shared(&db_path)?;

    let rows = if args.dry_run {
        pokemon_tcg_data::pending_corrections(&conn)?
    } else {
        pokemon_tcg_data::apply_corrections_to_db(&conn)?
    };
    let verb = if args.dry_run {
        "would change"
    } else {
        "healed"
    };
    for r in &rows {
        println!(
            "  {} number {} -> {} (sortable {} -> {})",
            r.card_id,
            r.current_number,
            r.corrected_number,
            r.current_number_sortable,
            r.corrected_number_sortable
        );
    }
    println!("{} row(s) {verb}.", rows.len());
    Ok(())
}

/// Execute `pkdump data backfill-value-history` — a one-time reconstruction
/// of the collection's value over time. Unlike `refresh`/`setup` (which open
/// the *shared* catalog read-write), value snapshots live in the *user* DB,
/// so this opens a user connection (`connect_user`) with the shared catalog
/// attached read-only — collection value needs the user's copies.
fn backfill_value_history(args: RefreshArgs) -> anyhow::Result<()> {
    let shared_db = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    let user_db = crate::collection::user_db()?;
    println!(
        "Backfilling value history into {} (catalog {})",
        user_db.display(),
        shared_db.display()
    );
    let mut conn = pkdump_db::connect_user(&user_db, &shared_db)?;
    let rows = pkdump_db::value_history::backfill(&mut conn)?;
    println!("Value-history backfill complete: {rows} snapshot rows.");
    Ok(())
}

/// Execute `pkdump data expand` — reconcile JSON-driven lookups and
/// re-run variant expansion. Skips all network steps. Use after editing
/// `data/variants.json` or `data/tcgcsv_sub_type_variants.json`, or
/// after a migration that adds a bridge entry the catalog doesn't yet
/// have linked in `tcgplayer_groups`.
fn expand_only(args: RefreshArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    println!("Reconciling variants table from data/variants.json...");
    let n_variants = pkdump_db::variants::reconcile(&mut conn)?;
    println!("  {n_variants} variant rows reconciled");

    println!("Reconciling tcgcsv_sub_type_variant_map...");
    let n_sub = pkdump_db::sub_type_map::reconcile(&mut conn)?;
    println!("  {n_sub} (group, sub_type) → variant rows");

    println!("Reconciling bundles table from data/bundles.json...");
    let n_bundles = pkdump_db::bundles::reconcile(&mut conn)?;
    println!("  {n_bundles} bundles registered");

    // Re-run set discovery too: it's local (it reads the TCGCSV products
    // already in the DB), so editing data/overrides/tcgcsv_set_discovery
    // .json takes effect here without a network refresh.
    println!("Discovering new sets from unbridged TCGCSV groups...");
    for d in pkdump_ingest::set_discovery::discover_new_sets(&mut conn)? {
        println!(
            "  {} ({}) — {} from group {}, {} cards",
            d.set_code, d.series, d.name, d.group_id, d.cards
        );
    }

    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    // `data expand` is the local re-run, so its clock is genuinely now: there
    // are no landed bytes behind it to reproduce a timestamp from.
    let printings =
        overrides::expand_all_printings(&mut conn, &overlay, &chrono::Utc::now().to_rfc3339())?;
    println!("  wrote {printings} printings");

    println!("Checking TCGplayer mapping coverage...");
    coverage::report_unmapped_sets(&conn)?;
    Ok(())
}

/// Execute `pkdump data normalize-symbols` — just the symbols phase, no
/// network catalog work.
fn normalize_symbols(args: RefreshArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    let mut conn = pkdump_db::open_shared(&db_path)?;
    let data_dir = db_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    println!("Normalizing set symbol glyphs...");
    let s = symbols::normalize_all_symbols(&mut conn, &data_dir)?;
    println!(
        "  {} processed, {} cached, {} overrides, {} failed",
        s.processed, s.cached, s.overrides, s.failed
    );
    Ok(())
}

/// Execute `pkdump data refresh`.
///
/// The pipeline itself lives in `pkdump-derive` — the same code the offline
/// `pkdump-lake-derive shared` job runs, so item 3's row-identical comparison
/// is between two runs of one derivation over two sources of the same bytes,
/// not between two implementations. What is left here is the online half: the
/// database to open, whether to land, and reading the clock.
///
/// Nothing in this file reads `raw/`, and nothing in `pkdump-cli` can — that
/// is the boundary the whole epic turns on. `--from-raw` belongs on the
/// offline job, which is a different binary in a different crate.
///
/// ## Exit status (pd-nons)
///
/// | | |
/// | --- | --- |
/// | 0 | every upstream was acquired and every phase ran |
/// | 2 | **partial**: the pokemontcg.io tail failed after exhausting its retries; the run continued, TCGCSV was acquired and the catalog derived |
/// | 1 | the run failed |
///
/// 2 is a distinct status because the two outcomes want different answers: a
/// tail that fails one night costs a day's set list, a TCGCSV pull that fails
/// costs a day's prices permanently. It is deliberately **not** wired to
/// `SuccessExitStatus=` in `deploy/pkdump-refresh.service` — a set list that
/// silently stopped advancing is exactly the failure nothing else on the box
/// would report, so a partial run still pages. See the unit for the argument.
fn refresh(args: RefreshCmdArgs) -> anyhow::Result<()> {
    let db_path = match args.common.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    // The run's clock, read ONCE — see `pkdump_derive::clock`. It picks the
    // ingest_date partition, it is recorded in every manifest, and it is what
    // every fetched_at / observed_at column in this run gets, so an offline
    // derive from the bytes this run lands can reproduce them exactly.
    let clock = pkdump_derive::DeriveClock::now();

    // Resolved before anything is fetched: a landing zone that was asked for
    // and is not configured should stop the run at the start, not after an
    // hour of requests whose bytes then have nowhere to go.
    let landing = crate::landing::open(args.land_raw, &clock)?;

    let data_dir = db_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let report = pkdump_derive::derive(
        &mut conn,
        &pkdump_derive::Options {
            clock,
            data_dir: &data_dir,
            landing,
            // The online side never replays. It has no way to: reading the
            // landing zone is `pkdump-lakehouse`'s job and this crate does
            // not depend on it.
            replay: None,
        },
    )?;

    if let Some(e) = report.tail_error {
        eprintln!("!! Refresh PARTIAL: {}", db_path.display());
        eprintln!("!!   the pokemontcg.io tail failed after its retries: {e}");
        eprintln!(
            "!!   The run CONTINUED past it: TCGCSV groups, products and prices were acquired \
             and the catalog was derived from them. The set list is as old as the last run that \
             finished one. Exit status 2."
        );
        // Not an `Err`: anyhow's main would print the error and exit 1, which
        // is the status a run that acquired nothing carries. This one
        // acquired the perishable half.
        drop(conn);
        std::process::exit(2);
    }

    println!("Refresh complete: {}", db_path.display());
    Ok(())
}
