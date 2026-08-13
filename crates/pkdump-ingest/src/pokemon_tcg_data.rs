//! Importer for the `PokemonTCG/pokemon-tcg-data` GitHub repository.
//!
//! This repo is the primary bulk catalog source (RESEARCH.md §2): one JSON
//! file per set under `cards/en/<setid>.json`, plus `sets/en.json`. It uses
//! the same schema as the pokemontcg.io API minus the price blocks. The
//! pokemontcg.io client fills the 2–3 month tail of newest sets the repo
//! lags on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pkdump_lake::{Dataset, PartFormat, Source};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{IngestError, Result};
use crate::landing::{self, Wire};
use crate::pokemontcg::{PokemonTcgCard, PokemonTcgSet, cards_from_values};

const UPSTREAM_CARD_CORRECTIONS: &str =
    include_str!("../../../data/overrides/upstream_card_corrections.json");

#[derive(Debug, Clone, Deserialize)]
struct UpstreamCardCorrection {
    card_id: String,
    /// Overrides `cards.number` when pokemontcg.io's `number` disagrees
    /// with the canonical id/image filename. `raw_json` is preserved
    /// verbatim — only the materialized column is corrected.
    #[serde(default)]
    number: Option<String>,
}

fn upstream_card_corrections() -> &'static HashMap<String, UpstreamCardCorrection> {
    static MAP: OnceLock<HashMap<String, UpstreamCardCorrection>> = OnceLock::new();
    MAP.get_or_init(|| {
        let entries: Vec<UpstreamCardCorrection> = serde_json::from_str(UPSTREAM_CARD_CORRECTIONS)
            .expect("upstream_card_corrections.json failed to parse");
        entries
            .into_iter()
            .map(|c| (c.card_id.clone(), c))
            .collect()
    })
}

/// Apply field-level corrections to a `PokemonTcgCard` before it's
/// written to the catalog. Upstream pokemontcg.io occasionally ships
/// rows where its own `id` and `number` disagree (e.g. `zsv10pt5-80`
/// shipped with `number="60"`). The override registry is the registered
/// list of those known cases — keyed by card_id.
fn apply_upstream_card_correction(card: &mut PokemonTcgCard) {
    if let Some(c) = upstream_card_corrections().get(&card.id)
        && let Some(n) = &c.number
    {
        card.number = n.clone();
    }
}

/// A catalog row the correction registry disagrees with — either its
/// `number` or the `number_sortable` derived from it. Produced by
/// [`pending_corrections`], applied by [`apply_corrections_to_db`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCorrection {
    pub card_id: String,
    pub current_number: String,
    pub current_number_sortable: i64,
    pub corrected_number: String,
    pub corrected_number_sortable: i64,
}

/// Scan already-ingested rows for ones the correction registry disagrees
/// with. `upsert_card` only corrects cards as they are ingested, and
/// `pkdump data refresh` skips sets already in the catalog — so a
/// correction added (or edited) after a card landed never reaches its
/// row without this pass.
///
/// Registered `card_id`s the catalog doesn't have are skipped, as are
/// rows that already match. Results are sorted by `card_id` so the
/// dry-run report is stable.
pub fn pending_corrections(conn: &Connection) -> Result<Vec<PendingCorrection>> {
    use rusqlite::OptionalExtension;

    let mut stmt = conn.prepare("SELECT number, number_sortable FROM cards WHERE card_id = ?1")?;
    let mut pending = Vec::new();
    for (card_id, correction) in upstream_card_corrections() {
        let Some(corrected_number) = &correction.number else {
            continue;
        };
        let row: Option<(String, i64)> = stmt
            .query_row([card_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        // Not ingested yet — `upsert_card` will apply the correction when
        // the card first lands.
        let Some((current_number, current_number_sortable)) = row else {
            continue;
        };
        let corrected_number_sortable = pkdump_core::number_sortable(corrected_number);
        if current_number == *corrected_number
            && current_number_sortable == corrected_number_sortable
        {
            continue;
        }
        pending.push(PendingCorrection {
            card_id: card_id.clone(),
            current_number,
            current_number_sortable,
            corrected_number: corrected_number.clone(),
            corrected_number_sortable,
        });
    }
    pending.sort_by(|a, b| a.card_id.cmp(&b.card_id));
    Ok(pending)
}

/// Re-apply the correction registry to rows already in the catalog,
/// returning the rows that changed. Idempotent — a second run finds
/// nothing pending and writes nothing. `raw_json` is left untouched, the
/// same convention `upsert_card` follows.
pub fn apply_corrections_to_db(conn: &Connection) -> Result<Vec<PendingCorrection>> {
    let pending = pending_corrections(conn)?;
    let mut stmt =
        conn.prepare("UPDATE cards SET number = ?2, number_sortable = ?3 WHERE card_id = ?1")?;
    for p in &pending {
        stmt.execute(rusqlite::params![
            p.card_id,
            p.corrected_number,
            p.corrected_number_sortable,
        ])?;
    }
    Ok(pending)
}

const REPO_TARBALL: &str =
    "https://codeload.github.com/PokemonTCG/pokemon-tcg-data/tar.gz/refs/heads/master";

/// Counts produced by an import run.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImportStats {
    pub sets: usize,
    pub cards: usize,
}

/// Import sets and cards from a local checkout of the pokemon-tcg-data repo.
/// `dir` must contain `sets/en.json` and `cards/en/<setid>.json`. Idempotent —
/// re-running upserts in place.
pub fn import_from_dir(conn: &mut Connection, dir: &Path) -> Result<ImportStats> {
    let sets_path = dir.join("sets").join("en.json");
    let sets_text = std::fs::read_to_string(&sets_path)
        .map_err(|e| IngestError::BadResponse(format!("{}: {e}", sets_path.display())))?;
    // The repo stores bare JSON arrays — there is no API-style `data` envelope.
    let sets: Vec<PokemonTcgSet> = serde_json::from_str(&sets_text)?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut stats = ImportStats::default();
    let tx = conn.transaction()?;
    for set in &sets {
        upsert_set(&tx, set, &now)?;
        stats.sets += 1;

        let card_path = dir
            .join("cards")
            .join("en")
            .join(format!("{}.json", set.id));
        if !card_path.exists() {
            // A set can exist before its card file is published — skip it.
            continue;
        }
        let card_text = std::fs::read_to_string(&card_path)
            .map_err(|e| IngestError::BadResponse(format!("{}: {e}", card_path.display())))?;
        let values: Vec<Value> = serde_json::from_str(&card_text)?;
        for card in cards_from_values(&values)? {
            upsert_card(&tx, &card, &set.id)?;
            stats.cards += 1;
        }
    }
    tx.commit()?;
    Ok(stats)
}

/// Download the repo tarball and import it into the shared catalog.
///
/// `wire` lands the tarball exactly as fetched before a byte of it is
/// unpacked — or replays the one a previous run landed. The corpus is one
/// archive carrying both sets and cards, so it lands under `dataset=bulk`
/// rather than pretending to be either.
pub fn download_and_import(conn: &mut Connection, wire: &Wire) -> Result<ImportStats> {
    let http = reqwest::blocking::Client::builder()
        .user_agent("pokedumpster/0.1 (+cache-population)")
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let bytes = landing::fetch_bytes(
        &http,
        http.get(REPO_TARBALL),
        wire,
        Source::PokemonTcgData,
        Dataset::Bulk,
        PartFormat::TarGz,
    )?;

    let tmp = tempfile::tempdir()?;
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    tar::Archive::new(gz).unpack(tmp.path())?;

    // The tarball unpacks into a single top-level directory.
    let root: PathBuf = std::fs::read_dir(tmp.path())?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or_else(|| IngestError::BadResponse("tarball had no directory".into()))?;

    import_from_dir(conn, &root)
}

/// Upsert a single set into the catalog. Shared by the file importer and
/// the pokemontcg.io tail fetch (`pkdump setup`). `now` is the fetch
/// timestamp recorded in `ptcgio_fetched_at`.
pub fn upsert_set(conn: &Connection, set: &PokemonTcgSet, now: &str) -> Result<()> {
    let (logo, symbol) = match &set.images {
        Some(i) => (i.logo.clone(), i.symbol.clone()),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO sets
           (set_code, ptcgo_code, name, series, total, printed_total,
            release_date, logo_url, symbol_url, ptcgio_fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(set_code) DO UPDATE SET
           ptcgo_code        = excluded.ptcgo_code,
           name              = excluded.name,
           series            = excluded.series,
           total             = excluded.total,
           printed_total     = excluded.printed_total,
           release_date      = excluded.release_date,
           logo_url          = excluded.logo_url,
           symbol_url        = excluded.symbol_url,
           ptcgio_fetched_at = excluded.ptcgio_fetched_at",
        rusqlite::params![
            set.id,
            set.ptcgo_code,
            set.name,
            set.series,
            set.total,
            set.printed_total,
            set.release_date,
            logo,
            symbol,
            now,
        ],
    )?;
    Ok(())
}

/// Upsert a single card into the catalog. Shared by the file importer and
/// the pokemontcg.io tail fetch (`pkdump setup`). `set_code` is supplied by
/// the caller — the repo's per-set card files do not carry a `set` object.
pub fn upsert_card(conn: &Connection, card: &PokemonTcgCard, set_code: &str) -> Result<()> {
    let mut card = card.clone();
    apply_upstream_card_correction(&mut card);
    let card = &card;

    let arr_s = |v: &Option<Vec<String>>| v.as_ref().map(|x| Value::from(x.clone()).to_string());
    let arr_i = |v: &Option<Vec<i64>>| v.as_ref().map(|x| Value::from(x.clone()).to_string());
    let val = |v: &Option<Value>| v.as_ref().map(Value::to_string);
    let (small, large) = match &card.images {
        Some(i) => (i.small.clone(), i.large.clone()),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO cards
           (card_id, set_code, number, number_sortable, name, supertype,
            subtypes, hp, types, rarity, artist, flavor_text, attacks,
            abilities, weaknesses, resistances, retreat_cost, regulation_mark,
            national_pokedex_numbers, legalities, image_small, image_large,
            raw_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
         ON CONFLICT(card_id) DO UPDATE SET
           set_code                 = excluded.set_code,
           number                   = excluded.number,
           number_sortable          = excluded.number_sortable,
           name                     = excluded.name,
           supertype                = excluded.supertype,
           subtypes                 = excluded.subtypes,
           hp                       = excluded.hp,
           types                    = excluded.types,
           rarity                   = excluded.rarity,
           artist                   = excluded.artist,
           flavor_text              = excluded.flavor_text,
           attacks                  = excluded.attacks,
           abilities                = excluded.abilities,
           weaknesses               = excluded.weaknesses,
           resistances              = excluded.resistances,
           retreat_cost             = excluded.retreat_cost,
           regulation_mark          = excluded.regulation_mark,
           national_pokedex_numbers = excluded.national_pokedex_numbers,
           legalities               = excluded.legalities,
           image_small              = excluded.image_small,
           image_large              = excluded.image_large,
           raw_json                 = excluded.raw_json",
        rusqlite::params![
            card.id,
            set_code,
            card.number,
            pkdump_core::number_sortable(&card.number),
            card.name,
            card.supertype,
            arr_s(&card.subtypes),
            card.hp.as_deref().and_then(|s| s.parse::<i64>().ok()),
            arr_s(&card.types),
            card.rarity,
            card.artist,
            card.flavor_text,
            val(&card.attacks),
            val(&card.abilities),
            val(&card.weaknesses),
            val(&card.resistances),
            arr_s(&card.retreat_cost),
            card.regulation_mark,
            arr_i(&card.national_pokedex_numbers),
            val(&card.legalities),
            small,
            large,
            card.raw.to_string(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETS_FIXTURE: &str = r#"[
      {"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
       "printedTotal":165,"total":207,"ptcgoCode":"MEW",
       "releaseDate":"2023/09/22",
       "images":{"symbol":"https://x/sym.png","logo":"https://x/logo.png"}}
    ]"#;

    // The repo's per-set card files carry no `set` object — the set is the
    // filename. The fixture mirrors that, so this test would catch a card
    // struct that wrongly requires `set`.
    const CARDS_FIXTURE: &str = r#"[
      {"id":"sv3pt5-1","name":"Bulbasaur","supertype":"Pokémon",
       "subtypes":["Basic"],"hp":"70","types":["Grass"],"number":"1",
       "rarity":"Common","artist":"Narumi Sato","regulationMark":"F",
       "nationalPokedexNumbers":[1],
       "images":{"small":"https://x/1.png","large":"https://x/1_hires.png"}},
      {"id":"sv3pt5-199","name":"Charizard ex","supertype":"Pokémon",
       "subtypes":["Basic","ex"],"hp":"330","types":["Fire"],"number":"199",
       "rarity":"Special Illustration Rare","artist":"PLANETA Mochizuki",
       "images":{"small":"https://x/199.png","large":"https://x/199_hires.png"}}
    ]"#;

    fn build_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sets")).unwrap();
        std::fs::create_dir_all(dir.path().join("cards").join("en")).unwrap();
        std::fs::write(dir.path().join("sets").join("en.json"), SETS_FIXTURE).unwrap();
        std::fs::write(
            dir.path().join("cards").join("en").join("sv3pt5.json"),
            CARDS_FIXTURE,
        )
        .unwrap();
        dir
    }

    #[test]
    fn imports_sets_and_cards() {
        let repo = build_repo();
        let dbdir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dbdir.path().join("shared.sqlite")).unwrap();

        let stats = import_from_dir(&mut conn, repo.path()).unwrap();
        assert_eq!(stats.sets, 1);
        assert_eq!(stats.cards, 2);

        // Set landed with its collector-facing code.
        let ptcgo: String = conn
            .query_row(
                "SELECT ptcgo_code FROM sets WHERE set_code = 'sv3pt5'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ptcgo, "MEW");

        // hp parsed string -> integer; number_sortable computed.
        let (hp, ns): (i64, i64) = conn
            .query_row(
                "SELECT hp, number_sortable FROM cards WHERE card_id = 'sv3pt5-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(hp, 70);
        assert_eq!(ns, 1);
    }

    #[test]
    fn upsert_card_applies_upstream_number_correction() {
        // pokemontcg.io ships zsv10pt5-80 (Antique Cover Fossil) with
        // number="60", colliding with zsv10pt5-60 Escavalier in our
        // binder layout and 404ing /card/zsv10pt5/80. The override
        // registry corrects number to "80" so the row lands at slot 80
        // and number_sortable is computed from the corrected value.
        let dbdir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dbdir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, total, printed_total) \
             VALUES ('zsv10pt5', 'Black Bolt', 'Scarlet & Violet', 172, 86)",
            [],
        )
        .unwrap();

        let raw = serde_json::json!({
            "id": "zsv10pt5-80",
            "name": "Antique Cover Fossil",
            "supertype": "Trainer",
            "subtypes": ["Item"],
            "number": "60",
            "rarity": "Common",
            "images": {
                "small": "https://images.pokemontcg.io/zsv10pt5/80.png",
                "large": "https://images.pokemontcg.io/zsv10pt5/80_hires.png"
            }
        });
        let mut card: PokemonTcgCard = serde_json::from_value(raw.clone()).unwrap();
        card.raw = raw;

        upsert_card(&conn, &card, "zsv10pt5").unwrap();

        let (number, sortable): (String, i64) = conn
            .query_row(
                "SELECT number, number_sortable FROM cards WHERE card_id = 'zsv10pt5-80'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            number, "80",
            "override must correct number to the id-suffix"
        );
        assert_eq!(
            sortable, 80,
            "number_sortable must follow the corrected number"
        );

        // Sanity: a row whose id and upstream number already agree
        // passes through untouched (override only fires on registered
        // card_ids).
        let raw2 = serde_json::json!({
            "id": "zsv10pt5-60",
            "name": "Escavalier",
            "supertype": "Pokémon",
            "number": "60",
            "rarity": "Uncommon",
            "images": {"small": "https://x/60.png", "large": "https://x/60_hires.png"}
        });
        let mut card2: PokemonTcgCard = serde_json::from_value(raw2.clone()).unwrap();
        card2.raw = raw2;
        upsert_card(&conn, &card2, "zsv10pt5").unwrap();
        let number2: String = conn
            .query_row(
                "SELECT number FROM cards WHERE card_id = 'zsv10pt5-60'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(number2, "60");
    }

    #[test]
    fn apply_corrections_heals_already_ingested_rows() {
        // The row landed before the override existed (or via a path that
        // bypassed upsert_card), so it carries upstream's wrong number.
        // `pkdump data refresh` never re-upserts it — import_tail skips
        // sets already in the catalog — so the heal has to come from the
        // registry re-application pass.
        let dbdir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dbdir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, total, printed_total) \
             VALUES ('zsv10pt5', 'Black Bolt', 'Scarlet & Violet', 172, 86)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, raw_json) \
             VALUES ('zsv10pt5-80', 'zsv10pt5', '60', 60, 'Antique Cover Fossil', '{}')",
            [],
        )
        .unwrap();
        // A card with no registry entry — must be left alone.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, raw_json) \
             VALUES ('zsv10pt5-60', 'zsv10pt5', '60', 60, 'Escavalier', '{}')",
            [],
        )
        .unwrap();

        let pending = pending_corrections(&conn).unwrap();
        assert_eq!(pending.len(), 1, "only the registered card is pending");
        assert_eq!(pending[0].card_id, "zsv10pt5-80");
        assert_eq!(pending[0].current_number, "60");
        assert_eq!(pending[0].corrected_number, "80");

        let applied = apply_corrections_to_db(&conn).unwrap();
        assert_eq!(applied.len(), 1);

        let (number, sortable): (String, i64) = conn
            .query_row(
                "SELECT number, number_sortable FROM cards WHERE card_id = 'zsv10pt5-80'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(number, "80", "correction must heal the existing row");
        assert_eq!(sortable, 80, "number_sortable must be recomputed");

        // Unregistered card untouched.
        let other: String = conn
            .query_row(
                "SELECT number FROM cards WHERE card_id = 'zsv10pt5-60'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other, "60");

        // Second run is a no-op — nothing pending, nothing written.
        assert!(
            apply_corrections_to_db(&conn).unwrap().is_empty(),
            "re-applying corrections must be idempotent"
        );
    }

    #[test]
    fn apply_corrections_recomputes_stale_sortable() {
        // A half-heal (number fixed by hand, number_sortable left behind)
        // still counts as pending — the derived column is part of what the
        // correction owns.
        let dbdir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dbdir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, total, printed_total) \
             VALUES ('zsv10pt5', 'Black Bolt', 'Scarlet & Violet', 172, 86)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, raw_json) \
             VALUES ('zsv10pt5-80', 'zsv10pt5', '80', 60, 'Antique Cover Fossil', '{}')",
            [],
        )
        .unwrap();

        assert_eq!(apply_corrections_to_db(&conn).unwrap().len(), 1);
        let sortable: i64 = conn
            .query_row(
                "SELECT number_sortable FROM cards WHERE card_id = 'zsv10pt5-80'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sortable, 80);
    }

    #[test]
    fn pending_corrections_skips_cards_not_in_catalog() {
        // An empty catalog has nothing to heal — the correction still
        // applies to the card at ingest time, so this is not an error.
        let dbdir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dbdir.path().join("shared.sqlite")).unwrap();
        assert!(pending_corrections(&conn).unwrap().is_empty());
    }

    #[test]
    fn reimport_is_idempotent() {
        let repo = build_repo();
        let dbdir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dbdir.path().join("shared.sqlite")).unwrap();

        import_from_dir(&mut conn, repo.path()).unwrap();
        import_from_dir(&mut conn, repo.path()).unwrap();

        let cards: i64 = conn
            .query_row("SELECT count(*) FROM cards", [], |r| r.get(0))
            .unwrap();
        let sets: i64 = conn
            .query_row("SELECT count(*) FROM sets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cards, 2, "re-import must not duplicate cards");
        assert_eq!(sets, 1, "re-import must not duplicate sets");
    }
}
