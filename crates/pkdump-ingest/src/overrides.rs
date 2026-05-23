//! Variant expansion: derive each card's set of printings (with sub_type
//! and tcgplayer_product_id pre-resolved) from TCGCSV products + prices,
//! then apply the hand-curated overlay for the cases TCGCSV doesn't cover
//! (notably cross-group stamped promos).
//!
//! `data/overrides/variant_augmentations.json` is embedded at compile time
//! so the overlay ships with the binary (the pokedex pattern). See PLAN.md
//! §4 and `data/known_issues.md`.

use std::collections::BTreeMap;

use rusqlite::Connection;

use pkdump_core::variant::{VariantOverride, sub_type_to_variant};

use crate::error::Result;
use crate::tcgcsv::normalize_collector_number;

const VARIANT_AUGMENTATIONS: &str =
    include_str!("../../../data/overrides/variant_augmentations.json");

/// Parse the embedded variant-augmentation overlay.
pub fn load_variant_augmentations() -> Result<Vec<VariantOverride>> {
    Ok(serde_json::from_str(VARIANT_AUGMENTATIONS)?)
}

/// (variant_code, sub_type_name, tcgplayer_product_id). sub_type and
/// product_id are `None` for overlay-added variants — those exist in the
/// real world but lack a TCGplayer record (stamped promos, etc.).
type VariantResolution = (String, Option<String>, Option<i64>);

struct CardRow {
    card_id: String,
    set_code: String,
    number: String,
    rarity: Option<String>,
}

/// Look up TCGCSV products for a card and resolve each to (variant,
/// sub_type, product_id). Returns an empty vec when the card has no TCGCSV
/// products — caller handles the bootstrap fallback.
fn variants_from_tcgcsv(conn: &Connection, card: &CardRow) -> Result<Vec<VariantResolution>> {
    // Pull every product in the card's group whose collector number
    // matches (TCGCSV's number is suffixed with the set total, our cards
    // table has the bare form; normalize both).
    let mut stmt = conn.prepare(
        "SELECT tp.product_id, tp.collector_number, tp.derived_variant \
           FROM tcgcsv_products tp \
           JOIN sets s ON s.tcgcsv_group_id = tp.group_id \
          WHERE s.set_code = ?1",
    )?;
    let raw: Vec<(i64, Option<String>, Option<String>)> = stmt
        .query_map([&card.set_code], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let want = normalize_collector_number(&card.number);
    let mut out: Vec<VariantResolution> = Vec::new();
    for (product_id, raw_num, derived) in raw {
        let Some(num) = raw_num else { continue };
        if normalize_collector_number(&num) != want {
            continue;
        }
        match derived {
            Some(pattern) => {
                // Pattern product owns its product_id; take whatever
                // sub_type TCGCSV prices it under (set-dependent: ASC uses
                // "Reverse Holofoil", BLK/WHT use "Holofoil"). One sub_type
                // per pattern product in practice.
                let sub: Option<String> = conn
                    .prepare(
                        "SELECT sub_type_name FROM prices \
                          WHERE tcgplayer_product_id = ?1 LIMIT 1",
                    )?
                    .query_row([product_id], |r| r.get(0))
                    .ok();
                out.push((pattern, sub, Some(product_id)));
            }
            None => {
                // Base product: one variant per advertised sub_type.
                let mut p_stmt = conn.prepare(
                    "SELECT DISTINCT sub_type_name FROM prices \
                      WHERE tcgplayer_product_id = ?1",
                )?;
                let sub_types: Vec<String> = p_stmt
                    .query_map([product_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for sub in sub_types {
                    if let Some(variant) = sub_type_to_variant(&sub) {
                        out.push((variant.to_string(), Some(sub), Some(product_id)));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Run variant expansion for every card. Each printing carries its
/// `variant`, `sub_type_name`, and `tcgplayer_product_id` so price queries
/// stay a straight JOIN with no per-variant conditional logic.
///
/// Printings expansion no longer produces are soft-deprecated — their
/// `deprecated_at` is set, never deleted (PLAN.md §4.4). Idempotent.
pub fn expand_all_printings(conn: &mut Connection, overrides: &[VariantOverride]) -> Result<usize> {
    let cards: Vec<CardRow> = {
        let mut stmt = conn.prepare("SELECT card_id, set_code, number, rarity FROM cards")?;
        let rows = stmt.query_map([], |r| {
            Ok(CardRow {
                card_id: r.get(0)?,
                set_code: r.get(1)?,
                number: r.get(2)?,
                rarity: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut printings = 0usize;
    for c in &cards {
        let tcgcsv_variants = variants_from_tcgcsv(conn, c)?;
        let base: Vec<VariantResolution> = if tcgcsv_variants.is_empty() {
            // Brand-new card TCGCSV hasn't indexed yet — give it a `normal`
            // placeholder so the binder still renders it, and let the next
            // refresh replace with the real set once TCGCSV catches up.
            vec![("normal".into(), None, None)]
        } else {
            tcgcsv_variants
        };

        // De-dup + apply overlay. Map preserves the resolution
        // (sub_type/product_id) for TCGCSV variants and lets overlay-added
        // variants come in with NULLs.
        let mut variant_map: BTreeMap<String, (Option<String>, Option<i64>)> = BTreeMap::new();
        for (variant, sub, pid) in base {
            variant_map.entry(variant).or_insert((sub, pid));
        }
        for ov in overrides {
            if ov
                .match_
                .matches(&c.set_code, c.rarity.as_deref(), &c.number)
            {
                for add in &ov.add {
                    variant_map.entry(add.clone()).or_insert((None, None));
                }
                for remove in &ov.remove {
                    variant_map.remove(remove);
                }
            }
        }

        // Open a per-card transaction so a refresh that's interrupted
        // mid-stream doesn't leave a card half-deprecated.
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE printings SET deprecated_at = ?1 \
             WHERE card_id = ?2 AND deprecated_at IS NULL",
            rusqlite::params![now, c.card_id],
        )?;
        for (variant, (sub, pid)) in variant_map {
            let printing_id = format!("{}-{variant}", c.card_id);
            tx.execute(
                "INSERT INTO printings (printing_id, card_id, variant, language, \
                                        sub_type_name, tcgplayer_product_id) \
                 VALUES (?1, ?2, ?3, 'en', ?4, ?5) \
                 ON CONFLICT(printing_id) DO UPDATE SET \
                   deprecated_at = NULL, \
                   sub_type_name = excluded.sub_type_name, \
                   tcgplayer_product_id = excluded.tcgplayer_product_id",
                rusqlite::params![printing_id, c.card_id, variant, sub, pid],
            )?;
            printings += 1;
        }
        tx.commit()?;
    }
    Ok(printings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, tcgcsv_group_id) \
             VALUES ('me2pt5', 'Ascended Heroes', 'Mega Evolution', 24541)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('me2pt5-158', 'me2pt5', '158', 158, 'Dreepy', 'Common')",
            [],
        )
        .unwrap();
        // Base product with only "Normal" sub_type (ASC C/U/R have no plain RH).
        for (pid, num, name, derived) in [
            (675970i64, "158/217", "Dreepy - 158/217", None::<&str>),
            (
                676976,
                "158/217",
                "Dreepy - 158/217 (Quick Ball)",
                Some("quickball_rh"),
            ),
            (
                677116,
                "158/217",
                "Dreepy - 158/217 (Energy Symbol Pattern)",
                Some("energy_symbol_rh"),
            ),
        ] {
            conn.execute(
                "INSERT INTO tcgcsv_products \
                   (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
                 VALUES (?1, 24541, ?2, ?3, ?4, '2026-05-23')",
                rusqlite::params![pid, name, num, derived],
            )
            .unwrap();
        }
        // Prices establish the sub_types per product (Normal for base,
        // Reverse Holofoil for the patterns — set-specific convention).
        for (pid, sub) in [
            (675970i64, "Normal"),
            (676976, "Reverse Holofoil"),
            (677116, "Reverse Holofoil"),
        ] {
            conn.execute(
                "INSERT INTO prices \
                   (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                 VALUES (?1, ?2, 'tcgplayer', 'market', 1.0, '2026-05-23')",
                rusqlite::params![pid, sub],
            )
            .unwrap();
        }
        (dir, conn)
    }

    #[test]
    fn embedded_overlay_parses() {
        assert!(load_variant_augmentations().is_ok());
    }

    #[test]
    fn asc_dreepy_resolves_to_three_truthful_variants() {
        let (_d, mut conn) = seed();
        expand_all_printings(&mut conn, &[]).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT variant, sub_type_name, tcgplayer_product_id FROM printings \
                 WHERE card_id = 'me2pt5-158' AND deprecated_at IS NULL \
                 ORDER BY variant",
            )
            .unwrap();
        let rows: Vec<(String, Option<String>, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "energy_symbol_rh".into(),
                    Some("Reverse Holofoil".into()),
                    Some(677116)
                ),
                ("normal".into(), Some("Normal".into()), Some(675970)),
                (
                    "quickball_rh".into(),
                    Some("Reverse Holofoil".into()),
                    Some(676976)
                ),
            ],
            "no fabricated reverse_holo; each variant carries its real sub_type and product_id",
        );
    }

    #[test]
    fn base_product_with_multiple_sub_types_splits_into_multiple_variants() {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, tcgcsv_group_id) \
             VALUES ('sv3pt5', '151', 'Scarlet & Violet', 23237)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur', 'Common')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (502552, 23237, 'Bulbasaur - 001/165', '001/165', NULL, '2026-05-23')",
            [],
        )
        .unwrap();
        // Base product carries Normal AND Reverse Holofoil sub_types →
        // expansion should produce both `normal` and `reverse_holo`
        // pointing at the same product_id but distinct sub_type_name.
        for sub in ["Normal", "Reverse Holofoil"] {
            conn.execute(
                "INSERT INTO prices \
                   (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                 VALUES (502552, ?1, 'tcgplayer', 'market', 0.10, '2026-05-23')",
                [sub],
            )
            .unwrap();
        }
        let mut conn = conn;
        expand_all_printings(&mut conn, &[]).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT variant, sub_type_name FROM printings \
                 WHERE card_id = 'sv3pt5-1' AND deprecated_at IS NULL ORDER BY variant",
            )
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("normal".into(), "Normal".into()),
                ("reverse_holo".into(), "Reverse Holofoil".into()),
            ]
        );
    }

    #[test]
    fn card_without_tcgcsv_falls_back_to_normal_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) \
             VALUES ('xx99', 'Brand New Set', 'Test')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('xx99-1', 'xx99', '1', 1, 'Fakemon', 'Common')",
            [],
        )
        .unwrap();
        // No TCGCSV row for this set.
        expand_all_printings(&mut conn, &[]).unwrap();
        let variants: Vec<String> = conn
            .prepare(
                "SELECT variant FROM printings WHERE card_id = 'xx99-1' AND deprecated_at IS NULL",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            variants,
            vec!["normal"],
            "fallback creates exactly one printing — TCGCSV refines on next refresh"
        );
    }

    #[test]
    fn overlay_can_inject_variants_tcgcsv_does_not_carry() {
        // Stamped promos and similar live in different TCGCSV groups and
        // can't be matched automatically yet, so the overlay still has to
        // add them by hand. Verify those add even when TCGCSV already
        // produced other variants for the card.
        let (_d, mut conn) = seed();
        let overlay: Vec<VariantOverride> = serde_json::from_str(
            r#"[{"match":{"set":"me2pt5","number":"158"},"add":["stamp_pokemoncenter"]}]"#,
        )
        .unwrap();
        expand_all_printings(&mut conn, &overlay).unwrap();
        let stamp_sub: Option<Option<String>> = conn
            .query_row(
                "SELECT sub_type_name FROM printings WHERE printing_id = 'me2pt5-158-stamp_pokemoncenter'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert!(stamp_sub.is_some(), "overlay add created the printing");
        assert!(
            stamp_sub.unwrap().is_none(),
            "overlay-added variants don't have a TCGplayer sub_type"
        );
    }

    #[test]
    fn soft_deprecates_dropped_printings() {
        let (_d, mut conn) = seed();
        // First run with no overlay — produces 3 variants.
        expand_all_printings(&mut conn, &[]).unwrap();
        // Now wipe the Quick Ball product so the next expansion drops it.
        conn.execute("DELETE FROM tcgcsv_products WHERE product_id = 676976", [])
            .unwrap();
        conn.execute("DELETE FROM prices WHERE tcgplayer_product_id = 676976", [])
            .unwrap();
        expand_all_printings(&mut conn, &[]).unwrap();
        let dropped: Option<String> = conn
            .query_row(
                "SELECT deprecated_at FROM printings WHERE printing_id = 'me2pt5-158-quickball_rh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(dropped.is_some(), "dropped variant got soft-deprecated");
    }
}
