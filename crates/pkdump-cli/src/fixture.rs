//! `pkdump seed-fixture` — build the deterministic Pokémon test fixture for
//! the intents UI-testing harness.
//!
//! Produces two SQLite files in `--out` (default `tests/ui/fixtures`):
//!
//! * `shared.sqlite` — immutable catalog (sets, cards, printings, prices).
//! * `collection.sqlite` — per-user mutable data (binders, decks, batches,
//!   orders, sealed, wishlist, views, collection).
//!
//! Both databases are built through PokeDumpster's own code so the schema
//! init runs and the running server can open them unchanged. The intents
//! harness snapshots/restores these before each test.
//!
//! The data is small, deterministic, and Pokémon-authentic; intent YAML/hint
//! files reference it by the stable names documented in
//! `tests/ui/fixtures/README.md`.

use std::path::PathBuf;

use rusqlite::Connection;

use pkdump_db::{
    batches::{self, NewBatch},
    binders::{self, NewBinder},
    collection::{self, NewCopy},
    decks::{self, NewDeck},
    orders::{self, NewOrder, OrderLine},
    sealed::{self, NewSealed},
    wishlist::{self, NewWish},
};

/// Arguments for `pkdump seed-fixture`.
#[derive(clap::Args)]
pub struct FixtureArgs {
    /// Output directory for `shared.sqlite` and `collection.sqlite`.
    #[arg(long, value_name = "DIR", default_value = "tests/ui/fixtures")]
    out: PathBuf,
}

/// Execute `pkdump seed-fixture`.
pub fn run(args: FixtureArgs) -> anyhow::Result<()> {
    std::fs::create_dir_all(&args.out)?;
    let shared_path = args.out.join("shared.sqlite");
    let user_path = args.out.join("collection.sqlite");

    // Clean rebuild — stale rows would break determinism, even though
    // the new IF NOT EXISTS schema would otherwise let us reuse the file.
    for p in [&shared_path, &user_path] {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }

    println!("Building shared catalog at {}", shared_path.display());
    let shared = pkdump_db::open_shared(&shared_path)?;
    seed_catalog(&shared)?;
    // The keyword registry, rarity ranks and flag definitions fixture-backed
    // search tests need came in with the read-write open above
    // (connection.rs::converge).
    // Materialize latest_prices from the seeded prices (vi37) so fixture-backed
    // pages show market values without re-running a full catalog refresh.
    pkdump_db::latest_prices::refresh_latest_prices(&shared)?;
    let (sets, cards, printings, prices) = catalog_counts(&shared)?;
    println!("  {sets} sets, {cards} cards, {printings} printings, {prices} prices");
    drop(shared);

    println!("Building user collection at {}", user_path.display());
    let mut user = pkdump_db::connect_user(&user_path, &shared_path)?;
    seed_user(&mut user)?;
    let copies: i64 = user.query_row("SELECT count(*) FROM collection", [], |r| r.get(0))?;
    println!("  {copies} collection copies seeded");
    // Reconstruct value-history snapshots from the seeded prices + copies so
    // fixture-backed value-chart tests have data (deterministic: fixed prices,
    // fixed acquired_at). (pokedumpster-e1vo)
    let snaps = pkdump_db::value_history::backfill(&mut user)?;
    println!("  {snaps} value-history snapshot rows");

    println!("Fixture ready: {}", args.out.display());
    Ok(())
}

fn catalog_counts(conn: &Connection) -> anyhow::Result<(i64, i64, i64, i64)> {
    Ok((
        conn.query_row("SELECT count(*) FROM sets", [], |r| r.get(0))?,
        conn.query_row("SELECT count(*) FROM cards", [], |r| r.get(0))?,
        conn.query_row("SELECT count(*) FROM printings", [], |r| r.get(0))?,
        conn.query_row("SELECT count(*) FROM prices", [], |r| r.get(0))?,
    ))
}

// ---------------------------------------------------------------------------
// Catalog (shared.sqlite)
// ---------------------------------------------------------------------------

/// A fixed observation date so prices are byte-for-byte reproducible.
const OBSERVED_AT: &str = "2024-01-15";
/// A fixed ingest timestamp for the catalog freshness markers.
const FETCHED_AT: &str = "2024-01-15T00:00:00Z";

/// Compact card spec: number, name, supertype, rarity, artist, hp.
struct CardSpec {
    number: &'static str,
    sortable: i64,
    name: &'static str,
    supertype: &'static str,
    rarity: &'static str,
    artist: &'static str,
    hp: Option<i64>,
}

/// Variants to materialise for a card, with optional TCGplayer product link.
struct PrintingSpec {
    variant: &'static str,
    sub_type: Option<&'static str>,
    /// TCGplayer product id; `Some` means a `prices` row is written too.
    product_id: Option<i64>,
    market_price: f64,
}

/// A card paired with the printings to materialise for it.
type CardEntry = (CardSpec, Vec<PrintingSpec>);

/// A set row: set_code, ptcgo_code, name, series, series_sort, set_sort,
/// total, printed_total, release_date.
type SetRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    i64,
    i64,
    i64,
    i64,
    &'static str,
);

/// A sealed-product row: product_id, set_code, name, category, card_count,
/// product_size, release_date.
type SealedRow = (
    i64,
    &'static str,
    &'static str,
    &'static str,
    Option<i64>,
    Option<i64>,
    &'static str,
);

fn seed_catalog(conn: &Connection) -> anyhow::Result<()> {
    // --- Sets ----------------------------------------------------------
    // (set_code, ptcgo_code, name, series, series_sort, set_sort,
    //  total, printed_total, release_date)
    let sets: [SetRow; 3] = [
        (
            "base1",
            "BS",
            "Base Set",
            "Base",
            1,
            1,
            102,
            102,
            "1999/01/09",
        ),
        (
            "sv3pt5",
            "MEW",
            "151",
            "Scarlet & Violet",
            9,
            35,
            165,
            207,
            "2023/09/22",
        ),
        (
            "sv8",
            "SSP",
            "Surging Sparks",
            "Scarlet & Violet",
            9,
            42,
            191,
            252,
            "2024/11/08",
        ),
    ];
    for (code, ptcgo, name, series, ss, sst, total, printed, release) in sets {
        conn.execute(
            "INSERT INTO sets \
               (set_code, ptcgo_code, name, series, series_sort_order, \
                set_sort_order, total, printed_total, release_date, \
                logo_url, symbol_url, ptcgio_fetched_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                code,
                ptcgo,
                name,
                series,
                ss,
                sst,
                total,
                printed,
                release,
                format!("https://images.pokemontcg.io/{code}/logo.png"),
                format!("https://images.pokemontcg.io/{code}/symbol.png"),
                FETCHED_AT,
            ],
        )?;
    }

    // --- Cards + printings + prices ------------------------------------
    // Product ids are allocated deterministically per linked printing.
    seed_set_cards(conn, "base1", &base_set_cards())?;
    seed_set_cards(conn, "sv3pt5", &set_151_cards())?;
    seed_set_cards(conn, "sv8", &surging_sparks_cards())?;
    enrich_card_facets(conn)?;

    // --- Sealed products ----------------------------------------------
    // (product_id, set_code, name, category, card_count, product_size,
    //  release_date)
    let sealed: [SealedRow; 4] = [
        (
            900001,
            "base1",
            "Base Set Booster Box",
            "booster_box",
            Some(11),
            Some(36),
            "1999/01/09",
        ),
        (
            900002,
            "sv3pt5",
            "151 Elite Trainer Box",
            "etb",
            Some(10),
            Some(9),
            "2023/09/22",
        ),
        (
            900003,
            "sv3pt5",
            "151 Booster Bundle",
            "bundle",
            Some(10),
            Some(6),
            "2023/09/22",
        ),
        (
            900004,
            "sv8",
            "Surging Sparks Booster Pack",
            "booster_pack",
            Some(10),
            Some(1),
            "2024/11/08",
        ),
    ];
    for (pid, set, name, cat, cc, size, release) in sealed {
        conn.execute(
            "INSERT INTO sealed_products \
               (product_id, set_code, name, category, card_count, \
                product_size, release_date, image_url, tcgplayer_url, fetched_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                pid,
                set,
                name,
                cat,
                cc,
                size,
                release,
                format!("https://product-images.tcgplayer.com/{pid}.jpg"),
                format!("https://www.tcgplayer.com/product/{pid}"),
                FETCHED_AT,
            ],
        )?;
    }

    Ok(())
}

/// Insert one set's cards, their printings, and a price row per linked
/// printing. Product ids are deterministic: `set_base + card_index*10 +
/// printing_index`.
fn seed_set_cards(conn: &Connection, set_code: &str, cards: &[CardEntry]) -> anyhow::Result<()> {
    for (ci, (card, printings)) in cards.iter().enumerate() {
        let card_id = format!("{set_code}-{}", card.number);
        conn.execute(
            "INSERT INTO cards \
               (card_id, set_code, number, number_sortable, name, supertype, \
                rarity, artist, hp, image_small, image_large) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                card_id,
                set_code,
                card.number,
                card.sortable,
                card.name,
                card.supertype,
                card.rarity,
                card.artist,
                card.hp,
                format!(
                    "https://images.pokemontcg.io/{set_code}/{}.png",
                    card.number
                ),
                format!(
                    "https://images.pokemontcg.io/{set_code}/{}_hires.png",
                    card.number
                ),
            ],
        )?;

        for (pi, p) in printings.iter().enumerate() {
            let printing_id = format!("{card_id}-{}", p.variant);
            // sub_type_name links a printing to its prices row (the market_price
            // join in search.rs keys on it). Production ingest populates it; the
            // fixture must too, or every price reads back null (pokedumpster-qm9).
            conn.execute(
                "INSERT INTO printings \
                   (printing_id, card_id, variant, language, tcgplayer_product_id, \
                    sub_type_name) \
                 VALUES (?1, ?2, ?3, 'en', ?4, ?5)",
                rusqlite::params![printing_id, card_id, p.variant, p.product_id, p.sub_type],
            )?;

            if let (Some(pid), Some(sub)) = (p.product_id, p.sub_type) {
                conn.execute(
                    "INSERT INTO prices \
                       (tcgplayer_product_id, sub_type_name, source, \
                        price_type, price, observed_at) \
                     VALUES (?1, ?2, 'tcgplayer', 'market', ?3, ?4)",
                    rusqlite::params![pid, sub, p.market_price, OBSERVED_AT],
                )?;
            }
            let _ = (ci, pi);
        }
    }
    Ok(())
}

/// Backfill the Pokémon-specific facet columns the `card()` helper doesn't
/// carry — energy `types` (the headline search facet), plus a little
/// `subtypes`/`attacks` on flagship cards — keyed by name so the UI search
/// tests (and `/collection` screenshots) can exercise `t:`, `sub:`, `dmg:`,
/// `has:attack`, etc. against believable data.
fn enrich_card_facets(conn: &Connection) -> anyhow::Result<()> {
    // (name, energy type)
    let types: &[(&str, &str)] = &[
        ("Charizard", "Fire"),
        ("Charizard ex", "Fire"),
        ("Charmander", "Fire"),
        ("Magmar", "Fire"),
        ("Blastoise", "Water"),
        ("Squirtle", "Water"),
        ("Milotic ex", "Water"),
        ("Venusaur", "Grass"),
        ("Bulbasaur", "Grass"),
        ("Exeggcute", "Grass"),
        ("Alolan Exeggutor ex", "Grass"),
        ("Pikachu", "Lightning"),
        ("Pikachu ex", "Lightning"),
        ("Raichu", "Lightning"),
        ("Alakazam ex", "Psychic"),
        ("Mew", "Psychic"),
        ("Mew ex", "Psychic"),
        ("Latias ex", "Psychic"),
        ("Hitmonchan", "Fighting"),
        ("Ditto", "Colorless"),
        ("Snorlax", "Colorless"),
    ];
    for (name, ty) in types {
        conn.execute(
            "UPDATE cards SET types = ?1 WHERE name = ?2",
            rusqlite::params![format!("[\"{ty}\"]"), name],
        )?;
    }

    // Stage subtypes for the evolved Base Set holos, and Basic+ex for the ex
    // cards — enough to demonstrate sub: queries.
    for name in ["Charizard", "Blastoise", "Venusaur"] {
        conn.execute(
            "UPDATE cards SET subtypes = '[\"Stage 2\"]' WHERE name = ?1",
            rusqlite::params![name],
        )?;
    }
    conn.execute(
        "UPDATE cards SET subtypes = '[\"Basic\",\"ex\"]' WHERE name LIKE '% ex'",
        [],
    )?;

    // A couple of attacks so dmg:/has:attack/o: have something to match.
    conn.execute(
        "UPDATE cards SET attacks = '[{\"name\":\"Fire Spin\",\"damage\":\"100\",\
            \"cost\":[\"Fire\",\"Fire\",\"Fire\",\"Fire\"],\"text\":\"Discard 2 Energy.\"}]' \
         WHERE name = 'Charizard'",
        [],
    )?;
    conn.execute(
        "UPDATE cards SET attacks = '[{\"name\":\"Thunder Jolt\",\"damage\":\"30\",\
            \"cost\":[\"Lightning\"],\"text\":\"Flip a coin.\"}]' \
         WHERE name = 'Pikachu'",
        [],
    )?;

    // National Pokédex numbers — drives the card-detail "Pokédex" row and the
    // pokedex: facet. Mew + Mew ex share #151 so pokedex:151 returns two cards.
    let dex: &[(&str, &str)] = &[
        ("Bulbasaur", "[1]"),
        ("Venusaur", "[3]"),
        ("Charmander", "[4]"),
        ("Charizard", "[6]"),
        ("Charizard ex", "[6]"),
        ("Squirtle", "[7]"),
        ("Blastoise", "[9]"),
        ("Pikachu", "[25]"),
        ("Pikachu ex", "[25]"),
        ("Raichu", "[26]"),
        ("Alakazam ex", "[65]"),
        ("Exeggcute", "[102]"),
        ("Alolan Exeggutor ex", "[103]"),
        ("Hitmonchan", "[107]"),
        ("Magmar", "[126]"),
        ("Ditto", "[132]"),
        ("Snorlax", "[143]"),
        ("Mew", "[151]"),
        ("Mew ex", "[151]"),
        ("Milotic ex", "[350]"),
        ("Latias ex", "[380]"),
    ];
    for (name, nums) in dex {
        conn.execute(
            "UPDATE cards SET national_pokedex_numbers = ?1 WHERE name = ?2",
            rusqlite::params![nums, name],
        )?;
    }

    // Type weaknesses — drives the weakness: facet (cards sharing a weakness).
    // All four Fire Pokémon take Water weakness, so weakness:Water returns them.
    let weak: &[(&str, &str)] = &[
        ("Charizard", "Water"),
        ("Charizard ex", "Water"),
        ("Charmander", "Water"),
        ("Magmar", "Water"),
        ("Blastoise", "Lightning"),
        ("Squirtle", "Lightning"),
        ("Milotic ex", "Lightning"),
        ("Venusaur", "Fire"),
        ("Bulbasaur", "Fire"),
        ("Exeggcute", "Fire"),
        ("Alolan Exeggutor ex", "Fire"),
        ("Pikachu", "Fighting"),
        ("Pikachu ex", "Fighting"),
        ("Raichu", "Fighting"),
        ("Snorlax", "Fighting"),
        ("Ditto", "Fighting"),
        ("Alakazam ex", "Psychic"),
        ("Mew", "Psychic"),
        ("Mew ex", "Psychic"),
        ("Latias ex", "Psychic"),
    ];
    for (name, ty) in weak {
        conn.execute(
            "UPDATE cards SET weaknesses = ?1 WHERE name = ?2",
            rusqlite::params![format!("[{{\"type\":\"{ty}\",\"value\":\"×2\"}}]"), name],
        )?;
    }

    // Retreat costs (N colourless) — drives the retreat: facet (same cost
    // count). Charizard + Blastoise both retreat for 3, so retreat:3 pairs them.
    let retreat: &[(&str, usize)] = &[
        ("Charizard", 3),
        ("Blastoise", 3),
        ("Snorlax", 4),
        ("Charizard ex", 2),
        ("Venusaur", 2),
        ("Pikachu", 1),
        ("Pikachu ex", 1),
        ("Raichu", 1),
        ("Charmander", 1),
        ("Squirtle", 1),
        ("Bulbasaur", 1),
        ("Mew", 1),
        ("Mew ex", 1),
        ("Ditto", 1),
    ];
    for (name, n) in retreat {
        let arr = format!("[{}]", vec!["\"Colorless\""; *n].join(","));
        conn.execute(
            "UPDATE cards SET retreat_cost = ?1 WHERE name = ?2",
            rusqlite::params![arr, name],
        )?;
    }

    // Abilities — drives the ability: facet. Classic Base Set powers.
    conn.execute(
        "UPDATE cards SET abilities = '[{\"name\":\"Rain Dance\",\"type\":\"Pokémon Power\",\
            \"text\":\"As often as you like during your turn, you may attach 1 Water Energy \
            card to 1 of your Water Pokémon.\"}]' WHERE name = 'Blastoise'",
        [],
    )?;
    conn.execute(
        "UPDATE cards SET abilities = '[{\"name\":\"Energy Trans\",\"type\":\"Pokémon Power\",\
            \"text\":\"Move 1 Grass Energy card from 1 of your Pokémon to another.\"}]' \
         WHERE name = 'Venusaur'",
        [],
    )?;

    // Evolution chains — extracted from raw_json by the card-detail query, so
    // set just the evolves keys. Pikachu ⇄ Raichu is fully present in the
    // fixture, exercising the name: search both links resolve to.
    conn.execute(
        "UPDATE cards SET raw_json = '{\"evolvesTo\":[\"Raichu\"]}' WHERE name = 'Pikachu'",
        [],
    )?;
    conn.execute(
        "UPDATE cards SET raw_json = '{\"evolvesFrom\":\"Pikachu\"}' WHERE name = 'Raichu'",
        [],
    )?;
    Ok(())
}

/// `normal` + `reverse_holo` for a common/uncommon. Two product ids.
fn np_reverse(base: i64, normal: f64, reverse: f64) -> Vec<PrintingSpec> {
    vec![
        PrintingSpec {
            variant: "normal",
            sub_type: Some("Normal"),
            product_id: Some(base),
            market_price: normal,
        },
        PrintingSpec {
            variant: "reverse_holo",
            sub_type: Some("Reverse Holofoil"),
            product_id: Some(base + 1),
            market_price: reverse,
        },
    ]
}

/// A single `holo` printing — for rares with no non-foil version.
fn holo_only(base: i64, price: f64) -> Vec<PrintingSpec> {
    vec![PrintingSpec {
        variant: "holo",
        sub_type: Some("Holofoil"),
        product_id: Some(base),
        market_price: price,
    }]
}

/// `holo` + `reverse_holo` — the common shape for a Base Set holo rare.
fn holo_reverse(base: i64, holo: f64, reverse: f64) -> Vec<PrintingSpec> {
    vec![
        PrintingSpec {
            variant: "holo",
            sub_type: Some("Holofoil"),
            product_id: Some(base),
            market_price: holo,
        },
        PrintingSpec {
            variant: "reverse_holo",
            sub_type: Some("Reverse Holofoil"),
            product_id: Some(base + 1),
            market_price: reverse,
        },
    ]
}

fn card(
    number: &'static str,
    sortable: i64,
    name: &'static str,
    supertype: &'static str,
    rarity: &'static str,
    artist: &'static str,
    hp: Option<i64>,
) -> CardSpec {
    CardSpec {
        number,
        sortable,
        name,
        supertype,
        rarity,
        artist,
        hp,
    }
}

/// Base Set — 9 classic cards spanning Common → Rare Holo.
fn base_set_cards() -> Vec<CardEntry> {
    vec![
        (
            card(
                "4",
                4,
                "Charizard",
                "Pokémon",
                "Rare Holo",
                "Mitsuhiro Arita",
                Some(120),
            ),
            holo_only(100040, 320.00),
        ),
        (
            card(
                "2",
                2,
                "Blastoise",
                "Pokémon",
                "Rare Holo",
                "Ken Sugimori",
                Some(100),
            ),
            holo_only(100020, 140.00),
        ),
        (
            card(
                "15",
                15,
                "Venusaur",
                "Pokémon",
                "Rare Holo",
                "Mitsuhiro Arita",
                Some(100),
            ),
            holo_only(100150, 110.00),
        ),
        (
            card(
                "58",
                58,
                "Pikachu",
                "Pokémon",
                "Common",
                "Mitsuhiro Arita",
                Some(40),
            ),
            np_reverse(100580, 6.50, 12.00),
        ),
        (
            card(
                "24",
                24,
                "Raichu",
                "Pokémon",
                "Rare",
                "Ken Sugimori",
                Some(80),
            ),
            holo_only(100240, 18.00),
        ),
        (
            card(
                "46",
                46,
                "Bulbasaur",
                "Pokémon",
                "Common",
                "Mitsuhiro Arita",
                Some(40),
            ),
            np_reverse(100460, 3.50, 7.00),
        ),
        (
            card(
                "7",
                7,
                "Hitmonchan",
                "Pokémon",
                "Rare Holo",
                "Ken Sugimori",
                Some(70),
            ),
            holo_only(100070, 22.00),
        ),
        (
            card(
                "88",
                88,
                "Energy Removal",
                "Trainer",
                "Common",
                "Keiji Kinebuchi",
                None,
            ),
            np_reverse(100880, 0.75, 2.00),
        ),
        (
            card(
                "98",
                98,
                "Fire Energy",
                "Energy",
                "Common",
                "Keiji Kinebuchi",
                None,
            ),
            np_reverse(100980, 0.25, 1.00),
        ),
    ]
}

/// 151 (sv3pt5) — 13 cards, including a secret rare numbered above the
/// printed total of 165.
fn set_151_cards() -> Vec<CardEntry> {
    vec![
        (
            card(
                "1",
                1,
                "Bulbasaur",
                "Pokémon",
                "Common",
                "Narumi Sato",
                Some(70),
            ),
            np_reverse(200010, 0.50, 1.25),
        ),
        (
            card(
                "4",
                4,
                "Charmander",
                "Pokémon",
                "Common",
                "Tefoll",
                Some(60),
            ),
            np_reverse(200040, 1.00, 2.50),
        ),
        (
            card(
                "6",
                6,
                "Charizard ex",
                "Pokémon",
                "Double Rare",
                "5ban Graphics",
                Some(330),
            ),
            holo_only(200060, 24.00),
        ),
        (
            card("7", 7, "Squirtle", "Pokémon", "Common", "Sekio", Some(60)),
            np_reverse(200070, 0.50, 1.25),
        ),
        (
            card(
                "25",
                25,
                "Pikachu",
                "Pokémon",
                "Common",
                "Saboteri",
                Some(60),
            ),
            np_reverse(200250, 1.50, 4.00),
        ),
        (
            card(
                "65",
                65,
                "Alakazam ex",
                "Pokémon",
                "Double Rare",
                "5ban Graphics",
                Some(310),
            ),
            holo_only(200650, 9.00),
        ),
        (
            card(
                "131",
                131,
                "Mew",
                "Pokémon",
                "Rare",
                "Sanosuke Sakuma",
                Some(70),
            ),
            holo_reverse(201310, 3.00, 5.00),
        ),
        (
            card(
                "105",
                105,
                "Ditto",
                "Pokémon",
                "Uncommon",
                "Ryuta Fuse",
                Some(60),
            ),
            np_reverse(201050, 0.40, 1.10),
        ),
        (
            card(
                "151",
                151,
                "Snorlax",
                "Pokémon",
                "Uncommon",
                "Kagemaru Himeno",
                Some(140),
            ),
            np_reverse(201510, 0.60, 1.50),
        ),
        (
            card(
                "166",
                166,
                "Bulbasaur",
                "Pokémon",
                "Illustration Rare",
                "Saboteri",
                Some(70),
            ),
            holo_only(201660, 8.50),
        ),
        (
            card(
                "199",
                199,
                "Charizard ex",
                "Pokémon",
                "Special Illustration Rare",
                "Akira Egawa",
                Some(330),
            ),
            holo_only(201990, 280.00),
        ),
        (
            card(
                "201",
                201,
                "Mew ex",
                "Pokémon",
                "Hyper Rare",
                "aky CG Works",
                Some(260),
            ),
            holo_only(202010, 95.00),
        ),
        (
            card(
                "165",
                165,
                "Professor's Research",
                "Trainer",
                "Illustration Rare",
                "Yuu Nishida",
                None,
            ),
            holo_only(201650, 14.00),
        ),
    ]
}

/// Surging Sparks (sv8) — 9 cards, modern rarities + a secret rare above the
/// printed total of 191.
fn surging_sparks_cards() -> Vec<CardEntry> {
    vec![
        (
            card(
                "3",
                3,
                "Exeggcute",
                "Pokémon",
                "Common",
                "Shibuzoh.",
                Some(50),
            ),
            np_reverse(300030, 0.20, 0.90),
        ),
        (
            card(
                "32",
                32,
                "Magmar",
                "Pokémon",
                "Common",
                "Naoki Saito",
                Some(80),
            ),
            np_reverse(300320, 0.25, 1.00),
        ),
        (
            card(
                "57",
                57,
                "Pikachu ex",
                "Pokémon",
                "Double Rare",
                "PLANETA Igarashi",
                Some(200),
            ),
            holo_only(300570, 12.00),
        ),
        (
            card(
                "89",
                89,
                "Milotic ex",
                "Pokémon",
                "Double Rare",
                "5ban Graphics",
                Some(310),
            ),
            holo_only(300890, 6.50),
        ),
        (
            card(
                "120",
                120,
                "Boss's Orders",
                "Trainer",
                "Uncommon",
                "Yusuke Ishikawa",
                None,
            ),
            np_reverse(301200, 0.75, 2.25),
        ),
        (
            card(
                "160",
                160,
                "Latias ex",
                "Pokémon",
                "Illustration Rare",
                "Teradon",
                Some(220),
            ),
            holo_only(301600, 18.00),
        ),
        (
            card(
                "191",
                191,
                "Alolan Exeggutor ex",
                "Pokémon",
                "Illustration Rare",
                "PLANETA Mochizuki",
                Some(280),
            ),
            holo_only(301910, 11.00),
        ),
        (
            card(
                "238",
                238,
                "Pikachu ex",
                "Pokémon",
                "Special Illustration Rare",
                "PLANETA Mochizuki",
                Some(200),
            ),
            holo_only(302380, 130.00),
        ),
        (
            card(
                "252",
                252,
                "Iono",
                "Trainer",
                "Special Illustration Rare",
                "Saji",
                None,
            ),
            holo_only(302520, 40.00),
        ),
    ]
}

// ---------------------------------------------------------------------------
// User data (collection.sqlite)
// ---------------------------------------------------------------------------

fn seed_user(conn: &mut Connection) -> anyhow::Result<()> {
    // --- Binders -------------------------------------------------------
    let trade_binder = binders::create(
        conn,
        &NewBinder {
            name: "Trade Binder".into(),
            description: Some("Spare cards available for trade.".into()),
            color: "red".into_some(),
            binder_type: "trade".into_some(),
            pocket_size: Some(9),
            storage_location: "Shelf A".into_some(),
        },
    )?;
    let masters = binders::create(
        conn,
        &NewBinder {
            name: "Master Set: 151".into(),
            description: Some("Working toward a complete 151 master set.".into()),
            color: "blue".into_some(),
            binder_type: "set".into_some(),
            pocket_size: Some(12),
            storage_location: "Shelf A".into_some(),
        },
    )?;
    let vault = binders::create(
        conn,
        &NewBinder {
            name: "Vintage Vault".into(),
            description: Some("High-value vintage holos.".into()),
            color: "gold".into_some(),
            binder_type: "showcase".into_some(),
            pocket_size: Some(9),
            storage_location: "Safe".into_some(),
        },
    )?;

    // --- Decks (idea / ready / built) ----------------------------------
    let charizard_deck = decks::create(
        conn,
        &NewDeck {
            name: "Charizard ex Control".into(),
            description: Some("Fire-type control built around Charizard ex.".into()),
            format: "standard".into_some(),
            owner: "Ryan".into_some(),
            state: "built".into_some(),
            sleeve_color: "black".into_some(),
            storage_location: "Deck Box 1".into_some(),
            notes: "Tournament-ready.".into_some(),
        },
    )?;
    let _pikachu_deck = decks::create(
        conn,
        &NewDeck {
            name: "Pikachu ex Aggro".into(),
            description: Some("Fast Lightning aggro.".into()),
            format: "standard".into_some(),
            owner: "Ryan".into_some(),
            state: "ready".into_some(),
            sleeve_color: "yellow".into_some(),
            storage_location: "Deck Box 2".into_some(),
            notes: None,
        },
    )?;
    let _vintage_deck = decks::create(
        conn,
        &NewDeck {
            name: "Vintage Base Brawl".into(),
            description: Some("A casual deck idea using Base Set classics.".into()),
            format: "casual".into_some(),
            owner: "Alice".into_some(),
            state: "idea".into_some(),
            sleeve_color: None,
            storage_location: None,
            notes: "Needs more energy.".into_some(),
        },
    )?;

    // --- Orders --------------------------------------------------------
    // One received order and one still-open order. `orders::create` seeds
    // its own batch + `ordered` collection rows; `receive` flips them owned.
    let received_order = orders::create(
        conn,
        &NewOrder {
            order_number: "TCG-100001".into_some(),
            source: "tcgplayer".into(),
            seller_name: "CardKingdomLA".into_some(),
            order_date: "2024-01-05".into_some(),
            subtotal: Some(31.50),
            shipping: Some(4.99),
            tax: Some(2.84),
            total: Some(39.33),
            shipping_status: "delivered".into_some(),
            estimated_delivery: "2024-01-12".into_some(),
            notes: "Singles for the Charizard deck.".into_some(),
        },
        &[
            OrderLine {
                printing_id: "sv3pt5-6-holo".into(),
                quantity: 1,
                purchase_price: Some(24.00),
            },
            OrderLine {
                printing_id: "sv3pt5-4-normal".into(),
                quantity: 2,
                purchase_price: Some(1.00),
            },
        ],
    )?;
    orders::receive(conn, received_order)?;

    let _open_order = orders::create(
        conn,
        &NewOrder {
            order_number: "EBAY-55012".into_some(),
            source: "ebay".into(),
            seller_name: "vintage_pulls".into_some(),
            order_date: "2024-01-14".into_some(),
            subtotal: Some(320.00),
            shipping: Some(0.00),
            tax: Some(0.00),
            total: Some(320.00),
            shipping_status: "shipped".into_some(),
            estimated_delivery: "2024-01-22".into_some(),
            notes: "Base Set Charizard — still in transit.".into_some(),
        },
        &[OrderLine {
            printing_id: "base1-4-holo".into(),
            quantity: 1,
            purchase_price: Some(320.00),
        }],
    )?;

    // --- Batches -------------------------------------------------------
    // (Orders already created their own batches; these are the manual /
    // binder-click / CSV ingest batches.)
    let manual_batch = batches::create(
        conn,
        &NewBatch {
            batch_type: "manual_id".into(),
            name: "Vintage holo entry".into_some(),
            notes: "Hand-entered Base Set holos.".into_some(),
            order_id: None,
            binder_id: Some(vault),
        },
    )?;
    let click_batch = batches::create(
        conn,
        &NewBatch {
            batch_type: "binder_click".into(),
            name: "151 binder page-through".into_some(),
            notes: "Registered via the binder-page browser.".into_some(),
            order_id: None,
            binder_id: Some(masters),
        },
    )?;
    let csv_batch = batches::create(
        conn,
        &NewBatch {
            batch_type: "csv_manabox".into(),
            name: "ManaBox export 2024-01".into_some(),
            notes: "Bulk import from a ManaBox CSV.".into_some(),
            order_id: None,
            binder_id: Some(trade_binder),
        },
    )?;

    // --- Collection copies --------------------------------------------
    // Each tuple: printing_id, condition, status, source, binder, deck,
    // batch, purchase_price, notes.
    struct Copy {
        printing_id: &'static str,
        condition: &'static str,
        status: &'static str,
        source: &'static str,
        binder: Option<i64>,
        deck: Option<i64>,
        batch: Option<i64>,
        price: Option<f64>,
        notes: Option<&'static str>,
    }
    let copies = vec![
        // Vintage holos — manual batch, Vintage Vault binder.
        Copy {
            printing_id: "base1-4-holo",
            condition: "Lightly Played",
            status: "owned",
            source: "manual_id",
            binder: Some(vault),
            deck: None,
            batch: Some(manual_batch),
            price: Some(280.00),
            notes: Some("Childhood card — well loved."),
        },
        Copy {
            printing_id: "base1-2-holo",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: Some(vault),
            deck: None,
            batch: Some(manual_batch),
            price: Some(140.00),
            notes: None,
        },
        Copy {
            printing_id: "base1-15-holo",
            condition: "Moderately Played",
            status: "owned",
            source: "manual_id",
            binder: Some(vault),
            deck: None,
            batch: Some(manual_batch),
            price: Some(95.00),
            notes: None,
        },
        Copy {
            printing_id: "base1-7-holo",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: Some(vault),
            deck: None,
            batch: Some(manual_batch),
            price: Some(22.00),
            notes: None,
        },
        // A graded card (set via direct UPDATE after insert).
        Copy {
            printing_id: "base1-24-holo",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: Some(vault),
            deck: None,
            batch: Some(manual_batch),
            price: Some(60.00),
            notes: Some("PSA 8 slab."),
        },
        // 151 binder-click registrations — Master Set binder.
        Copy {
            printing_id: "sv3pt5-1-normal",
            condition: "Near Mint",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv3pt5-1-reverse_holo",
            condition: "Near Mint",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv3pt5-25-normal",
            condition: "Near Mint",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv3pt5-25-reverse_holo",
            condition: "Lightly Played",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv3pt5-131-holo",
            condition: "Near Mint",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: Some(3.00),
            notes: None,
        },
        Copy {
            printing_id: "sv3pt5-166-holo",
            condition: "Near Mint",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: Some(8.50),
            notes: Some("Bulbasaur illustration rare."),
        },
        Copy {
            printing_id: "sv3pt5-201-holo",
            condition: "Near Mint",
            status: "owned",
            source: "binder_click",
            binder: Some(masters),
            deck: None,
            batch: Some(click_batch),
            price: Some(95.00),
            notes: Some("Mew ex hyper rare — chase card."),
        },
        // ManaBox CSV import — Trade Binder.
        Copy {
            printing_id: "sv8-3-normal",
            condition: "Near Mint",
            status: "owned",
            source: "csv_manabox",
            binder: Some(trade_binder),
            deck: None,
            batch: Some(csv_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv8-32-normal",
            condition: "Near Mint",
            status: "owned",
            source: "csv_manabox",
            binder: Some(trade_binder),
            deck: None,
            batch: Some(csv_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv8-120-normal",
            condition: "Lightly Played",
            status: "owned",
            source: "csv_manabox",
            binder: Some(trade_binder),
            deck: None,
            batch: Some(csv_batch),
            price: None,
            notes: None,
        },
        Copy {
            printing_id: "sv8-160-holo",
            condition: "Near Mint",
            status: "listed",
            source: "csv_manabox",
            binder: Some(trade_binder),
            deck: None,
            batch: Some(csv_batch),
            price: Some(18.00),
            notes: Some("Listed for trade — Latias ex."),
        },
        // Cards built into the Charizard ex Control deck.
        Copy {
            printing_id: "sv3pt5-6-holo",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: None,
            deck: Some(charizard_deck),
            batch: None,
            price: Some(24.00),
            notes: Some("Deck centrepiece."),
        },
        Copy {
            printing_id: "sv3pt5-4-normal",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: None,
            deck: Some(charizard_deck),
            batch: None,
            price: Some(1.00),
            notes: None,
        },
        Copy {
            printing_id: "sv3pt5-7-normal",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: None,
            deck: Some(charizard_deck),
            batch: None,
            price: None,
            notes: None,
        },
        // Loose owned copies — unassigned.
        Copy {
            printing_id: "sv8-57-holo",
            condition: "Near Mint",
            status: "owned",
            source: "manual_id",
            binder: None,
            deck: None,
            batch: None,
            price: Some(12.00),
            notes: Some("Pikachu ex — undecided home."),
        },
        Copy {
            printing_id: "sv3pt5-105-normal",
            condition: "Heavily Played",
            status: "owned",
            source: "manual_id",
            binder: None,
            deck: None,
            batch: None,
            price: None,
            notes: None,
        },
        // A sold copy — exercises the disposed-status path.
        Copy {
            printing_id: "sv3pt5-65-holo",
            condition: "Near Mint",
            status: "sold",
            source: "manual_id",
            binder: None,
            deck: None,
            batch: None,
            price: Some(9.00),
            notes: Some("Sold to a friend."),
        },
    ];
    for c in &copies {
        let id = collection::add(
            conn,
            &NewCopy {
                printing_id: c.printing_id.into(),
                condition: c.condition.into_some(),
                language: None,
                purchase_price: c.price,
                acquired_at: Some("2024-01-10T12:00:00Z".into()),
                source: c.source.into(),
                status: c.status.into_some(),
                notes: c.notes.map(str::to_owned),
                order_id: None,
                binder_id: c.binder,
                deck_id: c.deck,
                batch_id: c.batch,
            },
        )?;
        // `NewCopy` has no grading fields — apply the one graded card here.
        if c.printing_id == "base1-24-holo" {
            conn.execute(
                "UPDATE collection SET graded = 1, grade_company = 'PSA', \
                   grade_value = 8.0, grade_cert = '12345678' WHERE id = ?1",
                [id],
            )?;
        }
    }

    // --- Sealed collection --------------------------------------------
    sealed::add(
        conn,
        &NewSealed {
            product_id: 900002,
            quantity: Some(1),
            condition: "Near Mint".into_some(),
            purchase_price: Some(49.99),
            purchase_date: "2023-09-22".into_some(),
            source: "pokemoncenter".into_some(),
            seller_name: "Pokémon Center".into_some(),
            notes: "Sealed 151 ETB — keeping factory sealed.".into_some(),
        },
    )?;
    sealed::add(
        conn,
        &NewSealed {
            product_id: 900004,
            quantity: Some(6),
            condition: "Near Mint".into_some(),
            purchase_price: Some(4.49),
            purchase_date: "2024-11-08".into_some(),
            source: "lgs".into_some(),
            seller_name: "Local Game Store".into_some(),
            notes: "Loose Surging Sparks packs to rip.".into_some(),
        },
    )?;

    // --- Wishlist ------------------------------------------------------
    wishlist::add(
        conn,
        &NewWish {
            card_id: "sv3pt5-199".into(),
            printing_id: "sv3pt5-199-holo".into_some(),
            max_price: Some(250.00),
            priority: Some(3),
            notes: "Charizard ex SIR — top of the want list.".into_some(),
        },
    )?;
    wishlist::add(
        conn,
        &NewWish {
            card_id: "sv8-238".into(),
            printing_id: None,
            max_price: Some(120.00),
            priority: Some(2),
            notes: "Pikachu ex SIR from Surging Sparks.".into_some(),
        },
    )?;
    wishlist::add(
        conn,
        &NewWish {
            card_id: "base1-4".into(),
            printing_id: "base1-4-holo".into_some(),
            max_price: Some(300.00),
            priority: Some(1),
            notes: "Already ordered — kept as a price watch.".into_some(),
        },
    )?;

    Ok(())
}

/// Tiny ergonomics helper so the data tables read cleanly: `"x".into_some()`
/// instead of `Some("x".into())`.
trait IntoSome {
    fn into_some(self) -> Option<String>;
}
impl IntoSome for &str {
    fn into_some(self) -> Option<String> {
        Some(self.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The committed catalog fixture, addressed from the crate rather than
    /// from whatever directory the test runner happens to stand in.
    fn committed_shared() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/ui/fixtures/shared.sqlite")
    }

    /// Every catalog object a database holds, mapped to its columns.
    /// `pragma_table_info` answers for views as well as tables, which is what
    /// makes `latest_sealed_prices` covered by the same pass.
    fn objects(conn: &Connection) -> rusqlite::Result<BTreeMap<String, Vec<String>>> {
        let names: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master
                  WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        let mut out = BTreeMap::new();
        for name in names {
            let columns: Vec<String> = conn
                .prepare("SELECT name FROM pragma_table_info(?1)")?
                .query_map([&name], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            out.insert(name, columns);
        }
        Ok(out)
    }

    /// The committed `shared.sqlite` must carry every table, view and column
    /// `schema_shared.sql` declares — because it is the ONE database this
    /// project never repairs on open. Every other file converges: the schema
    /// is re-applied with `CREATE … IF NOT EXISTS` and `add_missing_columns`
    /// ALTERs the rest. The catalog is ATTACHed READ-ONLY at request time, so
    /// a fixture generated before a schema addition stays behind forever, and
    /// the first query that names the new object dies with `no such table`.
    ///
    /// `PRAGMA user_version` cannot stand in for this check. Additive change
    /// travels by idempotent re-application and deliberately does not bump the
    /// version, so the stale fixture that produced this test reported the
    /// CURRENT version while missing two tables (pd-7hkf).
    ///
    /// Only objects the schema declares and the fixture lacks are a failure.
    /// A table the schema has since dropped, still sitting in the fixture,
    /// breaks no read — "behind" is the direction that hurts.
    #[test]
    fn the_committed_fixture_carries_every_catalog_object_the_schema_declares() {
        let dir = tempfile::tempdir().unwrap();
        let reference = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        let declared = objects(&reference).unwrap();
        // Not vacuous: a reference that declared nothing would pass whatever
        // the fixture holds.
        assert!(
            !declared.is_empty(),
            "schema_shared.sql declared no objects"
        );

        let fixture = pkdump_db::open_shared_readonly(&committed_shared()).unwrap();
        let present = objects(&fixture).unwrap();

        let mut missing = Vec::new();
        for (name, columns) in &declared {
            match present.get(name) {
                None => missing.push(name.clone()),
                Some(have) => missing.extend(
                    columns
                        .iter()
                        .filter(|c| !have.contains(c))
                        .map(|c| format!("{name}.{c}")),
                ),
            }
        }

        assert!(
            missing.is_empty(),
            "tests/ui/fixtures/shared.sqlite is behind schema_shared.sql \
             and is never repaired on open (it is ATTACHed read-only). \
             Missing: {}. Regenerate it: cargo run --bin pkdump -- seed-fixture",
            missing.join(", "),
        );
    }
}
