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
    VariantOverride, parse_product_card_name, parse_stamp_tag, variant_from_product_name,
};
use pkdump_db::sub_type_map::SubTypeVariantMap;

use crate::error::Result;
use crate::tcgcsv::normalize_collector_number;

/// TCGCSV groups scanned cross-group during variant expansion. Products
/// here belong to base cards that live in a different group (the card's
/// own set), so expansion has to look here explicitly to surface them.
///
/// - 2374 "Miscellaneous Cards and Products" (MCAP) — stamped promos
///   (Black Bolt Stamped, E3 Stamped, …) plus non-stamp pattern overlays
///   (Cosmo Holo, etc.). Variant comes from the product name.
/// - 1840 "Deck Exclusives" — non-foil reprints that ship inside
///   preconstructed products (Build & Battle decks, theme decks,
///   Battle Academies). Most entries are bare-name "Zacian - 045/094"
///   with no parenthetical; the variant comes from the
///   `prices.sub_type_name` (typically "Normal") rather than the name.
/// - 3179 / 23266 / 23561 — Trick or Trade BOOster Bundle (2022 / 2023 /
///   2024). All three years stamp reprinted cards with a Halloween
///   Pikachu jack-o-lantern on the artwork; the 10 TTBB 2024 Cosmos Holo
///   specials add a Cosmos Holo treatment too. Routed via two new
///   variant codes (`stamp_trick_or_trade`, `cosmos_holo_trick_or_trade`)
///   — see pokedumpster-vz2 / pokedumpster-bn9.
/// - 22872 "SV: Scarlet & Violet Promo Cards" (svp) — Prerelease and
///   Prerelease[Staff] promos that carry the *base* set's collector
///   number (e.g. "Sinistcha - 022/167 (Prerelease)" = sv6-22) and are
///   physically the base card plus a stamp. Bare SVP-namespaced promos
///   (e.g. "Aegislash - 060") carry no `/total`, so the printed_total
///   matcher rejects them and they stay svp cards. See pokedumpster-zq4.
/// - 2289 "Blister Exclusives" — Cosmos Holo promos numbered against
///   their base set (e.g. "Beheeyem - 62/99 (Cosmos Holo)"). Resolve via
///   the card-name + printed_total fallback as `cosmos_holo`.
const CROSS_GROUP_SOURCE_GROUPS: &[i64] = &[2374, 1840, 3179, 23266, 23561, 22872, 2289];

/// Subset of `CROSS_GROUP_SOURCE_GROUPS` whose bare-name products resolve
/// via the `tcgcsv_sub_type_variant_map` lookup instead of via a
/// name-pattern parser.
const DECK_EXCLUSIVES_GROUP_ID: i64 = 1840;

/// TCGplayer's "Miscellaneous Cards & Products" catch-all. Numbered
/// reprints here whose parenthetical isn't a recognized foil/stamp fall
/// back to the generic `promo` variant (retailer/event distribution).
const MCAP_GROUP_ID: i64 = 2374;

/// TCGCSV groups for the Trick or Trade BOOster Bundles. Bare-name
/// products in these groups (no parenthetical) carry the year-agnostic
/// Halloween Pikachu stamp on the card artwork → `stamp_trick_or_trade`.
/// Parenthetical "(Cosmos Holo)" products in TTBB 2024 (23561) layer a
/// Cosmos Holo treatment on top of the stamp → `cosmos_holo_trick_or_trade`.
const TTBB_GROUP_IDS: &[i64] = &[3179, 23266, 23561];

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

/// The lowercased text of the LAST parenthetical in a product name, e.g.
/// "Duraludon (Surging Sparks)" → Some("surging sparks"). `None` when the
/// name has no parenthetical. Mirrors the accent fold parse_stamp_tag uses
/// so set names with "Pokémon" match the catalog either way.
fn trailing_paren_lower(name: &str) -> Option<String> {
    let lower = name.to_lowercase().replace(['é', 'è', 'ê'], "e");
    let open = lower.rfind('(')?;
    let close = lower[open..].find(')')?;
    Some(lower[open + 1..open + close].trim().to_string())
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
    sub_type_map: &SubTypeVariantMap,
) -> Result<HashMap<String, Vec<CrossGroupProduct>>> {
    // i64 list → string is injection-safe; rusqlite's IN-list ergonomics
    // are awkward for a small static set, so inline the IDs.
    let in_list = CROSS_GROUP_SOURCE_GROUPS
        .iter()
        .map(|g| g.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT product_id, group_id, name, collector_number FROM tcgcsv_products \
          WHERE group_id IN ({in_list})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;

    // (lowercased set name, printed_total) → present. Lets the resolution
    // loop recognize SV-era Build & Battle / Prerelease promos that TCGCSV
    // names by their *set* — "Duraludon (Surging Sparks)" 129/191 — rather
    // than by a stamp suffix. Validating the parenthetical against a real
    // set name AND the collector /total against that set's printed_total
    // keeps it from firing on retailer-exclusive or holo-treatment
    // parentheticals ("(Toys R Us Promo)", "(Cosmos Foil)", …) that don't
    // name a set. See pokedumpster.
    let set_name_total: std::collections::HashSet<(String, i64)> = {
        let mut s = conn.prepare(
            "SELECT lower(name), printed_total FROM sets WHERE printed_total IS NOT NULL",
        )?;
        let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut by_number: HashMap<String, Vec<CrossGroupProduct>> = HashMap::new();
    for row in rows {
        let (product_id, group_id, name, raw_num) = row?;
        let Some(num) = raw_num else { continue };
        // Resolution order:
        //   1. Stamp parser ("(Black Bolt Stamped)", "(Prerelease)", …)
        //      — consumes the parenthetical, may yield a set_keyword.
        //   2. Pattern parser ("(Cosmo Holo)", "(Master Ball Pattern)", …)
        //      — name carries the variant; no set_keyword.
        //   3. Deck Exclusives: bare-name products (no parenthetical) in
        //      group 1840. Variant comes from the sub_type — typically
        //      "Normal" → `normal`. Products in 1840 with an unrecognized
        //      parenthetical (e.g. "(Cracked Ice Holo)") are skipped here
        //      and would need their own variant code before being lifted
        //      in via the pattern parser.
        let (variant, set_keyword, sub_type) = if let Some((v, kw)) = parse_stamp_tag(&name) {
            let sub = sub_type_first(conn, product_id)?;
            (v, kw, sub)
        } else if let Some(v) = variant_from_product_name(&name) {
            let sub = sub_type_first(conn, product_id)?;
            // TTBB 2024 Cosmos Holo specials parse as "cosmos_holo" by the
            // generic pattern parser. They're TTBB-exclusive (modern sets
            // don't otherwise ship Cosmos Holo), so route them to the
            // combined variant code that identifies them as TTBB on the
            // chip. See pokedumpster-vz2.
            let routed = if TTBB_GROUP_IDS.contains(&group_id) && v == "cosmos_holo" {
                "cosmos_holo_trick_or_trade".to_string()
            } else {
                v.to_string()
            };
            (routed, None, sub)
        } else if group_id == DECK_EXCLUSIVES_GROUP_ID && !name.contains('(') {
            let Some(sub) = sub_type_first(conn, product_id)? else {
                continue;
            };
            let Some(v) = sub_type_map.lookup(group_id, &sub) else {
                continue;
            };
            (v.to_string(), None, Some(sub))
        } else if TTBB_GROUP_IDS.contains(&group_id)
            && trailing_paren_lower(&name).is_none_or(|p| p.ends_with("copyright date"))
        {
            // TTBB reprints — e.g. "Phantump 016/196", "Sprigatito -
            // 012/193". The distinguishing feature is the Halloween
            // Pikachu stamp on the artwork. All three TTBB years share
            // the same stamp graphic, so they share one variant code; the
            // year is recoverable via the printing's tcgcsv group_id.
            //
            // Most are bare-name (no parenthetical). The exception is a
            // handful of cards reprinted in TTBB from two original print
            // runs that TCGCSV disambiguates by copyright line, e.g.
            // "Gengar (2022 Copyright Date)". That's still a Halloween
            // stamp — the copyright date is a print-run detail our variant
            // taxonomy doesn't model — so it routes to the same code. No
            // card carries both a plain and a copyright-dated TTBB product
            // in one group, so there's no printing-id collision.
            let sub = sub_type_first(conn, product_id)?;
            ("stamp_trick_or_trade".to_string(), None, sub)
        } else if let Some(set_kw) = trailing_paren_lower(&name)
            .zip(parse_set_total(&num))
            .filter(|(kw, total)| set_name_total.contains(&(kw.clone(), *total)))
            .map(|(kw, _)| kw)
        {
            // SV-era Build & Battle / Prerelease promo named by its set,
            // e.g. "Duraludon (Surging Sparks)" 129/191. The parenthetical
            // is a real set name and the /total matches that set's
            // printed_total — a set-logo stamp on the base card. Route to
            // the same `stamp_prerelease` code as the older "(Prerelease)"
            // promos; the set name becomes the disambiguating keyword.
            let sub = sub_type_first(conn, product_id)?;
            ("stamp_prerelease".to_string(), Some(set_kw), sub)
        } else if group_id == MCAP_GROUP_ID && name.contains('(') {
            // MCAP fallback: a numbered reprint with a parenthetical the
            // treatment/stamp parsers didn't recognize — a retailer or
            // event promo with no special foil (Toys R Us, Best Buy,
            // Build-A-Bear, SDCC, Movie Promo, …). Model foil-treatment-
            // only: collapse the distributor to one generic `promo`
            // variant. Still gated by the card-name + printed_total match
            // below, so only products that resolve to a real base card
            // actually become printings.
            let sub = sub_type_first(conn, product_id)?;
            ("promo".to_string(), None, sub)
        } else {
            continue;
        };
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

/// Fetch one `sub_type_name` from `prices` for the given product. Pattern
/// and stamp products in practice carry a single sub_type each, so LIMIT 1
/// is safe (whichever row the engine returns is the right one). For
/// products that lack any price row this returns `None` and the caller
/// falls back accordingly.
fn sub_type_first(conn: &Connection, product_id: i64) -> Result<Option<String>> {
    Ok(conn
        .prepare("SELECT sub_type_name FROM prices WHERE tcgplayer_product_id = ?1 LIMIT 1")?
        .query_row([product_id], |r| r.get(0))
        .ok())
}

/// Look up TCGCSV products for a card and resolve each to (variant,
/// sub_type, product_id). Returns an empty vec when the card has no TCGCSV
/// products — caller handles the bootstrap fallback.
///
/// A set can have multiple TCGCSV groups linked via `tcgplayer_groups
/// .set_code` (the bridge): the regular run as role='primary', plus any
/// auxiliary runs (e.g. base1's group 1663 'shadowless'). The query
/// iterates products from every linked group, and the per-group entry
/// in `tcgcsv_sub_type_variant_map` reroutes ambiguous sub_type strings
/// (e.g. "Unlimited Holofoil" → unlimited_holo in the Unlimited group,
/// → shadowless_holo in the Shadowless group). See pokedumpster-5is.
fn variants_from_tcgcsv(
    conn: &Connection,
    card: &CardRow,
    sub_type_map: &SubTypeVariantMap,
) -> Result<Vec<VariantResolution>> {
    let mut stmt = conn.prepare(
        "SELECT tp.product_id, tp.collector_number, tp.derived_variant, tp.group_id \
           FROM tcgcsv_products tp \
           JOIN tcgplayer_groups g ON g.group_id = tp.group_id \
          WHERE g.set_code = ?1",
    )?;
    let raw: Vec<(i64, Option<String>, Option<String>, i64)> = stmt
        .query_map([&card.set_code], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let want = normalize_collector_number(&card.number);
    let mut out: Vec<VariantResolution> = Vec::new();
    for (product_id, raw_num, derived, group_id) in raw {
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
                // Base product: one variant per advertised sub_type,
                // resolved through the group-aware map.
                let mut p_stmt = conn.prepare(
                    "SELECT DISTINCT sub_type_name FROM prices \
                      WHERE tcgplayer_product_id = ?1",
                )?;
                let sub_types: Vec<String> = p_stmt
                    .query_map([product_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for sub in sub_types {
                    if let Some(variant) = sub_type_map.lookup(group_id, &sub) {
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
        // Skip the curated standalone-promo set. Those cards have no TCGCSV
        // group bridge, so this pass would only ever hand them a `normal`
        // placeholder and then deprecate the real printing that
        // synthesize_standalone_promos attaches afterward. That synth step
        // owns the set end to end. See pokedumpster (MCAP epic).
        let mut stmt = conn.prepare(
            "SELECT c.card_id, c.set_code, c.number, c.rarity, \
                    LOWER(c.name), LOWER(s.name), s.printed_total \
               FROM cards c JOIN sets s ON s.set_code = c.set_code \
              WHERE c.set_code <> ?1",
        )?;
        let rows = stmt.query_map([crate::standalone_promos::PROMO_SET_CODE], |r| {
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

    let sub_type_map = SubTypeVariantMap::load(conn)?;
    let cross_group_by_number = preload_cross_group_products(conn, &sub_type_map)?;

    // Pre-ensure every variant code the upcoming expansion might emit, so
    // the per-card insert loop doesn't pay a SELECT+INSERT roundtrip per
    // (card, variant) pair just to satisfy the FK on printings.variant.
    // Codes come from three sources:
    //   - cross-group MCAP/Deck-Exclusives preload (stamps + patterns);
    //   - overlay add-lists;
    //   - tcgcsv_products.derived_variant for own-group products
    //     (parse_stamp_tag at import time can produce arbitrary stamp_*
    //     codes that aren't in the V2 seed — McDonald's promo stamps,
    //     Worlds/Regionals staff stamps, etc.).
    {
        use std::collections::HashSet;
        let mut codes: HashSet<String> = HashSet::new();
        for products in cross_group_by_number.values() {
            for s in products {
                codes.insert(s.variant.clone());
            }
        }
        for ov in overrides {
            for add in &ov.add {
                codes.insert(add.clone());
            }
        }
        {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT derived_variant FROM tcgcsv_products \
                  WHERE derived_variant IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                codes.insert(row?);
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
        let mut tcgcsv_variants = variants_from_tcgcsv(conn, c, &sub_type_map)?;
        // Cross-group MCAP products: stamps and non-stamp overlays
        // (e.g. "Cosmo Holo"). The matcher resolves a candidate to
        // this card via either a set-name keyword (stamps) or a
        // card-name + printed_total fallback (non-stamp overlays and
        // stamps without a set keyword). Numbers alone are too
        // ambiguous — the same number appears in many sets.
        if let Some(candidates) = cross_group_by_number.get(&normalize_collector_number(&c.number))
        {
            // Promo-namespace numbers (SWSH028, SM42, …) normalize to an
            // alpha-prefixed token unique within a Black Star Promo set, so
            // they carry no "/total". For those the (already number-equal)
            // candidate matches on card name alone — there's no printed_total
            // to compare. Pure-digit numbers stay strict (name + total) since
            // the same number recurs across many sets.
            let promo_namespace = normalize_collector_number(&c.number)
                .chars()
                .any(|ch| ch.is_ascii_alphabetic());
            for product in candidates {
                let matches = match &product.set_keyword {
                    Some(kw) => c.set_name_lower.contains(kw),
                    None => {
                        product.card_name_lower == c.name_lower
                            && if product.set_total.is_some() {
                                product.set_total == c.printed_total
                            } else {
                                promo_namespace
                            }
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

    /// Open a fresh shared catalog with the production-default variants
    /// and sub_type_variant_map seeds applied. Mirrors what `pkdump setup`
    /// does before expand_all_printings, so tests exercise the same FK
    /// state production runs against.
    fn fresh_shared() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        pkdump_db::variants::reconcile(&mut conn).unwrap();
        pkdump_db::sub_type_map::reconcile(&mut conn).unwrap();
        (dir, conn)
    }

    /// Bridge a set to a TCGCSV group via the `tcgplayer_groups.set_code`
    /// bridge. Replaces the dropped `sets.tcgcsv_group_id` column.
    fn link_set_to_group(conn: &Connection, set_code: &str, group_id: i64) {
        link_set_to_group_with_role(conn, set_code, group_id, "primary");
    }

    fn link_set_to_group_with_role(conn: &Connection, set_code: &str, group_id: i64, role: &str) {
        conn.execute(
            "INSERT INTO tcgplayer_groups (group_id, set_code, name, fetched_at, role) \
             VALUES (?1, ?2, ?3, '2026-05-28', ?4)",
            rusqlite::params![group_id, set_code, format!("test:{set_code}"), role],
        )
        .unwrap();
    }

    fn seed() -> (tempfile::TempDir, Connection) {
        let (dir, conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) \
             VALUES ('me2pt5', 'Ascended Heroes', 'Mega Evolution')",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "me2pt5", 24541);
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
        let (_d, conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) \
             VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "sv3pt5", 23237);
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
        let (_d, mut conn) = fresh_shared();
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
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('zsv10pt5', 'Black Bolt', 'Scarlet & Violet', 86)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "zsv10pt5", 24325);
        // A second set with the same printed_total demonstrates the
        // keyword disambiguates beyond just the number match.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('rsv10pt5', 'White Flare', 'Scarlet & Violet', 86)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "rsv10pt5", 24326);
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
        let (_d, mut conn) = fresh_shared();
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
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('me2pt5', 'Ascended Heroes', 'Mega Evolution', 217)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "me2pt5", 30001);
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
    fn own_group_stamp_variant_is_pre_ensured_before_printing_insert() {
        // Regression test for the FK violation on data refresh after
        // import_products started writing stamp codes to derived_variant:
        // the bulk-ensure at function entry must cover own-group derived
        // codes (which now include arbitrary stamp_* codes from
        // parse_stamp_tag), not just cross-group / overlay-add codes.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('m22', 'McDonald\u{2019}s Promos 2022', 'Sword & Shield', 15)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "m22", 3150);
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('m22-1', 'm22', '1', 1, 'Charizard', 'Promo')",
            [],
        )
        .unwrap();
        // A product in the card's own group whose derived_variant is a
        // stamp code not present in the V2 seed. Mirrors what
        // import_products writes today for McDonald's-style stamp
        // products like "Charizard - 1 (1 Charizard Stamp)".
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (999001, 3150, 'Charizard - 1 (1 Charizard Stamp)', '1', 'stamp_1_charizard', '2026-05-24')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (999001, 'Holofoil', 'tcgplayer', 'market', 1.0, '2026-05-24')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let row: (String, Option<i64>) = conn
            .query_row(
                "SELECT variant, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'm22-1-stamp_1_charizard'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("stamp_1_charizard".into(), Some(999001)));
        // Variant was pre-ensured into the variants table so the FK
        // could be satisfied.
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM variants WHERE code = 'stamp_1_charizard'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn cross_group_deck_exclusive_non_foil_attaches_to_base_card() {
        // Zacian - 045/094 (group 1840 "Deck Exclusives", product 664005)
        // is a non-foil reprint of me2-45 from a Phantasmal Flames
        // preconstructed product (Build & Battle Box, etc.). Like the
        // cosmos-holo case, the matcher resolves it via card-name +
        // printed_total — but here the variant comes from the
        // `prices.sub_type_name` ("Normal") rather than from any
        // parenthetical pattern in the product name.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('me2', 'Phantasmal Flames', 'Mega Evolution', 94)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "me2", 24448);
        // Same card name + number but a different printed total — the
        // disambiguation must keep the deck exclusive off this card.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('swsh2', 'Rebel Clash', 'Sword & Shield', 192)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "swsh2", 9999);
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('me2-45', 'me2', '45', 45, 'Zacian', 'Rare')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('swsh2-45', 'swsh2', '45', 45, 'Zacian', 'Rare')",
            [],
        )
        .unwrap();
        // The deck-exclusive product itself — bare-name (no parenthetical
        // pattern), in group 1840.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (664005, 1840, 'Zacian - 045/094', '045/094', NULL, '2026-05-24')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (664005, 'Normal', 'tcgplayer', 'market', 0.5, '2026-05-24')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        // PFL Zacian gains a `normal` printing tied to the deck-exclusive product.
        let row: (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT sub_type_name, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'me2-45-normal'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (Some("Normal".into()), Some(664005)));

        // The Rebel Clash Zacian (same name+number, different printed
        // total) might still get a bootstrap `normal` placeholder, but it
        // must NOT be tied to the deck-exclusive product — the
        // set-total disambiguator should keep the cross-group product
        // away from this card.
        let leaked_with_product: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings \
                  WHERE printing_id = 'swsh2-45-normal' \
                    AND tcgplayer_product_id = 664005",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            leaked_with_product, 0,
            "deck exclusive must not bleed onto same-name+number card in another set"
        );
    }

    #[test]
    fn cross_group_ttbb_2024_cosmos_holo_attaches_with_combined_code() {
        // TTBB 2024 (group 23561) ships 10 Cosmos Holo specials whose
        // product names match the generic "(Cosmos Holo)" pattern. They
        // must be routed to `cosmos_holo_trick_or_trade`, not the bare
        // `cosmos_holo` code — the combined variant identifies them as
        // TTBB-exclusive on the chip. See pokedumpster-vz2.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('sv3', 'Obsidian Flames', 'Scarlet & Violet', 197)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "sv3", 22930);
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('sv3-136', 'sv3', '136', 136, 'Darkrai', 'Rare')",
            [],
        )
        .unwrap();
        // TTBB 2024 group, "(Cosmos Holo)" parenthetical. The tcgcsv
        // ingest already pre-derives `cosmos_holo` from the name; the
        // bridge must remap it for products in group 23561.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (568826, 23561, 'Darkrai (Cosmos Holo)', '136/197', 'cosmos_holo', '2026-05-26')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (568826, 'Holofoil', 'tcgplayer', 'market', 5.0, '2026-05-26')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let row: (String, Option<i64>) = conn
            .query_row(
                "SELECT variant, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'sv3-136-cosmos_holo_trick_or_trade'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("cosmos_holo_trick_or_trade".into(), Some(568826)),
            "TTBB 2024 Cosmos Holo must attach as cosmos_holo_trick_or_trade"
        );
        // The unrouted cosmos_holo must NOT appear — that would be the
        // ambiguous "is it TTBB or a regular cosmos holo" chip.
        let bare: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE printing_id = 'sv3-136-cosmos_holo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bare, 0, "must not double-attach as bare cosmos_holo");
    }

    #[test]
    fn cross_group_ttbb_plain_reprint_attaches_as_stamp_trick_or_trade() {
        // TTBB 2023 (group 23266) ships 30 bare-name reprints with the
        // Halloween Pikachu stamp on the artwork — no parenthetical
        // means none of the existing parsers triggered before. The
        // bridge's TTBB branch routes them to `stamp_trick_or_trade`.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('swsh11', 'Lost Origin', 'Sword & Shield', 196)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "swsh11", 17688);
        // Same-named card in another set with a different printed_total —
        // the set_total disambiguator must keep the TTBB stamp off it.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('xy11', 'Steam Siege', 'XY', 114)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('swsh11-16', 'swsh11', '16', 16, 'Phantump', 'Common')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('xy11-16', 'xy11', '16', 16, 'Phantump', 'Common')",
            [],
        )
        .unwrap();
        // The TTBB 2023 product — bare name, no derived_variant.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (515647, 23266, 'Phantump', '016/196', NULL, '2026-05-26')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (515647, 'Normal', 'tcgplayer', 'market', 0.25, '2026-05-26')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let row: (String, Option<i64>) = conn
            .query_row(
                "SELECT variant, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'swsh11-16-stamp_trick_or_trade'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("stamp_trick_or_trade".into(), Some(515647)));
        // The XY11 Phantump (same name+number, different printed_total)
        // must NOT receive the TTBB stamp.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings \
                  WHERE printing_id = 'xy11-16-stamp_trick_or_trade'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            leaked, 0,
            "TTBB stamp must not bleed onto same-name+number card in another set"
        );
    }

    #[test]
    fn ttbb_copyright_date_reprint_attaches_as_stamp_trick_or_trade() {
        // A few cards are reprinted in TTBB from two original print runs;
        // TCGCSV disambiguates by copyright line, e.g. "Gengar (2022
        // Copyright Date)" 066/196 (group 23266). It's still a Halloween
        // stamp, so it must bridge to the base card as stamp_trick_or_trade
        // rather than being skipped for carrying a parenthetical. A skipped
        // product leaves the bundle binder with a broken orphan slot whose
        // "full card details" link 404s (pokedumpster).
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('swsh11', 'Lost Origin', 'Sword & Shield', 196)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('swsh11-66', 'swsh11', '66', 66, 'Gengar', 'Rare')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (515661, 23266, 'Gengar (2022 Copyright Date)', '066/196', NULL, '2026-06-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (515661, 'Normal', 'tcgplayer', 'market', 0.5, '2026-06-01')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let row: (String, Option<i64>) = conn
            .query_row(
                "SELECT variant, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'swsh11-66-stamp_trick_or_trade'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("stamp_trick_or_trade".into(), Some(515661)));
    }

    #[test]
    fn base_set_charizard_splits_into_holo_first_ed_holo_and_shadowless_holo() {
        // base1 (Base Set) is the original WotC set with three real-world
        // print runs: Unlimited (with art-frame shadow), Shadowless (no
        // stamp, no shadow), and 1st Edition (stamped, no shadow).
        //
        // TCGCSV models this as TWO groups bridged to the same set:
        //   - 604  "Base Set"             — sub_types Normal/Holofoil
        //                                   (means Unlimited-with-shadow)
        //   - 1663 "Base Set (Shadowless)" — sub_types 1st Edition/
        //                                   Unlimited (the 1st-Ed vs no-
        //                                   stamp split lives in
        //                                   subTypeName only)
        //
        // Per pokedumpster-5is, the bridge from base1 to both groups goes
        // through tcgplayer_groups.set_code, and the group-aware
        // sub_type_variant_map routes each (group, sub_type) to a
        // distinct PokeDumpster variant code.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('base1', 'Base', 'Original', 102)",
            [],
        )
        .unwrap();
        // Primary bridge — group 604, the Unlimited-with-shadow run.
        link_set_to_group(&conn, "base1", 604);
        // Auxiliary bridge — group 1663, the Shadowless productId umbrella.
        link_set_to_group_with_role(&conn, "base1", 1663, "shadowless");

        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('base1-4', 'base1', '4', 4, 'Charizard', 'Holo Rare')",
            [],
        )
        .unwrap();

        // Group 604 (Unlimited): Charizard product 42382 with Holofoil only.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (42382, 604, 'Charizard - 4/102', '004/102', NULL, '2026-05-28')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (42382, 'Holofoil', 'tcgplayer', 'market', 200.0, '2026-05-28')",
            [],
        )
        .unwrap();

        // Group 1663 (Shadowless): Charizard product 106999 with
        // 1st Edition Holofoil AND Unlimited Holofoil sub_types.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (106999, 1663, 'Charizard', '004/102', NULL, '2026-05-28')",
            [],
        )
        .unwrap();
        for sub in ["1st Edition Holofoil", "Unlimited Holofoil"] {
            conn.execute(
                "INSERT INTO prices \
                   (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                 VALUES (106999, ?1, 'tcgplayer', 'market', 500.0, '2026-05-28')",
                [sub],
            )
            .unwrap();
        }

        expand_all_printings(&mut conn, &[]).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT variant, sub_type_name, tcgplayer_product_id FROM printings \
                  WHERE card_id = 'base1-4' AND deprecated_at IS NULL \
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
                    "first_ed_holo".into(),
                    Some("1st Edition Holofoil".into()),
                    Some(106999),
                ),
                ("holo".into(), Some("Holofoil".into()), Some(42382)),
                (
                    "shadowless_holo".into(),
                    Some("Unlimited Holofoil".into()),
                    Some(106999),
                ),
            ],
            "Base Set Charizard must split into holo (604, Unlimited-with-shadow), \
             first_ed_holo (1663 + 1st Edition Holofoil sub_type), and \
             shadowless_holo (1663 + Unlimited Holofoil sub_type)",
        );
    }

    #[test]
    fn jungle_non_holo_splits_into_first_ed_normal_and_unlimited_normal() {
        // Jungle (group 635) ships a single TCGCSV group whose non-holo
        // products price two sub_types side-by-side: "1st Edition" and
        // "Unlimited". The flat Rust sub_type_to_variant this replaces
        // dropped both because they lacked the "Holofoil" suffix, so
        // Jungle commons silently collapsed into a single fabricated
        // 'normal' printing. The group-aware map routes them correctly.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('base2', 'Jungle', 'Original', 64)",
            [],
        )
        .unwrap();
        link_set_to_group(&conn, "base2", 635);

        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('base2-46', 'base2', '46', 46, 'Diglett', 'Common')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (45146, 635, 'Diglett', '046/064', NULL, '2026-05-28')",
            [],
        )
        .unwrap();
        for sub in ["1st Edition", "Unlimited"] {
            conn.execute(
                "INSERT INTO prices \
                   (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
                 VALUES (45146, ?1, 'tcgplayer', 'market', 1.0, '2026-05-28')",
                [sub],
            )
            .unwrap();
        }

        expand_all_printings(&mut conn, &[]).unwrap();

        let mut variants: Vec<String> = conn
            .prepare(
                "SELECT variant FROM printings \
                  WHERE card_id = 'base2-46' AND deprecated_at IS NULL",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        variants.sort();
        assert_eq!(
            variants,
            vec![
                "first_ed_normal".to_string(),
                "unlimited_normal".to_string()
            ],
            "plain '1st Edition' and 'Unlimited' sub_types must route to \
             first_ed_normal and unlimited_normal (regression for the \
             dropped Rust sub_type_to_variant)",
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

    #[test]
    fn sv_promo_prerelease_resolves_to_base_card_but_bare_promo_does_not() {
        // Group 22872 ("SV: Scarlet & Violet Promo Cards" = svp) hosts two
        // kinds of product: base-set-numbered Prerelease promos that ARE
        // the base card with a stamp (e.g. "Sinistcha - 022/167
        // (Prerelease)" = sv6-22), and SVP-namespaced promos with bare
        // numbers (e.g. "Aegislash - 060") that are their own svp cards.
        // The cross-group scan must bridge the former and leave the latter
        // alone (pokedumpster-zq4).
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('sv6', 'Twilight Masquerade', 'Scarlet & Violet', 167)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('sv6-22', 'sv6', '22', 22, 'Sinistcha', 'Rare')",
            [],
        )
        .unwrap();
        // Base-set-numbered Prerelease promo — must bridge to sv6-22.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (553715, 22872, 'Sinistcha - 022/167 (Prerelease)', '022/167', NULL, '2026-05-31')",
            [],
        )
        .unwrap();
        // SVP-namespaced bare-number promo, same set/number space — must NOT
        // bridge (no /total, so set_total is None and the matcher rejects it).
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (526610, 22872, 'Aegislash - 060 (Prerelease)', '060', NULL, '2026-05-31')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let resolved: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'sv6-22-stamp_prerelease'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 553715);
        // The bare-number Aegislash promo has no base card here and must
        // not have manufactured a bridged printing against sv6.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE tcgplayer_product_id = 526610",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            leaked, 0,
            "bare-numbered SVP promo must not bridge to a base set"
        );
    }

    #[test]
    fn set_named_prerelease_promo_resolves_via_set_name_and_total() {
        // SV-era Build & Battle / Prerelease promos are named by their set
        // in MCAP — "Duraludon (Surging Sparks)" 129/191 — not by a stamp
        // suffix. They resolve to the base card as stamp_prerelease when the
        // parenthetical is a real set name AND the /total matches that set's
        // printed_total. A look-alike retailer parenthetical with the same
        // /total must NOT bridge (pokedumpster).
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('sv8', 'Surging Sparks', 'Scarlet & Violet', 191)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('sv8-129', 'sv8', '129', 129, 'Duraludon', 'Common'), \
                    ('sv8-130', 'sv8', '130', 130, 'Klefki', 'Common')",
            [],
        )
        .unwrap();
        // Set-named prerelease promo — must bridge to sv8-129.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (663168, 2374, 'Duraludon (Surging Sparks)', '129/191', NULL, '2026-06-01')",
            [],
        )
        .unwrap();
        // Same set's printed_total, but the parenthetical is a retailer tag,
        // not a set name — must NOT bridge.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (999001, 2374, 'Klefki - 130/191 (Toys R Us Promo)', '130/191', NULL, '2026-06-01')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let resolved: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'sv8-129-stamp_prerelease'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 663168);
        // The retailer-tagged Klefki must NOT be mistaken for a set-named
        // prerelease...
        let as_prerelease: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE printing_id = 'sv8-130-stamp_prerelease'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            as_prerelease, 0,
            "retailer tag must not become a prerelease stamp"
        );
        // ...it bridges as the generic `promo` variant instead (phase 2).
        let as_promo: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings WHERE printing_id = 'sv8-130-promo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(as_promo, 999001);
    }

    #[test]
    fn mcap_promo_namespace_reprint_bridges_to_black_star_promo_set() {
        // "Duraludon - SWSH028 (EB Games Exclusive)" carries a promo-set
        // number (no /total) and must bridge to the swshp card SWSH028 via
        // number + card-name (there's no printed_total to match). A
        // same-number card with a DIFFERENT name must not catch it
        // (pokedumpster MCAP epic, phase 3).
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('swshp', 'SWSH Black Star Promos', 'Sword & Shield', 307)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('swshp-SWSH028', 'swshp', 'SWSH028', 28, 'Duraludon', 'Promo'), \
                    ('swshp-SWSH099', 'swshp', 'SWSH099', 99, 'Pikachu', 'Promo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (624648, 2374, 'Duraludon - SWSH028 (EB Games Exclusive)', 'SWSH028', NULL, '2026-06-02')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let resolved: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'swshp-SWSH028-promo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 624648);
        // Pikachu (same set, different number/name) gains nothing.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE printing_id = 'swshp-SWSH099-promo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
    }

    #[test]
    fn mcap_foil_treatment_and_retailer_promo_bridge_to_base_card() {
        // MCAP numbered reprints: a named foil treatment bridges as that
        // foil variant; a pure retailer/event tag with no foil collapses
        // to the generic `promo` variant. Both resolve via card-name +
        // printed_total (pokedumpster MCAP epic, phase 2).
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('sv3pt5', '151', 'Scarlet & Violet', 165)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur', 'Common'), \
                    ('sv3pt5-9', 'sv3pt5', '9', 9, 'Blastoise ex', 'Double Rare')",
            [],
        )
        .unwrap();
        // Foil-treatment reprint (double paren: foil first, retailer second).
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (587935, 2374, 'Bulbasaur - 001/165 (Reverse Cosmos Holo) (Costco Exclusive)', '001/165', NULL, '2026-06-02')",
            [],
        )
        .unwrap();
        // Pure-retailer reprint, no special foil → generic promo.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (517558, 2374, 'Bulbasaur - 001/165 (Best Buy Exclusive)', '001/165', NULL, '2026-06-02')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let foil: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'sv3pt5-1-reverse_cosmos_holo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(foil, 587935);
        let promo: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'sv3pt5-1-promo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(promo, 517558);
    }

    #[test]
    fn mcap_prerelease_with_upstream_truncated_number_resolves_to_base_card() {
        // The last MCAP residual that wasn't blocked on Japanese ingest:
        // "Buck's Training - 130/146 (Prerelease)" (221176) and its
        // [Staff] sibling (532631) are the only two products in any
        // ingested group whose extendedData Number lost the "/total" the
        // name still carries. Bare "130" is pure digits, so it can't fall
        // through to the promo-namespace escape hatch either, and both
        // products stayed unmodeled. Drive the products through
        // `import_products` (not a hand-written row) so the number repair
        // is part of what's under test.
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('dp6', 'Legends Awakened', 'Diamond & Pearl', 146)",
            [],
        )
        .unwrap();
        // Decoy: another set that also has a card 130. The card-name gate
        // is what keeps the promo off it.
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('dp3', 'Secret Wonders', 'Diamond & Pearl', 132)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('dp6-130', 'dp6', '130', 130, 'Buck''s Training', 'Uncommon'), \
                    ('dp3-130', 'dp3', '130', 130, 'Torterra', 'Rare Holo')",
            [],
        )
        .unwrap();

        let ext = |number: &str| {
            vec![crate::tcgcsv::ExtendedDatum {
                name: "Number".into(),
                value: number.into(),
            }]
        };
        let products = vec![
            crate::tcgcsv::TcgProduct {
                product_id: 221176,
                group_id: 2374,
                name: "Buck's Training - 130/146 (Prerelease)".into(),
                image_url: None,
                url: None,
                image_count: 1,
                extended_data: ext("130"),
            },
            crate::tcgcsv::TcgProduct {
                product_id: 532631,
                group_id: 2374,
                name: "Buck's Training - 130/146 (Prerelease) [Staff]".into(),
                image_url: None,
                url: None,
                image_count: 1,
                extended_data: ext("130"),
            },
        ];
        crate::tcgcsv::import_products(&mut conn, &products, "2026-07-31").unwrap();
        conn.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (221176, 'Holofoil', 'tcgplayer', 'market', 12.0, '2026-07-31')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let prerelease: (Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT sub_type_name, tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'dp6-130-stamp_prerelease' AND deprecated_at IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(prerelease, (Some("Holofoil".into()), Some(221176)));
        let staff: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'dp6-130-stamp_prerelease_staff' AND deprecated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(staff, 532631);
        // The recovered total is what keeps the promo off the other set's
        // card 130 — and so does the card name.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM printings WHERE card_id = 'dp3-130' \
                   AND variant LIKE 'stamp_prerelease%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
    }

    #[test]
    fn blister_exclusive_cosmos_holo_resolves_to_base_card() {
        // Group 2289 ("Blister Exclusives") hosts Cosmos Holo promos
        // numbered against their base set (e.g. "Beheeyem - 62/99
        // (Cosmos Holo)" = a 99-card set). Bridges via the card-name +
        // printed_total fallback as `cosmos_holo` (pokedumpster-zq4).
        let (_d, mut conn) = fresh_shared();
        conn.execute(
            "INSERT INTO sets (set_code, name, series, printed_total) \
             VALUES ('bw4', 'Next Destinies', 'Black & White', 99)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('bw4-62', 'bw4', '62', 62, 'Beheeyem', 'Uncommon')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, fetched_at) \
             VALUES (91396, 2289, 'Beheeyem - 62/99 (Cosmos Holo)', '062/099', 'cosmos_holo', '2026-05-31')",
            [],
        )
        .unwrap();

        expand_all_printings(&mut conn, &[]).unwrap();

        let resolved: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings \
                  WHERE printing_id = 'bw4-62-cosmos_holo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 91396);
    }
}
