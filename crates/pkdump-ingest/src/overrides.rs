//! Layer 3 of variant expansion: the JSON overlay, and the routine that
//! runs the full expansion into the `printings` table.
//!
//! `data/overrides/variant_augmentations.json` is embedded at compile time so
//! the overrides ship with the binary (the pokedex pattern). See PLAN.md §4
//! and `data/known_issues.md`.

use rusqlite::Connection;
use serde_json::Value;

use pkdump_core::variant::{VariantOverride, expand_variants};

use crate::error::Result;

const VARIANT_AUGMENTATIONS: &str =
    include_str!("../../../data/overrides/variant_augmentations.json");

/// Parse the embedded variant-augmentation overlay.
pub fn load_variant_augmentations() -> Result<Vec<VariantOverride>> {
    Ok(serde_json::from_str(VARIANT_AUGMENTATIONS)?)
}

/// Extract the TCGplayer price-block keys from a card's stored raw JSON.
fn price_keys_from_raw(raw_json: &str) -> Vec<String> {
    serde_json::from_str::<Value>(raw_json)
        .ok()
        .as_ref()
        .and_then(|v| v.get("tcgplayer"))
        .and_then(|v| v.get("prices"))
        .and_then(Value::as_object)
        .map(|prices| prices.keys().cloned().collect())
        .unwrap_or_default()
}

/// Run variant expansion for every card in the catalog, writing `printings`
/// rows. Printings expansion no longer produces are soft-deprecated — their
/// `deprecated_at` is set, never deleted (PLAN.md §4.4). Idempotent.
pub fn expand_all_printings(conn: &mut Connection, overrides: &[VariantOverride]) -> Result<usize> {
    let cards: Vec<(String, String, String, Option<String>, String)> = {
        let mut stmt =
            conn.prepare("SELECT card_id, set_code, number, rarity, raw_json FROM cards")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let now = chrono::Utc::now().to_rfc3339();
    let tx = conn.transaction()?;
    let mut printings = 0usize;
    for (card_id, set_code, number, rarity, raw_json) in &cards {
        let price_keys = price_keys_from_raw(raw_json);
        let variants = expand_variants(set_code, number, rarity.as_deref(), &price_keys, overrides);

        // Deprecate every currently-live printing of this card; the upserts
        // below revive the ones expansion still produces. Printings that drop
        // out keep their original deprecated_at and stay hidden.
        tx.execute(
            "UPDATE printings SET deprecated_at = ?1 \
             WHERE card_id = ?2 AND deprecated_at IS NULL",
            rusqlite::params![now, card_id],
        )?;
        for variant in &variants {
            let printing_id = format!("{card_id}-{variant}");
            tx.execute(
                "INSERT INTO printings (printing_id, card_id, variant, language) \
                 VALUES (?1, ?2, ?3, 'en') \
                 ON CONFLICT(printing_id) DO UPDATE SET deprecated_at = NULL",
                rusqlite::params![printing_id, card_id, variant],
            )?;
            printings += 1;
        }
    }
    tx.commit()?;
    Ok(printings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) \
             VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        // Common with TCGplayer normal + reverseHolofoil price keys.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity, raw_json) \
             VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur', 'Common', ?1)",
            [r#"{"tcgplayer":{"prices":{"normal":{},"reverseHolofoil":{}}}}"#],
        )
        .unwrap();
        // Uncommon with no TCGplayer block — exercises the rarity bootstrap.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity, raw_json) \
             VALUES ('sv3pt5-2', 'sv3pt5', '2', 2, 'Ivysaur', 'Uncommon', '{}')",
            [],
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn embedded_overlay_parses() {
        assert!(!load_variant_augmentations().unwrap().is_empty());
    }

    #[test]
    fn expands_cards_into_printings() {
        let (_d, mut conn) = seed();
        let overlay = load_variant_augmentations().unwrap();
        expand_all_printings(&mut conn, &overlay).unwrap();

        // sv3pt5-1: normal + reverse_holo (price keys) + the 151 overlay's
        // pokeball_rh + masterball_rh.
        let v1: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT variant FROM printings \
                     WHERE card_id = 'sv3pt5-1' AND deprecated_at IS NULL \
                     ORDER BY variant",
                )
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            v1,
            vec!["masterball_rh", "normal", "pokeball_rh", "reverse_holo"]
        );

        // sv3pt5-2: bootstrap (normal + reverse_holo) + overlay ball patterns.
        let n2: i64 = conn
            .query_row(
                "SELECT count(*) FROM printings \
                 WHERE card_id = 'sv3pt5-2' AND deprecated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n2, 4);
    }

    #[test]
    fn soft_deprecates_dropped_printings() {
        let (_d, mut conn) = seed();
        expand_all_printings(&mut conn, &load_variant_augmentations().unwrap()).unwrap();

        // Re-run with no overlay — the ball-pattern printings drop out.
        expand_all_printings(&mut conn, &[]).unwrap();

        let pokeball: Option<String> = conn
            .query_row(
                "SELECT deprecated_at FROM printings WHERE printing_id = 'sv3pt5-1-pokeball_rh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            pokeball.is_some(),
            "a dropped printing must be soft-deprecated, never deleted"
        );

        let normal: Option<String> = conn
            .query_row(
                "SELECT deprecated_at FROM printings WHERE printing_id = 'sv3pt5-1-normal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(normal.is_none(), "a still-live printing stays un-deprecated");
    }
}
