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
use pkdump_ingest::{overrides, pokemon_tcg_data, tcgcsv};

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
    }
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

    // 2. Variant expansion — runs before TCGCSV so the printings exist for
    //    the price-linking step. Idempotent.
    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    let printings = overrides::expand_all_printings(&mut conn, &overlay)?;
    println!("  wrote {printings} printings");

    // 3. TCGCSV groups, sealed products, a fresh price snapshot, and links.
    println!("Importing TCGCSV groups, sealed products, prices, links...");
    let r = import_tcgcsv(&mut conn)?;
    println!(
        "  {} groups, {} sealed products, {} price rows, {} cards linked",
        r.0, r.1, r.2, r.3
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
            pokemon_tcg_data::upsert_card(conn, &card)?;
        }
        added += 1;
    }
    Ok(added)
}

/// Import every TCGCSV group with its sealed products, a price snapshot, and
/// printing↔product links. Returns (groups, sealed products, price rows,
/// cards linked).
fn import_tcgcsv(conn: &mut Connection) -> anyhow::Result<(usize, usize, usize, usize)> {
    let client = TcgcsvClient::new()?;
    let now = chrono::Utc::now().to_rfc3339();
    let observed = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let groups = client.fetch_groups()?;
    let n_groups = tcgcsv::import_groups(conn, &groups, &now)?;

    let mut n_sealed = 0;
    let mut n_prices = 0;
    let mut n_linked = 0;
    for group in &groups {
        let products = client.fetch_products(group.group_id)?;
        n_sealed += tcgcsv::import_sealed_products(conn, &products, &now)?;
        n_linked += tcgcsv::link_card_printings(conn, &products)?;
        let prices = client.fetch_prices(group.group_id)?;
        n_prices += tcgcsv::import_prices(conn, &prices, &observed)?;
    }
    Ok((n_groups, n_sealed, n_prices, n_linked))
}
