//! Importer for the `PokemonTCG/pokemon-tcg-data` GitHub repository.
//!
//! This repo is the primary bulk catalog source (RESEARCH.md §2): one JSON
//! file per set under `cards/en/<setid>.json`, plus `sets/en.json`. It uses
//! the same schema as the pokemontcg.io API minus the price blocks. The
//! pokemontcg.io client fills the 2–3 month tail of newest sets the repo
//! lags on.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use crate::error::{IngestError, Result};
use crate::pokemontcg::{PokemonTcgCard, PokemonTcgSet, cards_from_values};

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
            upsert_card(&tx, &card)?;
            stats.cards += 1;
        }
    }
    tx.commit()?;
    Ok(stats)
}

/// Download the repo tarball and import it into the shared catalog.
pub fn download_and_import(conn: &mut Connection) -> Result<ImportStats> {
    let bytes = reqwest::blocking::Client::builder()
        .user_agent("pokedumpster/0.1 (+cache-population)")
        .timeout(std::time::Duration::from_secs(120))
        .build()?
        .get(REPO_TARBALL)
        .send()?
        .error_for_status()?
        .bytes()?;

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
/// the pokemontcg.io tail fetch (`pkdump setup`).
pub fn upsert_card(conn: &Connection, card: &PokemonTcgCard) -> Result<()> {
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
            card.set.id,
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

    const CARDS_FIXTURE: &str = r#"[
      {"id":"sv3pt5-1","name":"Bulbasaur","supertype":"Pokémon",
       "subtypes":["Basic"],"hp":"70","types":["Grass"],"number":"1",
       "rarity":"Common","artist":"Narumi Sato","regulationMark":"F",
       "nationalPokedexNumbers":[1],
       "images":{"small":"https://x/1.png","large":"https://x/1_hires.png"},
       "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"}},
      {"id":"sv3pt5-199","name":"Charizard ex","supertype":"Pokémon",
       "subtypes":["Basic","ex"],"hp":"330","types":["Fire"],"number":"199",
       "rarity":"Special Illustration Rare","artist":"PLANETA Mochizuki",
       "images":{"small":"https://x/199.png","large":"https://x/199_hires.png"},
       "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"}}
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
