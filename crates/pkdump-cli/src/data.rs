//! `pkdump data refresh` — the nightly LANDING run.
//!
//! It fetches the tail of newest sets from the pokemontcg.io API and every
//! TCGCSV group's products and prices, and writes each response into the
//! `raw/` landing zone. It builds nothing: since pd-lunn the catalog has ONE
//! builder, `pkdump-lake-derive shared`, which replays this partition some
//! hours later. `deploy/pkdump-derive.timer` is what runs it, and on a box
//! running this command it is not optional.
//!
//! What is left in this file is the online half of a run: which database to
//! read, where the bytes go, and the clock. **Nothing here reads `raw/`** —
//! that is the boundary the lakehouse epic turns on, and it is why the
//! deriving job is a different binary in a different crate rather than a
//! `--from-raw` flag on this one.

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
    /// Fetch every upstream and land it in `raw/`. Builds no catalog —
    /// `pkdump-lake-derive shared` does that, from the partition this run
    /// lands (pd-lunn).
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
///
/// **There is no `--land-raw` any more.** Landing was the optional half of a
/// command whose other half built the catalog; with that half deleted, a run
/// that does not land does nothing at all, so landing is unconditional and
/// `~/.config/pkdump/lake.env` is required. A flag left behind for a path that
/// no longer exists is a flag a runbook can still reach for.
#[derive(clap::Args)]
pub struct RefreshCmdArgs {
    #[command(flatten)]
    common: RefreshArgs,
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

/// Execute `pkdump data refresh` — fetch every upstream and LAND it.
///
/// ## It does not build the catalog any more (pd-lunn)
///
/// It used to: this function called [`pkdump_derive::derive`] and wrote
/// `shared.sqlite`, which meant the catalog had two builders. That is why
/// `pkdump-derive@<instance>.timer` stayed disabled everywhere — arming it
/// only did the same work a second time, from the bytes the first run had just
/// landed, overwriting a catalog that was already right.
///
/// Item 6 of the lake-as-source epic picks one. `pkdump-lake-derive shared` is
/// the builder; this command is the LANDING half and nothing else, which is
/// what the two units have claimed to be since item 5 shipped. The blocking
/// question — does a catalog replayed from `raw/` equal the one fetched
/// online — was answered against prod's own nightly partition on 2026-08-25:
/// row-identical across twenty tables, 12.6M price rows included.
///
/// So the shape here is now:
///
/// - the catalog is opened **read-only** (`open_shared_readonly`), and is
///   asked one question: which sets it already has. A read-only handle is why
///   "the refresh writes no catalog table" is a fact about the connection
///   rather than a claim about this function.
/// - landing is **required**, not a flag. See [`crate::landing::require`].
/// - the derivation happens hours later, in its own unit, from the partition
///   this run landed. `deploy/pkdump-derive.timer` is no longer optional on a
///   box that runs this: without it the catalog simply stops advancing.
///
/// ## Exit status (pd-nons)
///
/// | | |
/// | --- | --- |
/// | 0 | every upstream was acquired and landed |
/// | 2 | **partial**: the pokemontcg.io tail failed after exhausting its retries; the run continued and TCGCSV — the half a night cannot get back — was landed |
/// | 1 | the run failed |
///
/// 2 is a distinct status because the two outcomes want different answers: a
/// tail that fails one night costs a day's set list, a TCGCSV pull that fails
/// costs a day's prices permanently. It is deliberately **not** wired to
/// `SuccessExitStatus=` in `deploy/pkdump-refresh.service` — a set list that
/// silently stopped advancing is exactly the failure nothing else on the box
/// would report, so a partial run still reaches the wrapper's stall check. See
/// the unit for the argument.
fn refresh(args: RefreshCmdArgs) -> anyhow::Result<()> {
    let db_path = match args.common.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog READ-ONLY at {}", db_path.display());
    let conn = pkdump_db::open_shared_readonly(&db_path).map_err(|e| {
        anyhow::anyhow!(
            "{e}\n\nThe refresh reads the catalog to decide which sets are new; it does not \
             create one. If this box has never been set up, run `pkdump setup` first."
        )
    })?;

    // The run's clock, read ONCE — see `pkdump_derive::clock`. It picks the
    // ingest_date partition and it is recorded in every manifest, which is
    // what lets the offline derive stamp the same fetched_at / observed_at
    // into the same rows from these bytes.
    let clock = pkdump_derive::DeriveClock::now();

    // Resolved before anything is fetched: a landing zone that cannot be
    // opened stops the run at the start, not after an hour of requests whose
    // bytes then have nowhere to go.
    let landing = crate::landing::require(&clock)?;

    // Only the symbol phase reads this, and that phase is the deriving side's,
    // so nothing here opens it. Passed anyway rather than left to a default:
    // the field is not optional, and the catalog's own directory is the answer
    // every other command gives.
    let data_dir = db_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let report = pkdump_derive::land(
        &conn,
        &pkdump_derive::Options {
            clock,
            data_dir: &data_dir,
            landing: Some(landing),
            // The online side never replays. It has no way to: reading the
            // landing zone is `pkdump-lakehouse`'s job and this crate does
            // not depend on it.
            replay: None,
        },
    )?;

    if let Some(e) = report.tail_error {
        eprintln!("!! Refresh PARTIAL: nothing was derived, and the tail is short");
        eprintln!("!!   the pokemontcg.io tail failed after its retries: {e}");
        eprintln!(
            "!!   The run CONTINUED past it: TCGCSV groups, products and prices were fetched \
             and landed. Tonight's partition can still be derived; its set list will be as old \
             as the last partition that carried a whole one. Exit status 2."
        );
        // Not an `Err`: anyhow's main would print the error and exit 1, which
        // is the status a run that landed nothing carries. This one landed the
        // perishable half.
        drop(conn);
        std::process::exit(2);
    }

    println!("Refresh complete: landed, not derived. The catalog is built by pkdump-lake-derive.");
    Ok(())
}
