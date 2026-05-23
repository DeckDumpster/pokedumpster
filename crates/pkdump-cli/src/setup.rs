//! `pkdump setup` — build the shared catalog database from upstream sources.
//!
//! Pipeline (PLAN.md §4.1):
//!   1. bulk import the `pokemon-tcg-data` repo (sets + cards),
//!   2. fill the tail of newest sets from the pokemontcg.io API,
//!   3. import TCGCSV groups, sealed products, and prices,
//!   4. run three-layer variant expansion into `printings`.
//!
//! Steps 2 and 3 hit the network; `--skip-tail` / `--skip-prices` turn them
//! off, and `--from-dir` imports a local repo checkout instead of
//! downloading. With all three, setup runs fully offline.

use std::path::PathBuf;

use rusqlite::Connection;

use pkdump_ingest::pokemontcg::PokemonTcgClient;
use pkdump_ingest::tcgcsv::TcgcsvClient;
use pkdump_ingest::{overrides, pokemon_tcg_data, tcgcsv};

/// Arguments for `pkdump setup`.
#[derive(clap::Args)]
pub struct SetupArgs {
    /// Import the catalog from a local pokemon-tcg-data checkout instead of
    /// downloading the repo tarball.
    #[arg(long, value_name = "DIR")]
    from_dir: Option<PathBuf>,

    /// Skip the pokemontcg.io tail fetch (sets newer than the repo).
    #[arg(long)]
    skip_tail: bool,

    /// Skip the TCGCSV sealed-product and price import.
    #[arg(long)]
    skip_prices: bool,

    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

/// Execute `pkdump setup`.
pub fn run(args: SetupArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    // 1. Bulk catalog import.
    let stats = match &args.from_dir {
        Some(dir) => {
            println!("Importing pokemon-tcg-data from {}", dir.display());
            pokemon_tcg_data::import_from_dir(&mut conn, dir)?
        }
        None => {
            println!("Downloading the pokemon-tcg-data repo...");
            pokemon_tcg_data::download_and_import(&mut conn)?
        }
    };
    println!("  imported {} sets, {} cards", stats.sets, stats.cards);

    // 2. pokemontcg.io tail.
    if args.skip_tail {
        println!("Skipping pokemontcg.io tail fetch.");
    } else {
        println!("Filling newest sets from pokemontcg.io...");
        let added = import_tail(&mut conn)?;
        println!("  added {added} set(s) not yet in the repo");
    }

    // 3. TCGCSV groups, sealed products, single-card products, prices.
    //    Variant expansion (step 4) reads this back out as its
    //    authoritative source.
    if args.skip_prices {
        println!("Skipping TCGCSV import.");
    } else {
        println!("Importing TCGCSV groups, products, prices...");
        let r = import_tcgcsv(&mut conn)?;
        println!(
            "  {} groups, {} sealed products, {} card products, {} price rows",
            r.0, r.1, r.2, r.3
        );
    }

    // 4. Variant expansion. TCGCSV-derived first (each printing carries
    //    its sub_type_name + tcgplayer_product_id), overlay on top for
    //    cards TCGCSV can't model (stamps, etc.).
    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    let printings = overrides::expand_all_printings(&mut conn, &overlay)?;
    println!("  wrote {printings} printings");

    println!("Setup complete: {}", db_path.display());
    Ok(())
}

/// Fetch the pokemontcg.io set list and import any set the repo did not have.
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

#[cfg(test)]
mod tests {
    use super::*;

    const SETS_FIXTURE: &str = r#"[
      {"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
       "printedTotal":165,"total":207,"ptcgoCode":"MEW",
       "releaseDate":"2023/09/22"}
    ]"#;

    const CARDS_FIXTURE: &str = r#"[
      {"id":"sv3pt5-1","name":"Bulbasaur","supertype":"Pokémon",
       "subtypes":["Basic"],"hp":"70","types":["Grass"],"number":"1",
       "rarity":"Common",
       "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"},
       "tcgplayer":{"prices":{"normal":{},"reverseHolofoil":{}}}},
      {"id":"sv3pt5-4","name":"Charizard ex","supertype":"Pokémon",
       "subtypes":["Basic","ex"],"hp":"330","types":["Fire"],"number":"4",
       "rarity":"Double Rare",
       "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"},
       "tcgplayer":{"prices":{"holofoil":{}}}}
    ]"#;

    #[test]
    fn setup_from_dir_offline_builds_catalog() {
        // A minimal pokemon-tcg-data checkout.
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("sets")).unwrap();
        std::fs::create_dir_all(repo.path().join("cards").join("en")).unwrap();
        std::fs::write(repo.path().join("sets").join("en.json"), SETS_FIXTURE).unwrap();
        std::fs::write(
            repo.path().join("cards").join("en").join("sv3pt5.json"),
            CARDS_FIXTURE,
        )
        .unwrap();

        let dbdir = tempfile::tempdir().unwrap();
        let db_path = dbdir.path().join("shared.sqlite");

        run(SetupArgs {
            from_dir: Some(repo.path().to_path_buf()),
            skip_tail: true,
            skip_prices: true,
            db: Some(db_path.clone()),
        })
        .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let sets: i64 = conn
            .query_row("SELECT count(*) FROM sets", [], |r| r.get(0))
            .unwrap();
        let cards: i64 = conn
            .query_row("SELECT count(*) FROM cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sets, 1);
        assert_eq!(cards, 2);

        // Variant expansion ran. skip_prices means TCGCSV is absent, so
        // each card falls back to the bare `normal` placeholder; the 151
        // overlay then adds pokeball_rh + masterball_rh for Bulbasaur
        // (Common) and stops at `normal` for Charizard ex (no overlay
        // rule). A real refresh with TCGCSV present would replace these
        // with the true sub_type-derived variants.
        let bulbasaur: i64 = conn
            .query_row(
                "SELECT count(*) FROM printings \
                 WHERE card_id = 'sv3pt5-1' AND deprecated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bulbasaur, 3);

        let charizard: i64 = conn
            .query_row(
                "SELECT count(*) FROM printings \
                 WHERE card_id = 'sv3pt5-4' AND deprecated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(charizard, 1);
    }
}
