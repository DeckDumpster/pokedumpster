//! Read access to the set catalog, with per-set collection progress.

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;
use crate::search_meta::RarityLookup;

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
    /// The set was built locally from TCGCSV — either a bridge entry or
    /// TCGCSV set discovery (pd-558b1e4f) — because pokemontcg.io hasn't
    /// published it yet. Its cards, art and totals are provisional. Goes
    /// false on its own the refresh after upstream lands the real set.
    ///
    /// **A set upstream does not carry at all is not synthesized in this
    /// sense** (pd-mt57). The Japanese catalog is TCGCSV-native forever —
    /// pokemontcg.io has no Japanese data — so its rows have nothing to
    /// wait for and this stays false for them. `sets.ptcgio_covered` is
    /// what tells the two apart; "provisional" is a promise, and a badge
    /// that makes one has to be able to keep it.
    pub synthesized: bool,
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
                    AND cd.number_sortable <= s.printed_total) END, \
                s.ptcgio_fetched_at IS NULL AND s.ptcgio_covered = 1 \
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
            synthesized: r.get(13)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One rarity tier within a set, with how many of its cards the user owns.
///
/// `rarity` is the canonical spelling from the `rarities` table, and
/// `rank`/`grp` come from the same row: the tier's curated ordinal and its
/// group alias. Both ride along so the stats page can order and colour the
/// split without re-deriving a rarity typology in TypeScript — the catalog
/// already owns one.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct RarityCount {
    pub rarity: String,
    /// Curated ordinal from `rarities.rank`; [`UNRANKED_RARITY`] for a
    /// tier the table does not carry. Rows arrive pre-sorted by it.
    #[ts(type = "number")]
    pub rank: i64,
    /// Group alias (`common`, `ultra`, `secret`…), or `null` when the
    /// tier is unranked or the row declares no group.
    pub grp: Option<String>,
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
    /// Canonical spelling from the `rarities` table where the tier is
    /// known, else the raw catalog string.
    pub rarity: Option<String>,
    /// Group alias for the tier — the histogram paints its columns by it.
    pub rarity_grp: Option<String>,
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

/// Fold raw `(rarity, total, owned)` tallies onto their canonical tiers
/// and return them in the catalog's curated order.
///
/// Sorting here rather than in the client is the point: the ordinal lives
/// in `data/rarities.json` and nowhere else, so the stats table renders
/// the rows in the order it receives them.
pub(crate) fn rank_rarities(
    tiers: &RarityLookup,
    raw_counts: Vec<(String, i64, i64)>,
) -> Vec<RarityCount> {
    let mut folded: std::collections::HashMap<String, RarityCount> =
        std::collections::HashMap::new();
    for (raw, total, owned) in raw_counts {
        let name = tiers.display(&raw);
        let entry = folded.entry(name.clone()).or_insert_with(|| RarityCount {
            rarity: name,
            rank: tiers.rank(&raw),
            grp: tiers.grp(&raw),
            total_cards: 0,
            owned_cards: 0,
        });
        entry.total_cards += total;
        entry.owned_cards += owned;
    }
    let mut out: Vec<RarityCount> = folded.into_values().collect();
    out.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.rarity.cmp(&b.rarity)));
    out
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

    // Effective market price for a printing `p` — one rule, defined once in
    // `crate::prices`. Everything this query sees is a *catalog* printing, so
    // the gap-fill it relies on is the curated `catalog_price_overrides` row
    // (e.g. the basep promos TCGplayer does not price), entirely inside
    // `shared`. A tenant's own manual price cannot reach it (pd-m4gw).
    let price_expr = crate::prices::MARKET_PRICE_EXPR;
    // "Full set" market value = the minimum cost to own one of every card,
    // i.e. the CHEAPEST printing per card summed across cards — NOT the sum
    // of every printing. WOTC sets print each card in three runs (1st
    // Edition, Shadowless, Unlimited), and modern cards come in normal +
    // reverse holo; summing all printings triple-counts each card and is
    // dominated by the priciest run (a near-complete Base Set read as ~$28k
    // off the 1st-Edition holos). Taking the per-card minimum collapses to
    // the Unlimited run for WOTC and the base printing for modern sets —
    // the run a set collector actually completes. (pokedumpster)
    let market_value: f64 = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(card_min), 0) FROM ( \
               SELECT MIN({price_expr}) AS card_min \
               FROM printings p JOIN cards c ON p.card_id = c.card_id \
               WHERE c.set_code = ?1 AND p.deprecated_at IS NULL \
               GROUP BY p.card_id \
             )"
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
    // The rarity typology — canonical spelling, curated rank, group
    // alias — comes from the shared catalog's `rarities` table, not from
    // whatever string the upstream row happened to carry.
    let tiers = RarityLookup::load(conn)?;

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
            let raw: Option<String> = r.get(2)?;
            Ok(CardCopyCount {
                number: r.get(0)?,
                number_sortable: r.get(1)?,
                rarity: raw.as_deref().map(|s| tiers.display(s)),
                rarity_grp: raw.as_deref().and_then(|s| tiers.grp(s)),
                copies: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    // Grouped on the RAW string in SQL, then folded onto the canonical
    // spelling in Rust: a set carrying both "MEGA_ATTACK_RARE" and
    // "Mega Attack Rare" is one tier in the split, not two.
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
             GROUP BY rarity",
        )?;
        let rows = stmt.query_map([set_code], |r| {
            let raw: String = r.get(0)?;
            let total: i64 = r.get(1)?;
            let owned: i64 = r.get(2)?;
            Ok((raw, total, owned))
        })?;
        rank_rarities(&tiers, rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
    fn only_a_set_upstream_is_behind_on_reports_as_synthesized() {
        // pd-mt57. Three rows that all have a NULL `ptcgio_fetched_at` or
        // don't, crossed with whether pokemontcg.io carries the catalog at
        // all. `synthesized` drives a "provisional, upstream will replace
        // this" badge, so it may only be true where upstream can.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            // Awaiting upstream: locally built, and pokemontcg.io does
            // publish this catalog.
            c.execute(
                "INSERT INTO sets (set_code, name, series, release_date, ptcgio_covered) \
                 VALUES ('mep', 'ME Black Star Promos', 'Mega Evolution', '2025/09/26', 1)",
                [],
            )
            .unwrap();
            // TCGCSV-native: pokemontcg.io has no Japanese catalog, so
            // there is nothing for this row to be provisional about.
            c.execute(
                "INSERT INTO sets (set_code, name, series, release_date, ptcgio_covered) \
                 VALUES ('jp-24711', 'M5: Abyss Eye', 'Pokémon JP — Mega Evolution Era', \
                         '2026/05/22', 0)",
                [],
            )
            .unwrap();
            // Upstream-managed.
            c.execute(
                "INSERT INTO sets \
                   (set_code, name, series, release_date, ptcgio_fetched_at, ptcgio_covered) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet', '2023/09/22', '2026-07-31', 1)",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let by_code: std::collections::HashMap<String, bool> = list_sets(&conn)
            .unwrap()
            .into_iter()
            .map(|s| (s.set_code, s.synthesized))
            .collect();
        assert!(by_code["mep"], "upstream is behind — provisional");
        assert!(!by_code["jp-24711"], "upstream never carries it");
        assert!(!by_code["sv3pt5"], "upstream published it");
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
            let mut c = open_shared(&shared).unwrap();
            // The rarity split reads rank/grp off the `rarities` table;
            // the server reconciles it at startup, so a test that wants a
            // realistic split has to seed it too.
            crate::search_meta::reconcile(&mut c).unwrap();
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
            crate::latest_prices::refresh_latest_prices(&c).unwrap();
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

        // The split carries the catalog's typology so the stats page can
        // order and colour it without a rarity map of its own.
        assert_eq!(common.grp.as_deref(), Some("common"));
        assert_eq!(rare.grp.as_deref(), Some("rare"));
        assert!(common.rank < rare.rank, "curated order, not alphabetical");
        assert_eq!(
            a.rarities
                .iter()
                .map(|r| r.rarity.as_str())
                .collect::<Vec<_>>(),
            ["Common", "Rare"],
            "rows arrive pre-sorted by rank"
        );
        // The histogram colours by group, so every column carries one.
        assert_eq!(a.copy_counts[0].rarity.as_deref(), Some("Common"));
        assert_eq!(a.copy_counts[0].rarity_grp.as_deref(), Some("common"));

        assert!(analytics(&conn, "nope").unwrap().is_none());
    }

    #[test]
    fn rarity_split_folds_upstream_spelling_onto_one_canonical_tier() {
        // Upstream ships the same tier two ways — pokemontcg.io's
        // "Mega Attack Rare" and TCGCSV's "MEGA_ATTACK_RARE". They are one
        // rarity, and the split must say so: one row, both cards, the
        // catalog's spelling. Before the `rarities` lookup they were two
        // rows, and the stats page's own canonicalRarity() papered over it
        // only for the label — the counts stayed split.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let mut c = open_shared(&shared).unwrap();
            crate::search_meta::reconcile(&mut c).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('me1', 'Mega Evolution', 'ME')",
                [],
            )
            .unwrap();
            for (n, rarity) in [("1", "Mega Attack Rare"), ("2", "MEGA_ATTACK_RARE")] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                     VALUES (?1, 'me1', ?2, ?3, 'Card', ?4)",
                    rusqlite::params![format!("me1-{n}"), n, n.parse::<i64>().unwrap(), rarity],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        let a = analytics(&conn, "me1").unwrap().unwrap();
        assert_eq!(a.rarities.len(), 1, "one tier, not two spellings");
        assert_eq!(a.rarities[0].rarity, "Mega Attack Rare");
        assert_eq!(a.rarities[0].total_cards, 2);
        assert_eq!(a.rarities[0].grp.as_deref(), Some("special"));
        // The histogram gets the canonical spelling on both columns too.
        assert_eq!(a.copy_counts[1].rarity.as_deref(), Some("Mega Attack Rare"));
        assert_eq!(a.copy_counts[1].rarity_grp.as_deref(), Some("special"));
    }

    #[test]
    fn unranked_rarity_keeps_its_string_and_sorts_last() {
        // A tier missing from data/rarities.json is a gap in the seed, not
        // a reason to drop the cards: it keeps its raw label and falls in
        // behind every ranked tier.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let mut c = open_shared(&shared).unwrap();
            crate::search_meta::reconcile(&mut c).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('zz1', 'Zed', 'ZZ')",
                [],
            )
            .unwrap();
            for (n, rarity) in [("1", "Blorbo Rare"), ("2", "Common")] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                     VALUES (?1, 'zz1', ?2, ?3, 'Card', ?4)",
                    rusqlite::params![format!("zz1-{n}"), n, n.parse::<i64>().unwrap(), rarity],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        let a = analytics(&conn, "zz1").unwrap().unwrap();
        assert_eq!(
            a.rarities
                .iter()
                .map(|r| r.rarity.as_str())
                .collect::<Vec<_>>(),
            ["Common", "Blorbo Rare"]
        );
        let unknown = &a.rarities[1];
        assert_eq!(unknown.rank, crate::search_meta::UNRANKED_RARITY);
        assert_eq!(unknown.grp, None);
    }

    #[test]
    fn market_value_takes_cheapest_printing_per_card_not_sum_of_runs() {
        // A WOTC-style card with three print runs (Unlimited / Shadowless /
        // 1st Edition) must contribute only its CHEAPEST run to the full-set
        // value — not the sum of all three. Otherwise a set's "Full set"
        // figure is dominated by 1st-Edition holos (Base Set read ~$28k).
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('base1', 'Base', 'Base')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                 VALUES ('base1-4', 'base1', '4', 4, 'Charizard', 'Rare Holo')",
                [],
            )
            .unwrap();
            // Three print-run printings of the one card, priced 300 /
            // 800 / 10000 (Unlimited / Shadowless / 1st Edition).
            for (variant, product, price) in [
                ("unlimited_holo", 401, 300.0),
                ("shadowless_holo", 402, 800.0),
                ("first_ed_holo", 403, 10000.0),
            ] {
                c.execute(
                    "INSERT INTO printings \
                       (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                     VALUES (?1, 'base1-4', ?2, ?3, 'Holofoil')",
                    rusqlite::params![format!("base1-4-{variant}"), variant, product],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO prices \
                       (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                     VALUES (?1, 'Holofoil', 'tcgplayer', 'market', ?2, '2026-06-01')",
                    rusqlite::params![product, price],
                )
                .unwrap();
            }
            crate::latest_prices::refresh_latest_prices(&c).unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        let a = analytics(&conn, "base1").unwrap().unwrap();
        assert_eq!(a.total_printings, 3);
        // Cheapest run only, not 300 + 800 + 10000 = 11100.
        assert_eq!(a.market_value, 300.0);
    }
}
