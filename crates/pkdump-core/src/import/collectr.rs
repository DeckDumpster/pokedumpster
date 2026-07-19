//! Collectr CSV import (PLAN.md §9). Collectr (getcollectr.com) exports a
//! single file mixing single cards and sealed products across multiple
//! game categories. This parser splits that file along the garden wall:
//! singles become [`ParsedRow`]s, sealed products become
//! [`ParsedSealedRow`]s, and everything else (non-Pokémon rows) is reported
//! as skipped rather than silently dropped.
//!
//! Collectr columns: `Portfolio Name, Category, Set, Product Name,
//! Card Number, Rarity, Variance, Grade, Card Condition, Average Cost Paid,
//! Quantity, Market Price (As of <date>), Price Override, Watchlist,
//! Date Added, Notes`.
//!
//! The single/sealed discriminator is the **Card Number**: sealed products
//! carry none. Market Price is the platform's own valuation and is ignored —
//! PokeDumpster prices from its own catalog.

use std::collections::HashMap;

use super::{
    ImportError, ParsedRow, ParsedSealedRow, Result, manabox, map_condition, map_language,
};

/// A row skipped during a Collectr import, with a reason — surfaced in the
/// preview so nothing disappears without explanation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SkippedRow {
    pub source_line: usize,
    pub category: String,
    pub name: String,
    pub reason: String,
}

/// The split result of parsing a Collectr export.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CollectrParsed {
    pub singles: Vec<ParsedRow>,
    pub sealed: Vec<ParsedSealedRow>,
    pub skipped: Vec<SkippedRow>,
}

/// Parse a Collectr collection export, routing each row to singles, sealed,
/// or skipped.
pub fn parse(input: &str) -> Result<CollectrParsed> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(input.as_bytes());

    let headers: HashMap<String, usize> = reader
        .headers()?
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_lowercase(), i))
        .collect();
    // The market-price header carries a date suffix, so match by prefix.
    let idx = |name: &str| headers.get(name).copied();

    let col_category =
        idx("category").ok_or_else(|| ImportError::MissingColumn("Category".into()))?;
    let col_set = idx("set").ok_or_else(|| ImportError::MissingColumn("Set".into()))?;
    let col_name =
        idx("product name").ok_or_else(|| ImportError::MissingColumn("Product Name".into()))?;
    let col_number =
        idx("card number").ok_or_else(|| ImportError::MissingColumn("Card Number".into()))?;
    let col_variance = idx("variance");
    let col_condition = idx("card condition");
    let col_cost = idx("average cost paid");
    let col_quantity = idx("quantity");
    let col_date = idx("date added");
    let col_notes = idx("notes");

    let mut out = CollectrParsed {
        singles: Vec::new(),
        sealed: Vec::new(),
        skipped: Vec::new(),
    };

    for (rec_i, record) in reader.records().enumerate() {
        let record = record?;
        let line = rec_i + 2; // 1-based; +1 for the consumed header row.
        let get = |c: Option<usize>| c.and_then(|i| record.get(i)).unwrap_or("").trim();

        let category = get(Some(col_category)).to_string();
        let name = get(Some(col_name)).to_string();
        let set = get(Some(col_set));

        // PokeDumpster is a Pokémon tracker — drop other games, but say so.
        if !category.eq_ignore_ascii_case("pokemon") {
            out.skipped.push(SkippedRow {
                source_line: line,
                category: category.clone(),
                name,
                reason: format!("non-Pokémon category '{category}'"),
            });
            continue;
        }
        if set.is_empty() {
            out.skipped.push(SkippedRow {
                source_line: line,
                category,
                name,
                reason: "empty Set".into(),
            });
            continue;
        }

        let quantity = {
            let raw = get(col_quantity);
            if raw.is_empty() {
                1
            } else {
                raw.parse().map_err(|_| ImportError::BadRow {
                    line,
                    message: format!("non-numeric Quantity '{raw}'"),
                })?
            }
        };
        let condition = map_condition(get(col_condition));
        let purchase_price = parse_price(get(col_cost));
        let date_added = get(col_date);

        // The garden wall: a blank Card Number means a sealed product.
        let number_raw = get(Some(col_number));
        if number_raw.is_empty() {
            out.sealed.push(ParsedSealedRow {
                source_line: line,
                name,
                set_hint: set.to_string(),
                set_name: Some(set.to_string()),
                category_hint: None,
                quantity,
                condition,
                purchase_price,
                purchase_date: (!date_added.is_empty()).then(|| date_added.to_string()),
                notes: clean_notes(get(col_notes)),
            });
            continue;
        }

        let variant = manabox::map_variant(get(col_variance));
        let acquired_at = (!date_added.is_empty()).then(|| to_rfc3339(date_added));
        let single = ParsedRow {
            source_line: line,
            set_hint: set.to_string(),
            set_name: Some(set.to_string()),
            name: (!name.is_empty()).then(|| name.clone()),
            number: normalize_number(number_raw),
            variant,
            condition,
            language: map_language(""), // Collectr carries no language column.
            purchase_price,
            acquired_at,
            tags: Vec::new(),
        };
        // Singles are strictly one row per physical card.
        for _ in 0..quantity {
            out.singles.push(single.clone());
        }
    }
    Ok(out)
}

/// Collectr writes collector numbers as `number/total` (`221/217`),
/// occasionally with a set-abbrev prefix (`SVP 176`), and zero-padded to
/// three digits (`090/086`). Reduce to the bare, unpadded collector number
/// the catalog stores.
fn normalize_number(raw: &str) -> String {
    let s = raw.trim();
    let s = s.split('/').next().unwrap_or(s).trim();
    // "SVP 176" → "176": drop an alphabetic prefix before a numeric tail.
    let s = if let Some((prefix, tail)) = s.rsplit_once(' ')
        && !tail.is_empty()
        && tail.chars().all(|c| c.is_ascii_digit())
        && prefix.chars().any(|c| c.is_ascii_alphabetic())
    {
        tail
    } else {
        s
    };
    // "090" → "90": strip zero-padding, but only on all-digit tokens.
    super::normalize_collector_number(s)
}

/// Parse a Collectr price cell. Empty → `None`; a literal `0` is kept as
/// `Some(0.0)` (a recorded zero cost). Tolerant of junk → `None` so one bad
/// cell never fails the whole import.
fn parse_price(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    s.parse().ok()
}

/// A bare `YYYY-MM-DD` date → an RFC-3339 midnight-UTC timestamp, so it sorts
/// alongside the timestamps the rest of the app writes. Non-date input is
/// passed through unchanged.
fn to_rfc3339(date: &str) -> String {
    let d = date.trim();
    if d.len() == 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-' {
        format!("{d}T00:00:00Z")
    } else {
        d.to_string()
    }
}

/// Collectr sometimes leaves stray `; ` separators in the Notes cell. Trim
/// them; an all-separator cell becomes `None`.
fn clean_notes(raw: &str) -> Option<String> {
    let s = raw
        .trim()
        .trim_matches(|c: char| c == ';' || c.is_whitespace());
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative slice of a real Collectr export: a Lorcana row (must
    // be skipped), singles with slashed/prefixed numbers and both variances,
    // and sealed products with quantities.
    const SAMPLE: &str = "\
Portfolio Name,Category,Set,Product Name,Card Number,Rarity,Variance,Grade,Card Condition,Average Cost Paid,Quantity,Market Price (As of 2026-07-17),Price Override,Watchlist,Date Added,Notes
Main,Lorcana,Disney Lorcana Promo Cards,Elsa - The Fifth Spirit,6,Promo,Holofoil,Ungraded,Near Mint,0.0000,1,7.12,0,false,2026-01-24,
Main,Pokemon,Ascended Heroes,Budew,221/217,Illustration Rare,Holofoil,Ungraded,Near Mint,0,1,7.19,0,false,2026-04-14,
Main,Pokemon,Scarlet & Violet Promo,Umbreon ex - 176,SVP 176,Promo,Holofoil,Ungraded,Near Mint,0,2,41.43,0,false,2026-03-30,
Main,Pokemon,Mega Evolution Promos,Meloetta,026,Illustration Rare,Normal,Ungraded,Near Mint,0,1,14.69,0,false,2026-04-02,
Sealed Pokemon TCG,Pokemon,Journey Together,Journey Together Elite Trainer Box,,,Normal,Ungraded,Near Mint,75.0000,1,135.35,0,false,2026-02-28,
Sealed Pokemon TCG,Pokemon,Chaos Rising,Chaos Rising Booster Box,,,Normal,Ungraded,Near Mint,200.0000,2,200.26,0,false,2026-03-23,; ";

    #[test]
    fn splits_singles_sealed_and_skips_non_pokemon() {
        let out = parse(SAMPLE).unwrap();

        // Lorcana row is skipped, not imported.
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].category, "Lorcana");

        // Singles: Budew(x1) + Umbreon(x2, expanded) + Meloetta(x1) = 4 copies.
        assert_eq!(out.singles.len(), 4);
        // Sealed: 2 distinct products (quantities kept, not expanded).
        assert_eq!(out.sealed.len(), 2);
    }

    #[test]
    fn normalizes_numbers_and_variants() {
        let out = parse(SAMPLE).unwrap();
        let budew = &out.singles[0];
        assert_eq!(budew.number, "221"); // 221/217 → 221
        assert_eq!(budew.variant, "holo"); // Holofoil → holo
        assert_eq!(budew.set_hint, "Ascended Heroes");
        assert_eq!(budew.acquired_at.as_deref(), Some("2026-04-14T00:00:00Z"));

        let umbreon = &out.singles[1];
        assert_eq!(umbreon.number, "176"); // SVP 176 → 176
        assert_eq!(out.singles[1], out.singles[2]); // both expanded copies identical

        let meloetta = &out.singles[3];
        assert_eq!(meloetta.number, "26"); // 026 → 26 (zero-padding stripped)
        assert_eq!(meloetta.variant, "normal"); // Variance Normal → normal
    }

    #[test]
    fn strips_zero_padding_but_not_alphanumeric() {
        // Collectr zero-pads to three digits; the catalog stores unpadded.
        assert_eq!(normalize_number("090/086"), "90");
        assert_eq!(normalize_number("009"), "9");
        assert_eq!(normalize_number("SVP 176"), "176");
        // Alphanumeric collector numbers must survive untouched.
        assert_eq!(normalize_number("GG01"), "GG01");
        assert_eq!(normalize_number("SWSH123"), "SWSH123");
        assert_eq!(normalize_number("TG01/TG30"), "TG01");
    }

    #[test]
    fn sealed_keeps_quantity_and_cleans_notes() {
        let out = parse(SAMPLE).unwrap();
        let etb = &out.sealed[0];
        assert_eq!(etb.name, "Journey Together Elite Trainer Box");
        assert_eq!(etb.quantity, 1);
        assert_eq!(etb.purchase_price, Some(75.0));
        assert_eq!(etb.purchase_date.as_deref(), Some("2026-02-28"));

        let box_ = &out.sealed[1];
        assert_eq!(box_.quantity, 2); // not expanded
        assert_eq!(box_.notes, None); // stray "; " stripped away
    }
}
