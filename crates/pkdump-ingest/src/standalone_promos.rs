//! Curated synthesis for truly-standalone promos — cards that exist in
//! TCGplayer's "Miscellaneous Cards & Products" catch-all (group 2374) but
//! have no parent set at all (e.g. the sealed English Ancient Mew movie
//! promo). They can't bridge onto a base card the way stamped/treatment
//! reprints do, so we hand-list them and synthesize a card + printing into
//! a dedicated "Miscellaneous Promos" set.
//!
//! `data/overrides/standalone_promos.json` is the curated list, embedded at
//! compile time (the pokedex pattern). `expand_all_printings` skips
//! `PROMO_SET_CODE` so it never clobbers the printings written here.

use rusqlite::Connection;
use serde::Deserialize;

use crate::error::Result;

/// Set that hosts setless promos. `expand_all_printings` excludes it.
pub const PROMO_SET_CODE: &str = "mcap";

const PROMO_SET_NAME: &str = "Miscellaneous Promos";
const PROMO_SET_SERIES: &str = "Other";
// Arbitrary but stable so the /browse "Other" bucket orders this set
// deterministically; the set spans many eras so no real date fits.
const PROMO_SET_RELEASE_DATE: &str = "2000/07/18";

const STANDALONE_PROMOS_JSON: &str = include_str!("../../../data/overrides/standalone_promos.json");

#[derive(Debug, Deserialize)]
struct StandalonePromo {
    product_id: i64,
    number: String,
    name: String,
    variant: String,
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
}

fn load() -> Result<Vec<StandalonePromo>> {
    Ok(serde_json::from_str(STANDALONE_PROMOS_JSON)?)
}

/// Synthesize the Miscellaneous Promos set + one card and printing per
/// curated entry. Rarity, image, and the printing's `sub_type_name` are
/// pulled from the product's own `tcgcsv_products` / `prices` rows so the
/// printing matches a `latest_prices` key and shows a market price.
/// Idempotent: cards upsert, printings upsert on `printing_id`.
pub fn synthesize_standalone_promos(conn: &mut Connection) -> Result<usize> {
    let promos = load()?;
    let tx = conn.transaction()?;

    // Set shell. INSERT OR IGNORE so a real upstream row (should one ever
    // appear) wins, matching the synthesize_cards_for_bridges convention.
    tx.execute(
        "INSERT OR IGNORE INTO sets (set_code, name, series, release_date) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            PROMO_SET_CODE,
            PROMO_SET_NAME,
            PROMO_SET_SERIES,
            PROMO_SET_RELEASE_DATE
        ],
    )?;

    let mut n = 0;
    for p in &promos {
        // Image + rarity from the product; sub_type from its price rows so
        // the printing keys into latest_prices.
        let (image, rarity): (Option<String>, Option<String>) = tx
            .prepare("SELECT image_url, rarity FROM tcgcsv_products WHERE product_id = ?1")?
            .query_row([p.product_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap_or((None, None));
        let sub_type: Option<String> = tx
            .prepare("SELECT sub_type_name FROM prices WHERE tcgplayer_product_id = ?1 LIMIT 1")?
            .query_row([p.product_id], |r| r.get(0))
            .ok();

        let card_id = format!("{PROMO_SET_CODE}-{}", p.number);
        let sortable = pkdump_core::number_sortable(&p.number);

        tx.execute(
            "INSERT OR IGNORE INTO cards \
               (card_id, set_code, number, number_sortable, name, rarity, \
                image_small, image_large) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![
                card_id,
                PROMO_SET_CODE,
                p.number,
                sortable,
                p.name,
                rarity,
                image
            ],
        )?;
        n += tx.changes() as usize;
        // Heal a synth-owned row (raw_json IS NULL → not upstream-managed).
        tx.execute(
            "UPDATE cards SET name = ?2, rarity = ?3, image_small = ?4, image_large = ?4 \
              WHERE card_id = ?1 AND raw_json IS NULL",
            rusqlite::params![card_id, p.name, rarity, image],
        )?;

        // FK target for printings.variant.
        pkdump_db::variants::ensure_code(&tx, &p.variant)?;

        let printing_id = format!("{card_id}-{}", p.variant);
        tx.execute(
            "INSERT INTO printings \
               (printing_id, card_id, variant, language, sub_type_name, tcgplayer_product_id) \
             VALUES (?1, ?2, ?3, 'en', ?4, ?5) \
             ON CONFLICT(printing_id) DO UPDATE SET \
               deprecated_at = NULL, \
               sub_type_name = excluded.sub_type_name, \
               tcgplayer_product_id = excluded.tcgplayer_product_id",
            rusqlite::params![printing_id, card_id, p.variant, sub_type, p.product_id],
        )?;
    }
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkdump_db::open_shared;

    #[test]
    fn parses_curated_list() {
        // The embedded JSON must always parse — it ships in the binary.
        let promos = load().unwrap();
        assert!(
            promos
                .iter()
                .any(|p| p.name == "Ancient Mew" && p.number == "1")
        );
    }

    #[test]
    fn synthesizes_card_and_priced_printing() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        pkdump_db::variants::reconcile(&mut conn).unwrap();
        // Seed the Ancient Mew product + a market price so synth can pull
        // image/rarity/sub_type and the printing keys into latest_prices.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, rarity, image_url, fetched_at) \
             VALUES (108589, 2374, 'Ancient Mew', '1', 'Promo', 'http://img/108589.jpg', '2026-06-02')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (108589, 'Holofoil', 'tcgplayer', 'market', 122.26, '2026-06-02')",
            [],
        )
        .unwrap();

        let n = synthesize_standalone_promos(&mut conn).unwrap();
        assert!(n >= 1);

        // The set exists under the Other bucket.
        let series: String = conn
            .query_row(
                "SELECT series FROM sets WHERE set_code = ?1",
                [PROMO_SET_CODE],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(series, "Other");

        // Ancient Mew resolves to a holo printing linked to its product.
        let (variant, pid, sub): (String, i64, String) = conn
            .query_row(
                "SELECT variant, tcgplayer_product_id, sub_type_name FROM printings \
                  WHERE printing_id = 'mcap-1-holo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (variant.as_str(), pid, sub.as_str()),
            ("holo", 108589, "Holofoil")
        );

        // Idempotent: a second run inserts no new cards and keeps the link.
        let n2 = synthesize_standalone_promos(&mut conn).unwrap();
        assert_eq!(n2, 0);
    }
}
