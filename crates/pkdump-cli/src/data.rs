//! `pkdump data refresh` — nightly incremental catalog update.
//!
//! Unlike `pkdump setup`, this skips the `pokemon-tcg-data` bulk import. It
//! re-fetches the tail of newest sets from the pokemontcg.io API, re-imports
//! TCGCSV groups, sealed products, and prices, then re-applies the dirty-data
//! overrides. Each run appends a fresh price snapshot (a new `observed_at`)
//! to the `prices` table — the source for a future price-history chart.

use std::path::PathBuf;

use rusqlite::Connection;

use pkdump_ingest::pokemontcg::PokemonTcgClient;
use pkdump_ingest::tcgcsv::TcgcsvClient;
use pkdump_ingest::{japan, overrides, pokemon_tcg_data, symbols, tcgcsv};

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
    Refresh(RefreshArgs),
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
    /// `collection_value_snapshot` table. The nightly `refresh` records
    /// today going forward; this seeds the past. Idempotent.
    BackfillValueHistory(RefreshArgs),
}

/// Arguments for `pkdump data refresh`.
#[derive(clap::Args)]
pub struct RefreshArgs {
    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
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
    let printings = overrides::expand_all_printings(&mut conn, &overlay)?;
    println!("  wrote {printings} printings");
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
fn refresh(args: RefreshArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    // 1. Reconcile the variants table from data/variants.json — runs
    //    first because it's purely local (no network) and idempotent.
    //    Putting it ahead of the network calls means a flaky upstream
    //    can't keep variants.json edits from landing on the next refresh.
    println!("Reconciling variants table from data/variants.json...");
    let n_variants = pkdump_db::variants::reconcile(&mut conn)?;
    println!("  {n_variants} variant rows reconciled");

    // 1b. Reconcile (group, sub_type) → variant map from
    //     data/tcgcsv_sub_type_variants.json. Lives next to the
    //     variants seed and follows the same idempotent-reconcile
    //     pattern. Variant expansion (step 3 below) reads this back.
    println!("Reconciling tcgcsv_sub_type_variant_map...");
    let n_sub = pkdump_db::sub_type_map::reconcile(&mut conn)?;
    println!("  {n_sub} (group, sub_type) → variant rows");

    // 1c. Reconcile the bundles registry from data/bundles.json. Drives
    //     the /api/sets dispatch for TTBB-style containers.
    println!("Reconciling bundles table from data/bundles.json...");
    let n_bundles = pkdump_db::bundles::reconcile(&mut conn)?;
    println!("  {n_bundles} bundles registered");

    // 1d. Reconcile the search query language metadata from
    //     data/search_*.json (local + idempotent).
    println!("Reconciling search query metadata...");
    let sm = pkdump_db::search_meta::reconcile(&mut conn)?;
    println!(
        "  {} keywords, {} rarities, {} flags",
        sm.keywords, sm.rarities, sm.flags
    );

    // 2. pokemontcg.io tail — pick up sets released since the last refresh.
    println!("Filling newest sets from pokemontcg.io...");
    let added = import_tail(&mut conn)?;
    println!("  added {added} set(s) not yet in the catalog");

    // 2b. Re-apply the upstream-correction registry to rows already in the
    //     catalog. `upsert_card` above only corrects the sets import_tail
    //     just added — a correction registered after a card landed would
    //     otherwise never reach its row. Runs before variant expansion so
    //     downstream phases see the corrected numbers.
    println!("Re-applying upstream card corrections...");
    let healed = pokemon_tcg_data::apply_corrections_to_db(&conn)?;
    for h in &healed {
        println!(
            "  {} number {} -> {}",
            h.card_id, h.current_number, h.corrected_number
        );
    }
    println!("  {} row(s) healed", healed.len());

    // 2. TCGCSV groups, sealed products, single-card products, prices —
    //    raw ingest of everything TCGCSV publishes. Variant expansion in
    //    step 3 reads this back out to determine which printings actually
    //    exist for each card.
    println!("Importing TCGCSV groups, products, prices...");
    let r = import_tcgcsv(&mut conn)?;
    println!(
        "  {} groups, {} sealed products, {} card products, {} price rows",
        r.0, r.1, r.2, r.3
    );

    // 2b. Pokémon Japan (TCGCSV categoryId 85) — sets and cards are
    //     synthesized straight from TCGCSV, there being no pokemontcg.io
    //     counterpart. Runs after the English pass so the two never
    //     contend for a set_code. See `pkdump_ingest::japan`.
    println!("Importing the Pokémon Japan catalog (TCGCSV category 85)...");
    let j = japan::import_all(
        &mut conn,
        &chrono::Utc::now().to_rfc3339(),
        &chrono::Utc::now().format("%Y-%m-%d").to_string(),
    )?;
    println!(
        "  {} groups, {} cards, {} card products, {} sealed products, {} price rows",
        j.groups, j.cards, j.card_products, j.sealed_products, j.price_rows
    );

    // 2c. Auto-discover sets TCGCSV has published and pokemontcg.io
    //     hasn't — a numbered expansion group that bridges to nothing
    //     becomes a set + cards on its own, no hand-authored bridge and
    //     no waiting on upstream (pd-558b1e4f). Reads the products just
    //     imported, so it has to run after import_tcgcsv — and after the
    //     Japanese import, which bridges every category-85 group and so
    //     keeps them out of the unbridged pool discovery works from.
    println!("Discovering new sets from unbridged TCGCSV groups...");
    for d in pkdump_ingest::set_discovery::discover_new_sets(&mut conn)? {
        println!(
            "  {} ({}) — {} from group {}, {} cards",
            d.set_code, d.series, d.name, d.group_id, d.cards
        );
    }

    // 3. Synthesize card rows for bridged TCGCSV groups whose upstream
    //    pokemontcg.io entry doesn't exist yet (e.g. MEP). Idempotent
    //    INSERT OR IGNORE — once pokemontcg.io publishes the real set,
    //    upserts from import_tail win and synthesized stubs stand down.
    println!("Synthesizing cards for bridged groups...");
    let n_synth = tcgcsv::synthesize_cards_for_bridges(&mut conn)?;
    println!("  {n_synth} cards synthesized");

    // Curated standalone promos (Ancient Mew, etc.) — see setup.rs step 5b.
    let n_promo = pkdump_ingest::standalone_promos::synthesize_standalone_promos(&mut conn)?;
    println!("  {n_promo} standalone promos synthesized");

    // 5. Variant expansion. TCGCSV is authoritative for which printings a
    //    card has; the overlay still applies for cards TCGCSV can't model
    //    (cross-group stamped promos, etc.). Each printing carries its
    //    sub_type_name + tcgplayer_product_id so price queries stay a
    //    straight JOIN.
    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    let printings = overrides::expand_all_printings(&mut conn, &overlay)?;
    println!("  wrote {printings} printings");

    // 5. Normalize set symbol glyphs for any new sets the tail fetch added.
    //    Existing rows already point at /sym/<set>.png and are skipped via
    //    the http-prefix gate in normalize_all_symbols.
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

    // 6. Rebuild the materialized latest-prices table so the per-row
    //    market-price lookup on the collection/search/binder pages stays a
    //    point read rather than a GROUP BY over all of prices (vi37).
    println!("Refreshing materialized latest_prices...");
    let n_latest = pkdump_db::latest_prices::refresh_latest_prices(&conn)?;
    println!("  {n_latest} latest-price rows materialized");

    // 7. Snapshot today's collection value into the user DB (value-history
    //    chart, pokedumpster-e1vo). Value snapshots live in the *user* DB, so
    //    this opens a separate user connection with the just-refreshed shared
    //    catalog attached — it reads the materialized latest_prices written
    //    directly above, so it must run after that step.
    use std::io::Write;
    println!("Snapshotting today's collection value...");
    std::io::stdout().flush().ok();
    let user_db = crate::collection::user_db()?;
    let mut user_conn = pkdump_db::connect_user(&user_db, &db_path)?;
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let n_snap = pkdump_db::value_history::snapshot_today(&mut user_conn, &today)?;
    println!("  {n_snap} value-snapshot rows written for {today}");
    std::io::stdout().flush().ok();

    println!("Refresh complete: {}", db_path.display());
    Ok(())
}

/// Fetch the pokemontcg.io set list and import any set the catalog lacks.
///
/// A set row that exists but carries no `ptcgio_fetched_at` was
/// synthesized locally — from a bridge entry, or by TCGCSV set discovery
/// while upstream was still behind. Those count as missing: importing them
/// is exactly how the real cards supersede the synthesized stubs the day
/// pokemontcg.io publishes the set.
fn import_tail(conn: &mut Connection) -> anyhow::Result<usize> {
    let client = PokemonTcgClient::new()?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut added = 0;
    for set in client.fetch_sets()? {
        let exists: bool = conn
            .prepare("SELECT 1 FROM sets WHERE set_code = ?1 AND ptcgio_fetched_at IS NOT NULL")?
            .exists([&set.id])?;
        if exists {
            continue;
        }
        pokemon_tcg_data::upsert_set(conn, &set, &now)?;
        for card in client.fetch_cards_for_set(&set.id)? {
            pokemon_tcg_data::upsert_card(conn, &card, &set.id)?;
        }
        added += 1;
    }
    Ok(added)
}

/// Import every TCGCSV group: sealed products, single-card products
/// (persisted to `tcgcsv_products` for variant expansion to read), and a
/// fresh price snapshot. Returns (groups, sealed products, card products,
/// price rows).
fn import_tcgcsv(conn: &mut Connection) -> anyhow::Result<(usize, usize, usize, usize)> {
    let client = TcgcsvClient::new()?;
    let now = chrono::Utc::now().to_rfc3339();
    let observed = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let groups = client.fetch_groups()?;
    let n_groups = tcgcsv::import_groups(conn, &groups, &now)?;

    let mut n_sealed = 0;
    let mut n_cards = 0;
    let mut n_prices = 0;
    for group in &groups {
        let products = client.fetch_products(group.group_id)?;
        n_sealed += tcgcsv::import_sealed_products(conn, &products, &now)?;
        n_cards += tcgcsv::import_products(conn, &products, &now)?;
        let prices = client.fetch_prices(group.group_id)?;
        n_prices += tcgcsv::import_prices(conn, &prices, &observed)?;
    }
    Ok((n_groups, n_sealed, n_cards, n_prices))
}
