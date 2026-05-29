//! Read access to the set catalog, with per-set collection progress.

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

/// A set with its card count and how many of its cards the user owns —
/// the shape the `/browse` set picker renders. Bundles project into the
/// same type with `kind="bundle"` and synthesized series/null totals so
/// the picker can render them through the same tile component.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SetSummary {
    pub set_code: String,
    pub ptcgo_code: Option<String>,
    pub name: String,
    pub series: String,
    #[ts(type = "number | null")]
    pub total: Option<i64>,
    #[ts(type = "number | null")]
    pub printed_total: Option<i64>,
    pub release_date: Option<String>,
    pub logo_url: Option<String>,
    pub symbol_url: Option<String>,
    /// Cards catalogued in the set.
    #[ts(type = "number")]
    pub total_cards: i64,
    /// Distinct cards in the set the user owns at least one copy of.
    #[ts(type = "number")]
    pub owned_cards: i64,
    /// Base-set cards only — `number_sortable <= printed_total`. Excludes
    /// secret rares, subset sections, and promos. `None` when the set has
    /// no `printed_total` so the UI can hide the bar gracefully (avoids
    /// rendering 0/0 = NaN%).
    #[ts(type = "number | null")]
    pub base_total_cards: Option<i64>,
    #[ts(type = "number | null")]
    pub base_owned_cards: Option<i64>,
    /// `"set"` for real catalogued sets, `"bundle"` for TTBB-style
    /// logical-set containers.
    pub kind: String,
}

/// List every set, newest first, with card and owned-card counts. Requires a
/// user connection (the owned count joins the collection).
pub fn list_sets(conn: &Connection) -> Result<Vec<SetSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.set_code, s.ptcgo_code, s.name, s.series, s.total, \
                s.printed_total, s.release_date, s.logo_url, s.symbol_url, \
                (SELECT count(*) FROM cards WHERE set_code = s.set_code), \
                (SELECT count(DISTINCT cd.card_id) FROM collection c \
                   JOIN printings p ON c.printing_id = p.printing_id \
                   JOIN cards cd ON p.card_id = cd.card_id \
                 WHERE cd.set_code = s.set_code), \
                CASE WHEN s.printed_total IS NULL THEN NULL ELSE ( \
                  SELECT count(*) FROM cards \
                   WHERE set_code = s.set_code \
                     AND number_sortable <= s.printed_total) END, \
                CASE WHEN s.printed_total IS NULL THEN NULL ELSE ( \
                  SELECT count(DISTINCT cd.card_id) FROM collection co \
                    JOIN printings p ON co.printing_id = p.printing_id \
                    JOIN cards cd ON p.card_id = cd.card_id \
                  WHERE cd.set_code = s.set_code \
                    AND cd.number_sortable <= s.printed_total) END \
         FROM sets s \
         ORDER BY s.release_date DESC NULLS LAST, s.set_code",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SetSummary {
            set_code: r.get(0)?,
            ptcgo_code: r.get(1)?,
            name: r.get(2)?,
            series: r.get(3)?,
            total: r.get(4)?,
            printed_total: r.get(5)?,
            release_date: r.get(6)?,
            logo_url: r.get(7)?,
            symbol_url: r.get(8)?,
            total_cards: r.get(9)?,
            owned_cards: r.get(10)?,
            base_total_cards: r.get(11)?,
            base_owned_cards: r.get(12)?,
            kind: "set".to_string(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One rarity tier within a set, with how many of its cards the user owns.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct RarityCount {
    pub rarity: String,
    #[ts(type = "number")]
    pub total_cards: i64,
    #[ts(type = "number")]
    pub owned_cards: i64,
}

/// Per-card copy count for the stats-page histogram. One row per
/// catalogued card in the set, in collector-number order; `copies` is
/// the total physical count across every variant.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CardCopyCount {
    pub number: String,
    #[ts(type = "number")]
    pub number_sortable: i64,
    pub rarity: Option<String>,
    #[ts(type = "number")]
    pub copies: i64,
}

/// Analytical breakdown of a single set: completion against both the
/// numbered set and the master (every printing) set, the rarity split,
/// and value — the full set's market value and the value owned.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SetAnalytics {
    pub set_code: String,
    pub name: String,
    pub series: String,
    /// Base-set cards only (number ≤ printed_total — excludes secret
    /// rares, subset sections, promos), and how many the user owns
    /// (any variant counts).
    #[ts(type = "number")]
    pub base_total_cards: i64,
    #[ts(type = "number")]
    pub base_owned_cards: i64,
    /// Numbered cards in the set, and how many the user owns.
    #[ts(type = "number")]
    pub total_cards: i64,
    #[ts(type = "number")]
    pub owned_cards: i64,
    /// Non-deprecated printings (the master set), and how many are owned.
    #[ts(type = "number")]
    pub total_printings: i64,
    #[ts(type = "number")]
    pub owned_printings: i64,
    /// Total physical copies of any card in the set — sums duplicates.
    #[ts(type = "number")]
    pub owned_copies: i64,
    /// Market value: one of every printing in the set, one of every
    /// printing the user owns, and the all-physical-copies sum (counts
    /// duplicates).
    pub market_value: f64,
    pub owned_value_unique: f64,
    pub owned_value: f64,
    pub rarities: Vec<RarityCount>,
    /// One entry per card in the set, ordered by collector number —
    /// drives the stats-page copy-count histogram.
    pub copy_counts: Vec<CardCopyCount>,
}

/// Compute the analytical breakdown for one set. `None` if no such set.
pub fn analytics(conn: &Connection, set_code: &str) -> Result<Option<SetAnalytics>> {
    let header: Option<(String, String)> = conn
        .prepare("SELECT name, series FROM sets WHERE set_code = ?1")?
        .query_row([set_code], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((name, series)) = header else {
        return Ok(None);
    };

    let total_cards: i64 = conn.query_row(
        "SELECT count(*) FROM cards WHERE set_code = ?1",
        [set_code],
        |r| r.get(0),
    )?;
    let owned_cards: i64 = conn.query_row(
        "SELECT count(DISTINCT cd.card_id) FROM collection c \
           JOIN printings p ON c.printing_id = p.printing_id \
           JOIN cards cd ON p.card_id = cd.card_id \
         WHERE cd.set_code = ?1",
        [set_code],
        |r| r.get(0),
    )?;
    // Base-set cards = number ≤ printed_total. Matches binder.rs
    // section_of's "base" bucket — excludes secret rares + subsets.
    let base_total_cards: i64 = conn.query_row(
        "SELECT count(*) FROM cards c JOIN sets s ON s.set_code = c.set_code \
         WHERE c.set_code = ?1 \
           AND s.printed_total IS NOT NULL \
           AND c.number_sortable <= s.printed_total",
        [set_code],
        |r| r.get(0),
    )?;
    let base_owned_cards: i64 = conn.query_row(
        "SELECT count(DISTINCT cd.card_id) FROM collection co \
           JOIN printings p ON co.printing_id = p.printing_id \
           JOIN cards cd ON p.card_id = cd.card_id \
           JOIN sets s ON s.set_code = cd.set_code \
         WHERE cd.set_code = ?1 \
           AND s.printed_total IS NOT NULL \
           AND cd.number_sortable <= s.printed_total",
        [set_code],
        |r| r.get(0),
    )?;
    let total_printings: i64 = conn.query_row(
        "SELECT count(*) FROM printings p JOIN cards c ON p.card_id = c.card_id \
         WHERE c.set_code = ?1 AND p.deprecated_at IS NULL",
        [set_code],
        |r| r.get(0),
    )?;
    let owned_printings: i64 = conn.query_row(
        "SELECT count(DISTINCT c.printing_id) FROM collection c \
           JOIN printings p ON c.printing_id = p.printing_id \
           JOIN cards cd ON p.card_id = cd.card_id \
         WHERE cd.set_code = ?1 AND p.deprecated_at IS NULL",
        [set_code],
        |r| r.get(0),
    )?;

    // Effective market price for a printing `p`. Gap-fill rule: latest
    // TCGplayer market when present; otherwise the latest manual_prices
    // entry — so printings without a tcgplayer_product_id (e.g. basep
    // promos) still contribute once the user records a manual value.
    let price_expr = "COALESCE( \
            (SELECT lp.price FROM latest_prices lp \
               WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
                 AND lp.sub_type_name = p.sub_type_name \
                 AND lp.price_type = 'market' \
               LIMIT 1), \
            (SELECT mp.price FROM manual_prices mp \
               WHERE mp.printing_id = p.printing_id \
               ORDER BY mp.observed_at DESC LIMIT 1) \
        )";
    let market_value: f64 = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM({price_expr}), 0) \
             FROM printings p JOIN cards c ON p.card_id = c.card_id \
             WHERE c.set_code = ?1 AND p.deprecated_at IS NULL"
        ),
        [set_code],
        |r| r.get(0),
    )?;
    let owned_value: f64 = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM({price_expr}), 0) \
             FROM collection col JOIN printings p ON col.printing_id = p.printing_id \
             JOIN cards c ON p.card_id = c.card_id \
             WHERE c.set_code = ?1"
        ),
        [set_code],
        |r| r.get(0),
    )?;
    // One-of-each owned printing value — the "completion picture" sum
    // that doesn't double-count duplicates. Drops deprecated printings
    // for parity with owned_printings.
    let owned_value_unique: f64 = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM({price_expr}), 0) \
             FROM printings p JOIN cards c ON p.card_id = c.card_id \
             WHERE c.set_code = ?1 \
               AND p.deprecated_at IS NULL \
               AND EXISTS (SELECT 1 FROM collection col WHERE col.printing_id = p.printing_id)"
        ),
        [set_code],
        |r| r.get(0),
    )?;
    // Total physical copies in the set (one row per copy in `collection`).
    let owned_copies: i64 = conn.query_row(
        "SELECT count(*) FROM collection col \
           JOIN printings p ON col.printing_id = p.printing_id \
           JOIN cards c ON p.card_id = c.card_id \
         WHERE c.set_code = ?1",
        [set_code],
        |r| r.get(0),
    )?;

    // Per-card copy counts in collector-number order. LEFT JOIN through
    // printings/collection so cards owned 0 times still produce a row
    // (copies = 0 because COUNT ignores NULLs).
    let copy_counts: Vec<CardCopyCount> = {
        let mut stmt = conn.prepare(
            "SELECT c.number, c.number_sortable, c.rarity, \
                    COUNT(col.id) AS copies \
             FROM cards c \
             LEFT JOIN printings p ON p.card_id = c.card_id \
             LEFT JOIN collection col ON col.printing_id = p.printing_id \
             WHERE c.set_code = ?1 \
             GROUP BY c.card_id \
             ORDER BY c.number_sortable",
        )?;
        let rows = stmt.query_map([set_code], |r| {
            Ok(CardCopyCount {
                number: r.get(0)?,
                number_sortable: r.get(1)?,
                rarity: r.get(2)?,
                copies: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let rarities = {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(c.rarity, 'Unknown') AS rarity, \
                    count(*) AS total_cards, \
                    count(DISTINCT owned.card_id) AS owned_cards \
             FROM cards c \
             LEFT JOIN (SELECT DISTINCT cd.card_id FROM collection col \
                          JOIN printings p ON col.printing_id = p.printing_id \
                          JOIN cards cd ON p.card_id = cd.card_id \
                        WHERE cd.set_code = ?1) owned ON owned.card_id = c.card_id \
             WHERE c.set_code = ?1 \
             GROUP BY rarity ORDER BY total_cards DESC, rarity",
        )?;
        let rows = stmt.query_map([set_code], |r| {
            Ok(RarityCount {
                rarity: r.get(0)?,
                total_cards: r.get(1)?,
                owned_cards: r.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    Ok(Some(SetAnalytics {
        set_code: set_code.to_string(),
        name,
        series,
        base_total_cards,
        base_owned_cards,
        total_cards,
        owned_cards,
        total_printings,
        owned_printings,
        owned_copies,
        market_value,
        owned_value_unique,
        owned_value,
        rarities,
        copy_counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{self, NewCopy};
    use crate::{connect_user, open_shared};

    #[test]
    fn list_sets_reports_card_and_owned_counts() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, release_date) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet', '2023/09/22')",
                [],
            )
            .unwrap();
            for n in ["1", "2"] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'sv3pt5', ?2, ?3, 'Card')",
                    rusqlite::params![format!("sv3pt5-{n}"), n, n.parse::<i64>().unwrap()],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-1-normal', 'sv3pt5-1', 'normal')",
                [],
            )
            .unwrap();
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let sets = list_sets(&conn).unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].set_code, "sv3pt5");
        assert_eq!(sets[0].total_cards, 2);
        assert_eq!(sets[0].owned_cards, 1);
        // printed_total is NULL on this fixture, so base counts are None
        // — UI should hide the base bar in that case.
        assert_eq!(sets[0].base_total_cards, None);
        assert_eq!(sets[0].base_owned_cards, None);
    }

    #[test]
    fn list_sets_reports_base_counts_when_printed_total_known() {
        // printed_total=2 → cards #1 and #2 are base; #3 is a secret rare
        // outside the base count. Owning #1 (base) and #3 (secret) yields
        // base_owned=1 / base_total=2 while owned_cards=2 / total_cards=3.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, total, printed_total, release_date) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet', 3, 2, '2023/09/22')",
                [],
            )
            .unwrap();
            for n in ["1", "2", "3"] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'sv3pt5', ?2, ?3, 'Card')",
                    rusqlite::params![format!("sv3pt5-{n}"), n, n.parse::<i64>().unwrap()],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) \
                     VALUES (?1, ?2, 'normal')",
                    rusqlite::params![format!("sv3pt5-{n}-normal"), format!("sv3pt5-{n}")],
                )
                .unwrap();
            }
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        for n in ["1", "3"] {
            collection::add(
                &mut conn,
                &NewCopy {
                    printing_id: format!("sv3pt5-{n}-normal"),
                    source: "manual_id".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let sets = list_sets(&conn).unwrap();
        assert_eq!(sets[0].total_cards, 3);
        assert_eq!(sets[0].owned_cards, 2);
        assert_eq!(sets[0].base_total_cards, Some(2));
        assert_eq!(sets[0].base_owned_cards, Some(1));
    }

    #[test]
    fn analytics_breaks_down_completion_value_and_rarity() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'SV')",
                [],
            )
            .unwrap();
            // Three cards: two Common, one Rare.
            for (n, rarity) in [("1", "Common"), ("2", "Common"), ("3", "Rare")] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                     VALUES (?1, 'sv3pt5', ?2, ?3, 'Card', ?4)",
                    rusqlite::params![format!("sv3pt5-{n}"), n, n.parse::<i64>().unwrap(), rarity],
                )
                .unwrap();
            }
            // Each card has one normal printing linked to a TCGplayer product.
            for (n, product) in [("1", 101), ("2", 102), ("3", 103)] {
                c.execute(
                    "INSERT INTO printings \
                       (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                     VALUES (?1, ?2, 'normal', ?3, 'Normal')",
                    rusqlite::params![format!("sv3pt5-{n}-normal"), format!("sv3pt5-{n}"), product],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO prices \
                       (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                     VALUES (?1, 'Normal', 'tcgplayer', 'market', ?2, '2026-05-18')",
                    rusqlite::params![product, (product - 100) as f64],
                )
                .unwrap();
            }
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        // Own card 1 ($1) twice and card 3 ($3) once.
        for pid in ["sv3pt5-1-normal", "sv3pt5-1-normal", "sv3pt5-3-normal"] {
            collection::add(
                &mut conn,
                &NewCopy {
                    printing_id: pid.into(),
                    source: "manual_id".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let a = analytics(&conn, "sv3pt5").unwrap().unwrap();
        assert_eq!(a.total_cards, 3);
        assert_eq!(a.owned_cards, 2); // cards 1 and 3
        assert_eq!(a.total_printings, 3);
        assert_eq!(a.owned_printings, 2);
        assert_eq!(a.market_value, 6.0); // 1 + 2 + 3
        // Three physical copies → owned_value sums duplicates ($1+$1+$3),
        // owned_value_unique sums one-of-each owned printing ($1+$3).
        assert_eq!(a.owned_copies, 3);
        assert_eq!(a.owned_value, 5.0);
        assert_eq!(a.owned_value_unique, 4.0);

        // Per-card copy counts in collector-number order.
        assert_eq!(a.copy_counts.len(), 3);
        assert_eq!(a.copy_counts[0].number, "1");
        assert_eq!(a.copy_counts[0].copies, 2);
        assert_eq!(a.copy_counts[1].number, "2");
        assert_eq!(a.copy_counts[1].copies, 0);
        assert_eq!(a.copy_counts[2].number, "3");
        assert_eq!(a.copy_counts[2].copies, 1);

        let common = a.rarities.iter().find(|r| r.rarity == "Common").unwrap();
        assert_eq!(common.total_cards, 2);
        assert_eq!(common.owned_cards, 1);
        let rare = a.rarities.iter().find(|r| r.rarity == "Rare").unwrap();
        assert_eq!(rare.total_cards, 1);
        assert_eq!(rare.owned_cards, 1);

        assert!(analytics(&conn, "nope").unwrap().is_none());
    }
}
