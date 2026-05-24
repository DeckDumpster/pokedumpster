//! Variant expansion: derive each card's set of printings (with sub_type
//! and tcgplayer_product_id pre-resolved) from TCGCSV products + prices,
//! then apply the hand-curated overlay for the cases TCGCSV doesn't cover
//! (notably cross-group stamped promos).
//!
//! `data/overrides/variant_augmentations.json` is embedded at compile time
//! so the overlay ships with the binary (the pokedex pattern). See PLAN.md
//! §4 and `data/known_issues.md`.

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;

use pkdump_core::variant::{
    VariantOverride, parse_product_card_name, parse_stamp_tag, sub_type_to_variant,
    variant_from_product_name,
};

use crate::error::Result;
use crate::tcgcsv::normalize_collector_number;

/// TCGCSV group_id for the "Miscellaneous Cards and Products" bin, which
/// hosts stamped promos (Black Bolt Stamped, Darkness Ablaze Stamped, E3
/// Stamped, …). These products live cross-group from the base card's set,
/// so expansion has to look here explicitly to surface them.
const MISC_GROUP_ID: i64 = 2374;

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
    /// Lowercase card name — matched against the card name parsed from
    /// stamp product names when no set keyword is available.
    name_lower: String,
    /// Lowercase set name, used to match against stamp-tag set keywords
    /// like "black bolt" / "darkness ablaze" when resolving cross-group
    /// stamped promos to a base card.
    set_name_lower: String,
    /// The set's `printed_total` (e.g. 146, 102) — matched against the
    /// `/total` half of a stamp product's collector number when no set
    /// keyword is available.
    printed_total: Option<i64>,
}

/// A parsed cross-group product preloaded so the per-card pass is a
/// HashMap lookup. Covers both stamped promos (set_keyword filled when
/// the paren names a set) and non-stamp pattern overlays MCAP hosts
/// for numbered-set cards (Cosmo Holo, etc. — set_keyword is None and
/// the matcher relies on card_name_lower + set_total).
struct CrossGroupProduct {
    product_id: i64,
    variant: String,
    /// Lowercase set keyword from the parenthetical tag, e.g. "black bolt".
    /// When present, the base card must belong to a set whose name
    /// contains this token. When absent (Prerelease, Staff, event-only
    /// stamps; or any non-stamp overlay), the matcher falls back to
    /// `card_name_lower` + `set_total`.
    set_keyword: Option<String>,
    /// Lowercase card name parsed from the product name — used when
    /// `set_keyword` is None.
    card_name_lower: String,
    /// The `/total` half of the collector number (e.g. 146 from
    /// "130/146"). `None` for promos without a printed-total suffix.
    set_total: Option<i64>,
    sub_type: Option<String>,
}

/// Extract the `/total` half of a TCGCSV collector_number like "130/146".
/// Returns `None` when the number has no `/total` suffix.
fn parse_set_total(raw: &str) -> Option<i64> {
    let (_, total) = raw.split_once('/')?;
    total.trim().parse::<i64>().ok()
}

/// Preload every cross-group MCAP product into a map keyed by normalized
/// collector number, so per-card matching is an in-memory lookup. Tries
/// the stamp parser first (Black Bolt Stamped, Prerelease, etc.); if
/// that doesn't bite, falls back to `variant_from_product_name` to pick
/// up non-stamp pattern overlays MCAP hosts for numbered-set reprints
/// (e.g. "Erika's Tangela 007/217 (Cosmo Holo)"). Products that match
/// neither parser are skipped silently.
fn preload_cross_group_products(
    conn: &Connection,
) -> Result<HashMap<String, Vec<CrossGroupProduct>>> {
    let mut stmt = conn.prepare(
        "SELECT product_id, name, collector_number FROM tcgcsv_products \
          WHERE group_id = ?1",
    )?;
    let rows = stmt.query_map([MISC_GROUP_ID], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;

    let mut by_number: HashMap<String, Vec<CrossGroupProduct>> = HashMap::new();
    for row in rows {
        let (product_id, name, raw_num) = row?;
        let Some(num) = raw_num else { continue };
        // Stamp parser first — it consumes the parenthetical when it
        // matches; otherwise try the generic pattern parser. Stamps
        // carry an optional set_keyword for set-disambiguation;
        // non-stamp overlays don't (they rely on card-name + set-total
        // matching at attach time).
        let (variant, set_keyword) = if let Some((v, kw)) = parse_stamp_tag(&name) {
            (v, kw)
        } else if let Some(v) = variant_from_product_name(&name) {
            (v.to_string(), None)
        } else {
            continue;
        };
        let sub_type: Option<String> = conn
            .prepare("SELECT sub_type_name FROM prices WHERE tcgplayer_product_id = ?1 LIMIT 1")?
            .query_row([product_id], |r| r.get(0))
            .ok();
        let card_name_lower = parse_product_card_name(&name).to_lowercase();
        let set_total = parse_set_total(&num);
        by_number
            .entry(normalize_collector_number(&num))
            .or_default()
            .push(CrossGroupProduct {
                product_id,
                variant,
                set_keyword,
                card_name_lower,
                set_total,
                sub_type,
            });
    }
    Ok(by_number)
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
        let mut stmt = conn.prepare(
            "SELECT c.card_id, c.set_code, c.number, c.rarity, \
                    LOWER(c.name), LOWER(s.name), s.printed_total \
               FROM cards c JOIN sets s ON s.set_code = c.set_code",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CardRow {
                card_id: r.get(0)?,
                set_code: r.get(1)?,
                number: r.get(2)?,
                rarity: r.get(3)?,
                name_lower: r.get(4)?,
                set_name_lower: r.get(5)?,
                printed_total: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let cross_group_by_number = preload_cross_group_products(conn)?;

    // Pre-ensure every variant code the upcoming expansion might emit, so
    // the per-card insert loop doesn't pay a SELECT+INSERT roundtrip per
    // (card, variant) pair just to satisfy the FK on printings.variant.
    // TCGCSV-derived variants are already covered by the seed JSON
    // (sub_type_to_variant + variant_from_product_name only return seed
    // codes); we just need to add cross-group MCAP codes from preload +
    // overlay add-lists.
    {
        use std::collections::HashSet;
        let mut codes: HashSet<&str> = HashSet::new();
        for products in cross_group_by_number.values() {
            for s in products {
                codes.insert(&s.variant);
            }
        }
        for ov in overrides {
            for add in &ov.add {
                codes.insert(add);
            }
        }
        let tx = conn.transaction()?;
        for code in &codes {
            pkdump_db::variants::ensure_code(&tx, code)?;
        }
        tx.commit()?;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut printings = 0usize;
    // Periodic progress. Rust block-buffers stdout to pipes (so seed.sh's
    // tee shows nothing during the loop unless we flush). Emit a line
    // every PROGRESS_EVERY cards with elapsed + rate, and flush stdout
    // so it lands in the log immediately.
    const PROGRESS_EVERY: usize = 1_000;
    let total = cards.len();
    let start = std::time::Instant::now();
    use std::io::Write;
    for (i, c) in cards.iter().enumerate() {
        let mut tcgcsv_variants = variants_from_tcgcsv(conn, c)?;
        // Cross-group MCAP products: stamps and non-stamp overlays
        // (e.g. "Cosmo Holo"). The matcher resolves a candidate to
        // this card via either a set-name keyword (stamps) or a
        // card-name + printed_total fallback (non-stamp overlays and
        // stamps without a set keyword). Numbers alone are too
        // ambiguous — the same number appears in many sets.
        if let Some(candidates) = cross_group_by_number.get(&normalize_collector_number(&c.number))
        {
            for product in candidates {
                let matches = match &product.set_keyword {
                    Some(kw) => c.set_name_lower.contains(kw),
                    None => {
                        product.card_name_lower == c.name_lower
                            && product.set_total.is_some()
                            && product.set_total == c.printed_total
                    }
                };
                if matches {
                    tcgcsv_variants.push((
                        product.variant.clone(),
                        product.sub_type.clone(),
                        Some(product.product_id),
                    ));
                }
            }
        }
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
            // FK target was bulk-ensured at function entry above; no
            // per-row check needed.
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

        let done = i + 1;
        if done % PROGRESS_EVERY == 0 || done == total {
            let elapsed = start.elapsed().as_secs_f64();
            let rate = if elapsed > 0.0 {
                done as f64 / elapsed
            } else {
                0.0
            };
            let pct = (done as f64 / total as f64) * 100.0;
            println!(
                "  [{done:>5}/{total}] {pct:>5.1}% · {printings} printings · {elapsed:>5.1}s · {rate:>5.0} cards/s"
            );
            let _ = std::io::stdout().flush();
        }
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
    fn cross_group_stamp_resolves_to_base_card_via_set_keyword() {
        // Victini Black Bolt Stamped (MCAP product 668956) is a stamped
        // version of zsv10pt5-12 Victini. The matcher should resolve the
        // stamp to the base card by (number, set-name keyword) and create
        // a stamp_black_bolt printing tied to the MCAP product, even
        // though Victini lives in a different TCGCSV group.
        let dir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, tcgcsv_group_id, printed_total) \
             VALUES ('zsv10pt5', 'Black Bolt', 'Scarlet & Violet', 24325, 86)",
            [],
        )
        .unwrap();
        // A second set with the same printed_total demonstrates the
        // keyword disambiguates beyond just the number match.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, tcgcsv_group_id, printed_total) \
             VALUES ('rsv10pt5', 'White Flare', 'Scarlet & Violet', 24326, 86)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('zsv10pt5-12', 'zsv10pt5', '12', 12, 'Victini', 'Rare')",
            [],
        )
        .unwrap();
        // A same-numbered Victini in WHT — the stamp must NOT bleed onto it.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('rsv10pt5-12', 'rsv10pt5', '12', 12, 'Victini', 'Rare')",
            [],
        )
        .unwrap();
        // The stamp product, living in the MCAP group.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (668956, 2374, 'Victini (Black Bolt Stamped)', '012/086', NULL, '2026-05-23')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (668956, 'Holofoil', 'tcgplayer', 'market', 5.0, '2026-05-23')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        // Black Bolt Victini gains the stamp printing.
        let row: (String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT variant, sub_type_name, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'zsv10pt5-12-stamp_black_bolt'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "stamp_black_bolt".into(),
                Some("Holofoil".into()),
                Some(668956)
            )
        );

        // White Flare Victini does NOT receive a stamp_black_bolt
        // printing — the keyword filtered it out even though the number
        // matched.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE printing_id = 'rsv10pt5-12-stamp_black_bolt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            leaked, 0,
            "stamp must not bleed across sets with the same total"
        );
    }

    #[test]
    fn keyword_less_stamp_resolves_via_card_name_and_printed_total() {
        // "(Prerelease)" carries no set keyword — the matcher has to
        // disambiguate the base card by (card name, collector number,
        // set printed_total).
        let dir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        // Two sets, only one with the matching printed_total.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('hgss4', 'Triumphant', 'HGSS', 102), \
                    ('xy12', 'Evolutions', 'XY', 108)",
            [],
        )
        .unwrap();
        // Wartortle in two sets — only the one whose printed_total is
        // 112 should match the (Prerelease) stamp.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('ex2', 'Sandstorm', 'EX', 100)",
            [],
        )
        .unwrap();
        // Set with the matching total.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('hgss5', 'Call of Legends', 'HGSS', 112)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('hgss5-50', 'hgss5', '50', 50, 'Wartortle', 'Uncommon')",
            [],
        )
        .unwrap();
        // Same name+number in another set with different total — must NOT match.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('ex2-50', 'ex2', '50', 50, 'Wartortle', 'Uncommon')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (285695, 2374, 'Wartortle - 50/112 (Prerelease)', '050/112', NULL, '2026-05-23')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        // The Call of Legends Wartortle gains a stamp_prerelease printing.
        let resolved: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'hgss5-50-stamp_prerelease'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 285695);
        // The Sandstorm Wartortle (same number, different total) does not.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE printing_id = 'ex2-50-stamp_prerelease'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0, "printed_total must disambiguate the base set");
    }

    #[test]
    fn cross_group_cosmos_holo_attaches_to_numbered_set_card() {
        // Erika's Tangela (Cosmo Holo), MCAP product 679253, is a
        // non-stamp pattern reprint of me2pt5-7 (217-card set). The
        // matcher should resolve it via card-name + set-total fallback
        // (the parenthetical names a treatment, not a set, so there's
        // no set_keyword) and attach a `cosmos_holo` printing.
        let dir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, tcgcsv_group_id, printed_total) \
             VALUES ('me2pt5', 'Ascended Heroes', 'Mega Evolution', 30001, 217)",
            [],
        )
        .unwrap();
        // A second set with a DIFFERENT total — same name+number must
        // not pull the cosmos holo in via the card-name fallback.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('gym1', 'Gym Heroes', 'Gym', 132)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('me2pt5-7', 'me2pt5', '7', 7, 'Erika''s Tangela', 'Common')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('gym1-79', 'gym1', '79', 79, 'Erika''s Tangela', 'Common')",
            [],
        )
        .unwrap();
        // The MCAP product — group 2374, "(Cosmo Holo)" trailing tag.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (679253, 2374, 'Erika''s Tangela - 007/217 (Cosmo Holo)', '007/217', NULL, '2026-05-24')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (679253, 'Holofoil', 'tcgplayer', 'market', 2.0, '2026-05-24')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        // The ASC Erika's Tangela gains a cosmos_holo printing tied to
        // the MCAP product.
        let row: (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT sub_type_name, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'me2pt5-7-cosmos_holo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (Some("Holofoil".into()), Some(679253)));
        // The 1999 Erika's Tangela in gym1 does NOT receive a
        // cosmos_holo printing — the set total disambiguates.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE printing_id = 'gym1-79-cosmos_holo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            leaked, 0,
            "cosmo holo must not bleed onto same-named card in another set"
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
