//! Read access to the catalog: a card with its printings (owned counts +
//! market price) and the user's owned copies — the card-detail payload.

use rusqlite::{Connection, OptionalExtension, params};

use crate::collection::{self, CollectionEntry};
use crate::error::Result;

const CARD_COLS: &str = "c.card_id, c.set_code, c.number, c.number_sortable, c.name, \
     c.supertype, c.subtypes, c.hp, c.types, c.rarity, c.artist, c.flavor_text, c.attacks, \
     c.abilities, c.weaknesses, c.resistances, c.retreat_cost, c.regulation_mark, \
     c.national_pokedex_numbers, c.legalities, c.image_small, c.image_large, \
     json_extract(c.raw_json, '$.evolvesFrom') AS evolves_from, \
     json_extract(c.raw_json, '$.evolvesTo')   AS evolves_to, \
     s.ptcgo_code AS set_ptcgo_code, s.symbol_url AS set_symbol_url, s.name AS set_name";

/// A catalog card. JSON-typed columns (`subtypes`, `attacks`, …) are passed
/// through as raw JSON strings for the frontend to parse.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Card {
    pub card_id: String,
    pub set_code: String,
    pub number: String,
    #[ts(type = "number")]
    pub number_sortable: i64,
    pub name: String,
    pub supertype: Option<String>,
    pub subtypes: Option<String>,
    #[ts(type = "number | null")]
    pub hp: Option<i64>,
    pub types: Option<String>,
    pub rarity: Option<String>,
    pub artist: Option<String>,
    pub flavor_text: Option<String>,
    pub attacks: Option<String>,
    pub abilities: Option<String>,
    pub weaknesses: Option<String>,
    pub resistances: Option<String>,
    pub retreat_cost: Option<String>,
    pub regulation_mark: Option<String>,
    pub national_pokedex_numbers: Option<String>,
    pub legalities: Option<String>,
    pub image_small: Option<String>,
    pub image_large: Option<String>,
    /// pokemontcg.io `evolvesFrom` — the name of the card this evolves from.
    pub evolves_from: Option<String>,
    /// pokemontcg.io `evolvesTo` — a JSON array of names this evolves to.
    pub evolves_to: Option<String>,
    /// 3-letter TCGplayer code (e.g. "MEW", "WHT") for the set; preferred
    /// over `set_code` as the human-visible label.
    pub set_ptcgo_code: Option<String>,
    /// URL of the set's symbol image (joined from `sets.symbol_url`).
    pub set_symbol_url: Option<String>,
    /// Full set name (joined from `sets.name`).
    pub set_name: String,
}

/// One printing of a card, with how many copies the user owns and the
/// latest TCGplayer market price (NULL until printing↔product linking lands).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PrintingInfo {
    pub printing_id: String,
    pub variant: String,
    pub language: String,
    pub badge_overlay: Option<String>,
    pub image_override: Option<String>,
    pub deprecated: bool,
    #[ts(type = "number")]
    pub owned_count: i64,
    pub market_price: Option<f64>,
    /// TCGplayer product id, used to deep-link a printing to its product page.
    #[ts(type = "number | null")]
    pub tcgplayer_product_id: Option<i64>,
}

/// The full card-detail payload.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CardDetail {
    pub card: Card,
    pub printings: Vec<PrintingInfo>,
    pub copies: Vec<CollectionEntry>,
}

fn card_from_row(r: &rusqlite::Row) -> rusqlite::Result<Card> {
    Ok(Card {
        card_id: r.get(0)?,
        set_code: r.get(1)?,
        number: r.get(2)?,
        number_sortable: r.get(3)?,
        name: r.get(4)?,
        supertype: r.get(5)?,
        subtypes: r.get(6)?,
        hp: r.get(7)?,
        types: r.get(8)?,
        rarity: r.get(9)?,
        artist: r.get(10)?,
        flavor_text: r.get(11)?,
        attacks: r.get(12)?,
        abilities: r.get(13)?,
        weaknesses: r.get(14)?,
        resistances: r.get(15)?,
        retreat_cost: r.get(16)?,
        regulation_mark: r.get(17)?,
        national_pokedex_numbers: r.get(18)?,
        legalities: r.get(19)?,
        image_small: r.get(20)?,
        image_large: r.get(21)?,
        evolves_from: r.get(22)?,
        evolves_to: r.get(23)?,
        set_ptcgo_code: r.get(24)?,
        set_symbol_url: r.get(25)?,
        set_name: r.get(26)?,
    })
}

/// Fetch the card-detail payload for a card identified by set code and
/// collector number. Returns `None` if no such card is in the catalog.
pub fn get_card_detail(
    conn: &Connection,
    set_code: &str,
    number: &str,
) -> Result<Option<CardDetail>> {
    let card: Option<Card> = conn
        .prepare(&format!(
            "SELECT {CARD_COLS} FROM cards c \
             JOIN sets s ON s.set_code = c.set_code \
             WHERE c.set_code = ?1 AND c.number = ?2"
        ))?
        .query_row(params![set_code, number], card_from_row)
        .optional()?;
    let Some(card) = card else {
        return Ok(None);
    };

    let printings: Vec<PrintingInfo> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT p.printing_id, p.variant, p.language, p.badge_overlay, \
                    p.image_override, p.deprecated_at, \
                    (SELECT count(*) FROM collection c \
                       WHERE c.printing_id = p.printing_id), \
                    (SELECT lp.price FROM latest_prices lp \
                       WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
                         AND lp.price_type = 'market' \
                         AND lp.sub_type_name = ({subtype}) LIMIT 1), \
                    p.tcgplayer_product_id \
             FROM printings p WHERE p.card_id = ?1 ORDER BY p.variant",
            subtype = crate::VARIANT_PRICE_SUBTYPE,
        ))?;
        let rows = stmt.query_map([&card.card_id], |r| {
            Ok(PrintingInfo {
                printing_id: r.get(0)?,
                variant: r.get(1)?,
                language: r.get(2)?,
                badge_overlay: r.get(3)?,
                image_override: r.get(4)?,
                deprecated: r.get::<_, Option<String>>(5)?.is_some(),
                owned_count: r.get(6)?,
                market_price: r.get(7)?,
                tcgplayer_product_id: r.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let copies = collection::list_for_card(conn, &card.card_id)?;
    Ok(Some(CardDetail {
        card,
        printings,
        copies,
    }))
}

/// One snapshot in a printing's market-price history.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PricePoint {
    /// `YYYY-MM-DD` — the day TCGCSV was queried (see `import_prices`).
    pub date: String,
    pub price: f64,
}

/// Full price history for one printing × price_type. The frontend renders
/// one chart line per series. v1 only emits `price_type = "market"`.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PriceSeries {
    pub printing_id: String,
    pub variant: String,
    /// TCGplayer sub_type — `Normal` / `Holofoil` / `Reverse Holofoil` / …
    pub sub_type_name: String,
    pub price_type: String,
    pub points: Vec<PricePoint>,
}

/// All market-price snapshots for every printing of a card, ordered oldest
/// → newest within each series. Printings without a TCGplayer link or with
/// a variant that has no sub_type mapping yield no series.
pub fn get_card_prices(
    conn: &Connection,
    set_code: &str,
    number: &str,
) -> Result<Vec<PriceSeries>> {
    let card_id: Option<String> = conn
        .prepare("SELECT card_id FROM cards WHERE set_code = ?1 AND number = ?2")?
        .query_row(params![set_code, number], |r| r.get(0))
        .optional()?;
    let Some(card_id) = card_id else {
        return Ok(Vec::new());
    };

    let sub_expr = crate::VARIANT_PRICE_SUBTYPE;
    let mut stmt = conn.prepare(&format!(
        "SELECT p.printing_id, p.variant, ({sub_expr}) AS sub_type, \
                pr.price_type, pr.observed_at, pr.price \
           FROM printings p \
           JOIN prices pr ON pr.tcgplayer_product_id = p.tcgplayer_product_id \
                         AND pr.sub_type_name = ({sub_expr}) \
                         AND pr.source = 'tcgplayer' \
                         AND pr.price_type = 'market' \
          WHERE p.card_id = ?1 \
          ORDER BY p.variant, pr.observed_at",
    ))?;

    let mut series: Vec<PriceSeries> = Vec::new();
    let mut rows = stmt.query([&card_id])?;
    while let Some(r) = rows.next()? {
        let printing_id: String = r.get(0)?;
        let variant: String = r.get(1)?;
        let sub: String = r.get(2)?;
        let price_type: String = r.get(3)?;
        let date: String = r.get(4)?;
        let price: f64 = r.get(5)?;

        let point = PricePoint { date, price };
        let key = (&printing_id, &price_type);
        match series
            .last_mut()
            .filter(|s| s.printing_id == *key.0 && s.price_type == *key.1)
        {
            Some(s) => s.points.push(point),
            None => series.push(PriceSeries {
                printing_id,
                variant,
                sub_type_name: sub,
                price_type,
                points: vec![point],
            }),
        }
    }
    Ok(series)
}

/// One row in the global catalog-search results.
///
/// Mirrors the columns the collection table renders so the frontend can
/// fold catalog matches into the same grid/table view as owned copies —
/// `owned_count` distinguishes unowned tiles for the dim treatment.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CatalogSearchRow {
    pub card_id: String,
    pub set_code: String,
    pub set_ptcgo_code: Option<String>,
    pub set_name: String,
    pub set_symbol_url: Option<String>,
    pub number: String,
    pub name: String,
    pub rarity: Option<String>,
    pub supertype: Option<String>,
    pub subtypes: Option<String>,
    pub types: Option<String>,
    pub attacks: Option<String>,
    pub image_small: Option<String>,
    /// Sum of copies the user owns across every printing of this card.
    #[ts(type = "number")]
    pub owned_count: i64,
}

/// Catalog-wide name search. Used by the collection page's "All cards"
/// toggle so the user can find cards they don't own and add them. Returns
/// up to `limit` rows ranked by match quality (exact name → starts-with →
/// substring), then newer sets first within each tier. `q` must be
/// non-empty.
pub fn catalog_search(conn: &Connection, q: &str, limit: usize) -> Result<Vec<CatalogSearchRow>> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let like = format!("%{q}%");
    let starts = format!("{q}%");
    let mut stmt = conn.prepare(
        "SELECT c.card_id, c.set_code, s.ptcgo_code, s.name AS set_name, \
                s.symbol_url, c.number, c.name, c.rarity, c.supertype, \
                c.subtypes, c.types, c.attacks, c.image_small, \
                (SELECT COALESCE(SUM(1), 0) FROM collection co \
                   JOIN printings p ON p.printing_id = co.printing_id \
                  WHERE p.card_id = c.card_id) AS owned_count \
           FROM cards c \
           JOIN sets s ON s.set_code = c.set_code \
          WHERE c.name LIKE ?1 COLLATE NOCASE \
          ORDER BY \
            CASE WHEN c.name = ?2 COLLATE NOCASE THEN 0 \
                 WHEN c.name LIKE ?3 COLLATE NOCASE THEN 1 \
                 ELSE 2 END, \
            s.release_date DESC NULLS LAST, \
            c.set_code, c.number_sortable \
          LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![like, q, starts, limit as i64], |r| {
        Ok(CatalogSearchRow {
            card_id: r.get(0)?,
            set_code: r.get(1)?,
            set_ptcgo_code: r.get(2)?,
            set_name: r.get(3)?,
            set_symbol_url: r.get(4)?,
            number: r.get(5)?,
            name: r.get(6)?,
            rarity: r.get(7)?,
            supertype: r.get(8)?,
            subtypes: r.get(9)?,
            types: r.get(10)?,
            attacks: r.get(11)?,
            image_small: r.get(12)?,
            owned_count: r.get(13)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Find a card by name — return the (set_code, number) of the most
/// recently-released printing. Used by the modal's evolution links so a
/// name like "Pikachu" resolves to a clickable card.
pub fn find_first_by_name(conn: &Connection, name: &str) -> Result<Option<(String, String)>> {
    Ok(conn
        .prepare(
            "SELECT cd.set_code, cd.number FROM cards cd \
               JOIN sets s ON cd.set_code = s.set_code \
             WHERE cd.name = ?1 COLLATE NOCASE \
             ORDER BY s.release_date DESC NULLS LAST, cd.set_code, cd.number_sortable \
             LIMIT 1",
        )?
        .query_row([name], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::NewCopy;
    use crate::{connect_user, open_shared};

    fn user_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur', 'Common')",
                [],
            )
            .unwrap();
            for variant in ["normal", "reverse_holo"] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) \
                     VALUES (?1, 'sv3pt5-1', ?2)",
                    params![format!("sv3pt5-1-{variant}"), variant],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn card_detail_includes_printings_and_copies() {
        let (_d, mut conn) = user_conn();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let detail = get_card_detail(&conn, "sv3pt5", "1").unwrap().unwrap();
        assert_eq!(detail.card.name, "Bulbasaur");
        assert_eq!(detail.printings.len(), 2);
        assert_eq!(detail.copies.len(), 1);

        let normal = detail
            .printings
            .iter()
            .find(|p| p.variant == "normal")
            .unwrap();
        assert_eq!(normal.owned_count, 1);
        let rh = detail
            .printings
            .iter()
            .find(|p| p.variant == "reverse_holo")
            .unwrap();
        assert_eq!(rh.owned_count, 0);
    }

    #[test]
    fn missing_card_returns_none() {
        let (_d, conn) = user_conn();
        assert!(get_card_detail(&conn, "sv3pt5", "999").unwrap().is_none());
    }

    #[test]
    fn card_detail_resolves_per_variant_market_price() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
                [],
            )
            .unwrap();
            // Printings linked to TCGplayer product 5006.
            for v in ["normal", "reverse_holo"] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
                     VALUES (?1, 'sv3pt5-6', ?2, 5006)",
                    rusqlite::params![format!("sv3pt5-6-{v}"), v],
                )
                .unwrap();
            }
            // Distinct market prices per sub-type.
            for (sub, price) in [("Normal", 10.0_f64), ("Reverse Holofoil", 25.0)] {
                c.execute(
                    "INSERT INTO prices \
                       (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                     VALUES (5006, ?1, 'tcgplayer', 'market', ?2, '2026-05-18')",
                    rusqlite::params![sub, price],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let detail = get_card_detail(&conn, "sv3pt5", "6").unwrap().unwrap();

        let normal = detail
            .printings
            .iter()
            .find(|p| p.variant == "normal")
            .unwrap();
        let rh = detail
            .printings
            .iter()
            .find(|p| p.variant == "reverse_holo")
            .unwrap();
        assert_eq!(normal.market_price, Some(10.0));
        assert_eq!(rh.market_price, Some(25.0));
    }

    #[test]
    fn get_card_prices_returns_ordered_series_per_printing() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
                [],
            )
            .unwrap();
            for v in ["normal", "reverse_holo"] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
                     VALUES (?1, 'sv3pt5-6', ?2, 5006)",
                    params![format!("sv3pt5-6-{v}"), v],
                )
                .unwrap();
            }
            // Two days, two sub-types, plus a non-market row that must be filtered out.
            let rows = [
                ("Normal", "market", 9.5_f64, "2026-05-21"),
                ("Normal", "market", 10.0, "2026-05-22"),
                ("Reverse Holofoil", "market", 24.0, "2026-05-21"),
                ("Reverse Holofoil", "market", 25.0, "2026-05-22"),
                ("Normal", "low", 7.0, "2026-05-22"),
            ];
            for (sub, pt, price, day) in rows {
                c.execute(
                    "INSERT INTO prices \
                       (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                     VALUES (5006, ?1, 'tcgplayer', ?2, ?3, ?4)",
                    params![sub, pt, price, day],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let series = get_card_prices(&conn, "sv3pt5", "6").unwrap();
        assert_eq!(series.len(), 2, "one series per printing");
        let normal = series.iter().find(|s| s.variant == "normal").unwrap();
        assert_eq!(normal.points.len(), 2);
        assert_eq!(normal.points[0].date, "2026-05-21");
        assert_eq!(normal.points[0].price, 9.5);
        assert_eq!(normal.points[1].price, 10.0);
        assert!(series.iter().all(|s| s.price_type == "market"));
    }

    #[test]
    fn get_card_prices_returns_empty_for_unknown_card() {
        let (_d, conn) = user_conn();
        assert!(get_card_prices(&conn, "sv3pt5", "999").unwrap().is_empty());
    }

    #[test]
    fn catalog_search_ranks_exact_then_starts_then_contains() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            // Two sets, different release dates so the secondary sort is
            // observable (newer first within a match tier).
            c.execute(
                "INSERT INTO sets (set_code, name, series, release_date) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet', '2023/09/22'), \
                        ('sv8pt5', 'Prismatic Evolutions', 'Scarlet & Violet', '2025/01/17')",
                [],
            )
            .unwrap();
            // Insert four cards. Exact match wins regardless of release; the
            // starts-with row comes before the substring row.
            let cards = [
                ("sv3pt5-1", "sv3pt5", "1", "Bulbasaur"),
                ("sv8pt5-1", "sv8pt5", "1", "Bulbasaur"), // exact, newer
                ("sv3pt5-2", "sv3pt5", "2", "Bulbasaur ex"), // starts-with
                ("sv3pt5-7", "sv3pt5", "7", "Tangela"),   // no match
            ];
            for (id, set, num, name) in cards {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, set, num, num.parse::<i64>().unwrap_or(0), name],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let rows = catalog_search(&conn, "bulbasaur", 10).unwrap();
        assert_eq!(rows.len(), 3, "Tangela is not a match");
        // Exact-match tier: newer set comes first.
        assert_eq!(rows[0].card_id, "sv8pt5-1");
        assert_eq!(rows[1].card_id, "sv3pt5-1");
        // Then the starts-with row.
        assert_eq!(rows[2].card_id, "sv3pt5-2");
    }

    #[test]
    fn catalog_search_counts_owned_copies_across_all_printings() {
        let (_d, mut conn) = user_conn();
        // Catalog has Bulbasaur with both printings (set up by user_conn).
        // Add one copy of each printing.
        for variant in ["normal", "reverse_holo"] {
            collection::add(
                &mut conn,
                &NewCopy {
                    printing_id: format!("sv3pt5-1-{variant}"),
                    source: "test".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let rows = catalog_search(&conn, "bulb", 10).unwrap();
        let bulb = rows.iter().find(|r| r.card_id == "sv3pt5-1").unwrap();
        assert_eq!(bulb.owned_count, 2);
    }

    #[test]
    fn catalog_search_empty_query_returns_empty() {
        let (_d, conn) = user_conn();
        assert!(catalog_search(&conn, "", 10).unwrap().is_empty());
        assert!(catalog_search(&conn, "   ", 10).unwrap().is_empty());
    }
}
