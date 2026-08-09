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
    /// True when this row is a user_printings entry — the "Missing
    /// Variant" escape hatch. Renders with an italic + (user) tag in
    /// the UI.
    pub is_user_added: bool,
    /// Free-text variant description carried by user_printings rows.
    pub description: Option<String>,
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
        // UNION ALL of shared.printings and user_printings so the
        // missing-variant escape hatch (decision pokedumpster-x7k)
        // shows up alongside catalog rows on /card and CardModal. The
        // binder browse query (binder.rs) deliberately does NOT do
        // this — VariantModal stays catalog-only.
        let mut stmt = conn.prepare(
            "SELECT p.printing_id, p.variant, p.language, p.badge_overlay, \
                    p.image_override, p.deprecated_at, \
                    (SELECT count(*) FROM collection c \
                       WHERE c.printing_id = p.printing_id) AS owned_count, \
                    COALESCE( \
                       (SELECT lp.price FROM latest_prices lp \
                          WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
                            AND lp.sub_type_name = p.sub_type_name \
                            AND lp.price_type = 'market' \
                          LIMIT 1), \
                       (SELECT mp.price FROM manual_prices mp \
                          WHERE mp.printing_id = p.printing_id \
                          ORDER BY mp.observed_at DESC LIMIT 1) \
                    ) AS market_price, \
                    p.tcgplayer_product_id, \
                    0 AS is_user_added, \
                    NULL AS description \
             FROM printings p WHERE p.card_id = ?1 \
             UNION ALL \
             SELECT up.printing_id, up.variant, 'en' AS language, \
                    NULL AS badge_overlay, NULL AS image_override, \
                    NULL AS deprecated_at, \
                    (SELECT count(*) FROM collection c \
                       WHERE c.printing_id = up.printing_id) AS owned_count, \
                    (SELECT mp.price FROM manual_prices mp \
                       WHERE mp.printing_id = up.printing_id \
                       ORDER BY mp.observed_at DESC LIMIT 1) AS market_price, \
                    NULL AS tcgplayer_product_id, \
                    1 AS is_user_added, \
                    up.description \
             FROM user_printings up WHERE up.card_id = ?1 \
             ORDER BY is_user_added, variant, printing_id",
        )?;
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
                is_user_added: r.get::<_, i64>(9)? != 0,
                description: r.get(10)?,
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

/// All price snapshots for every printing of a card, ordered oldest →
/// newest within each series.
///
/// Emits two `price_type`s:
/// - `"market"` — TCGplayer market price from the shared `prices` table.
///   Printings with no `tcgplayer_product_id` (or no matching `prices`
///   row) yield no market series.
/// - `"manual"` — user-entered prices from the user-DB `manual_prices`
///   table. Surfaced so they're visible on the chart alongside TCGplayer.
///
/// For "the effective current value of this printing" the rule is gap-fill:
/// take the latest `market` point if one exists, otherwise the latest
/// `manual` point. The frontend renders all series and applies that rule
/// when computing summary stats.
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

    let mut stmt = conn.prepare(
        "SELECT p.printing_id, p.variant, p.sub_type_name, \
                'market' AS price_type, pr.observed_at AS observed_at, pr.price \
           FROM printings p \
           JOIN prices pr ON pr.tcgplayer_product_id = p.tcgplayer_product_id \
                         AND pr.sub_type_name = p.sub_type_name \
                         AND pr.source = 'tcgplayer' \
                         AND pr.price_type = 'market' \
          WHERE p.card_id = ?1 \
         UNION ALL \
         SELECT p.printing_id, p.variant, p.sub_type_name, \
                'manual' AS price_type, mp.observed_at AS observed_at, mp.price \
           FROM printings p \
           JOIN manual_prices mp ON mp.printing_id = p.printing_id \
          WHERE p.card_id = ?1 \
         UNION ALL \
         SELECT up.printing_id, up.variant, NULL AS sub_type_name, \
                'manual' AS price_type, mp.observed_at AS observed_at, mp.price \
           FROM user_printings up \
           JOIN manual_prices mp ON mp.printing_id = up.printing_id \
          WHERE up.card_id = ?1 \
          ORDER BY printing_id, price_type, observed_at",
    )?;

    let mut series: Vec<PriceSeries> = Vec::new();
    let mut rows = stmt.query([&card_id])?;
    while let Some(r) = rows.next()? {
        let printing_id: String = r.get(0)?;
        let variant: String = r.get(1)?;
        let sub: Option<String> = r.get(2)?;
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
                sub_type_name: sub.unwrap_or_default(),
                price_type,
                points: vec![point],
            }),
        }
    }
    Ok(series)
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
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity, \
                     artist, attacks) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur', 'Common', 'Narumi Sato', \
                     '[{\"name\":\"Vine Whip\",\"damage\":\"20\",\"cost\":[\"Grass\",\"Colorless\"], \
                        \"convertedEnergyCost\":2,\"text\":\"Nothing happens.\"}]')",
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

    // The other half of pd-lk8v. The search list stopped shipping `attacks`
    // and `artist` because it never drew them; the card modal (CardDetailView,
    // fed by this payload) is what does draw them, one card at a time. If this
    // ever thinned out to match the list, the Attacks section and the artist
    // facet link would silently go blank.
    #[test]
    fn card_detail_carries_the_full_attack_the_modal_renders() {
        let (_d, conn) = user_conn();
        let detail = get_card_detail(&conn, "sv3pt5", "1").unwrap().unwrap();

        let attacks = detail.card.attacks.as_deref().expect("attacks survive");
        for part in ["Vine Whip", "Grass", "Colorless", "20", "Nothing happens."] {
            assert!(
                attacks.contains(part),
                "the modal renders {part}; the detail payload must carry it"
            );
        }
        assert_eq!(detail.card.artist.as_deref(), Some("Narumi Sato"));
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
            // Printings linked to TCGplayer product 5006; each carries
            // the sub_type_name expansion would have set.
            for (v, sub) in [("normal", "Normal"), ("reverse_holo", "Reverse Holofoil")] {
                c.execute(
                    "INSERT INTO printings \
                       (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                     VALUES (?1, 'sv3pt5-6', ?2, 5006, ?3)",
                    rusqlite::params![format!("sv3pt5-6-{v}"), v, sub],
                )
                .unwrap();
            }
            for (sub, price) in [("Normal", 10.0_f64), ("Reverse Holofoil", 25.0)] {
                c.execute(
                    "INSERT INTO prices \
                       (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                     VALUES (5006, ?1, 'tcgplayer', 'market', ?2, '2026-05-18')",
                    rusqlite::params![sub, price],
                )
                .unwrap();
            }
            crate::latest_prices::refresh_latest_prices(&c).unwrap();
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
    fn card_detail_gap_fills_market_price_from_manual_when_no_tcgplayer() {
        // basep motivating case: printing has no tcgplayer_product_id,
        // so latest_prices yields NULL. CardDetail.market_price should
        // fall back to the most recent manual_prices entry.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('basep', 'Promos', 'Base')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('basep-10', 'basep', '10', 10, 'Meowth')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('basep-10-normal', 'basep-10', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        // No manual price yet → market_price is NULL.
        let detail = get_card_detail(&conn, "basep", "10").unwrap().unwrap();
        assert_eq!(detail.printings[0].market_price, None);

        // Add two manual entries; newest one wins.
        crate::manual_prices::insert(
            &conn,
            &crate::manual_prices::NewManualPrice {
                printing_id: "basep-10-normal".into(),
                price: 8.0,
                observed_at: Some("2024-01-01T00:00:00Z".into()),
                note: None,
            },
        )
        .unwrap();
        crate::manual_prices::insert(
            &conn,
            &crate::manual_prices::NewManualPrice {
                printing_id: "basep-10-normal".into(),
                price: 12.50,
                observed_at: Some("2026-05-01T00:00:00Z".into()),
                note: None,
            },
        )
        .unwrap();
        let detail = get_card_detail(&conn, "basep", "10").unwrap().unwrap();
        assert_eq!(detail.printings[0].market_price, Some(12.50));
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
            for (v, sub) in [("normal", "Normal"), ("reverse_holo", "Reverse Holofoil")] {
                c.execute(
                    "INSERT INTO printings \
                       (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                     VALUES (?1, 'sv3pt5-6', ?2, 5006, ?3)",
                    params![format!("sv3pt5-6-{v}"), v, sub],
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
    fn get_card_prices_emits_manual_series_even_without_tcgplayer_link() {
        // Mirrors the basep motivating case: a printing with no
        // tcgplayer_product_id but a user-entered manual price still
        // shows up in the chart payload.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('basep', 'Promos', 'Base')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('basep-14', 'basep', '14', 14, 'Mewtwo')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('basep-14-normal', 'basep-14', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        crate::manual_prices::insert(
            &conn,
            &crate::manual_prices::NewManualPrice {
                printing_id: "basep-14-normal".into(),
                price: 175.0,
                observed_at: Some("2024-03-01T00:00:00Z".into()),
                note: None,
            },
        )
        .unwrap();
        crate::manual_prices::insert(
            &conn,
            &crate::manual_prices::NewManualPrice {
                printing_id: "basep-14-normal".into(),
                price: 200.0,
                observed_at: Some("2024-06-01T00:00:00Z".into()),
                note: None,
            },
        )
        .unwrap();

        let series = get_card_prices(&conn, "basep", "14").unwrap();
        assert_eq!(series.len(), 1, "one manual series, no market");
        let s = &series[0];
        assert_eq!(s.price_type, "manual");
        assert_eq!(s.printing_id, "basep-14-normal");
        assert_eq!(s.points.len(), 2);
        // Oldest first within the series.
        assert_eq!(s.points[0].price, 175.0);
        assert_eq!(s.points[1].price, 200.0);
    }

    #[test]
    fn get_card_prices_emits_both_market_and_manual_series_for_same_printing() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings \
                   (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                 VALUES ('sv3pt5-6-normal', 'sv3pt5-6', 'normal', 5006, 'Normal')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO prices \
                   (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                 VALUES (5006, 'Normal', 'tcgplayer', 'market', 10.0, '2026-05-22')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        crate::manual_prices::insert(
            &conn,
            &crate::manual_prices::NewManualPrice {
                printing_id: "sv3pt5-6-normal".into(),
                price: 12.5,
                observed_at: Some("2026-05-25T00:00:00Z".into()),
                note: Some("eBay sold".into()),
            },
        )
        .unwrap();

        let series = get_card_prices(&conn, "sv3pt5", "6").unwrap();
        assert_eq!(series.len(), 2);
        let market = series.iter().find(|s| s.price_type == "market").unwrap();
        let manual = series.iter().find(|s| s.price_type == "manual").unwrap();
        assert_eq!(market.points.len(), 1);
        assert_eq!(manual.points.len(), 1);
        assert_eq!(market.points[0].price, 10.0);
        assert_eq!(manual.points[0].price, 12.5);
    }
}
