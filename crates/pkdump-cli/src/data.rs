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
use pkdump_ingest::{overrides, pokemon_tcg_data, symbols, tcgcsv};

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
}

/// Arguments for `pkdump data refresh`.
#[derive(clap::Args)]
pub struct RefreshArgs {
    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

/// Execute `pkdump data`.
pub fn run(args: DataArgs) -> anyhow::Result<()> {
    match args.command {
        DataCommand::Refresh(args) => refresh(args),
        DataCommand::NormalizeSymbols(args) => normalize_symbols(args),
    }
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

    // 1. pokemontcg.io tail — pick up sets released since the last refresh.
    println!("Filling newest sets from pokemontcg.io...");
    let added = import_tail(&mut conn)?;
    println!("  added {added} set(s) not yet in the catalog");

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

    // 3. Synthesize card rows for bridged TCGCSV groups whose upstream
    //    pokemontcg.io entry doesn't exist yet (e.g. MEP). Idempotent
    //    INSERT OR IGNORE — once pokemontcg.io publishes the real set,
    //    upserts from import_tail win and synthesized stubs stand down.
    println!("Synthesizing cards for bridged groups...");
    let n_synth = tcgcsv::synthesize_cards_for_bridges(&mut conn)?;
    println!("  {n_synth} cards synthesized");

    // 4. Variant expansion. TCGCSV is authoritative for which printings a
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

    println!("Refresh complete: {}", db_path.display());
    Ok(())
}

/// Fetch the pokemontcg.io set list and import any set the catalog lacks.
fn import_tail(conn: &mut Connection) -> anyhow::Result<usize> {
    let client = PokemonTcgClient::new()?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut added = 0;
    for set in client.fetch_sets()? {
        let exists: bool = conn
            .prepare("SELECT 1 FROM sets WHERE set_code = ?1")?
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
