//! ManaBox CSV import (PLAN.md §9.1) — the primary interchange format.
//!
//! ManaBox export columns: `Set code, Set name, Collector number, Foil,
//! Rarity, Quantity, ManaBox ID, Scryfall ID, Purchase price, Misprint,
//! Altered, Condition, Language, Purchase price currency`.

use std::collections::HashMap;

use super::{ImportError, ParsedRow, Result, is_true, map_condition, map_language};

/// Parse a ManaBox collection-export CSV into one `ParsedRow` per copy.
pub fn parse(input: &str) -> Result<Vec<ParsedRow>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(input.as_bytes());

    // Resolve columns by (lower-cased) header name, tolerant of reordering.
    let headers: HashMap<String, usize> = reader
        .headers()?
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_lowercase(), i))
        .collect();
    let idx = |name: &str| headers.get(name).copied();

    let col_number =
        idx("collector number").ok_or_else(|| ImportError::MissingColumn("Collector number".into()))?;
    let col_set_code = idx("set code");
    let col_set_name = idx("set name");
    if col_set_code.is_none() && col_set_name.is_none() {
        return Err(ImportError::MissingColumn("Set code / Set name".into()));
    }
    let col_foil = idx("foil");
    let col_quantity = idx("quantity");
    let col_condition = idx("condition");
    let col_language = idx("language");
    let col_price = idx("purchase price");
    let col_misprint = idx("misprint");
    let col_altered = idx("altered");

    let mut rows = Vec::new();
    for (rec_i, record) in reader.records().enumerate() {
        let record = record?;
        let line = rec_i + 2; // 1-based; +1 for the consumed header row.
        let get = |c: Option<usize>| c.and_then(|i| record.get(i)).unwrap_or("").trim();

        let number = get(Some(col_number)).to_string();
        if number.is_empty() {
            return Err(ImportError::BadRow { line, message: "empty Collector number".into() });
        }

        let set_code = get(col_set_code);
        let set_name = get(col_set_name);
        let set_hint = if set_code.is_empty() { set_name } else { set_code };
        if set_hint.is_empty() {
            return Err(ImportError::BadRow {
                line,
                message: "empty Set code and Set name".into(),
            });
        }

        let quantity: u32 = {
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

        let purchase_price = {
            let raw = get(col_price);
            if raw.is_empty() {
                None
            } else {
                Some(raw.parse().map_err(|_| ImportError::BadRow {
                    line,
                    message: format!("non-numeric Purchase price '{raw}'"),
                })?)
            }
        };

        let variant = map_variant(get(col_foil));
        let condition = map_condition(get(col_condition));
        let language = map_language(get(col_language));
        let mut tags = Vec::new();
        if is_true(get(col_misprint)) {
            tags.push("misprint".to_string());
        }
        if is_true(get(col_altered)) {
            tags.push("altered".to_string());
        }

        for _ in 0..quantity {
            rows.push(ParsedRow {
                source_line: line,
                set_hint: set_hint.to_string(),
                set_name: (!set_name.is_empty()).then(|| set_name.to_string()),
                number: number.clone(),
                variant: variant.clone(),
                condition: condition.clone(),
                language: language.clone(),
                purchase_price,
                tags: tags.clone(),
            });
        }
    }
    Ok(rows)
}

/// ManaBox `Foil` value → PokeDumpster variant code.
fn map_variant(foil: &str) -> String {
    match foil.trim().to_lowercase().as_str() {
        "" | "normal" | "non-foil" | "nonfoil" => "normal",
        "foil" | "holofoil" | "holo" | "etched" => "holo",
        "reverseholofoil" | "reverse_holo" | "reverse" | "reverse holo" => "reverse_holo",
        _ => "normal",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Set code,Set name,Collector number,Foil,Rarity,Quantity,ManaBox ID,Scryfall ID,Purchase price,Misprint,Altered,Condition,Language,Purchase price currency
SV3PT5,151,6,normal,Common,2,m1,s1,0.25,false,false,near_mint,en,USD
SV3PT5,151,199,reverseHolofoil,Illustration Rare,1,m2,s2,42.00,true,false,lightly_played,en,USD";

    #[test]
    fn expands_quantity_one_row_per_copy() {
        let rows = parse(SAMPLE).unwrap();
        assert_eq!(rows.len(), 3); // 2 + 1

        let first = &rows[0];
        assert_eq!(first.set_hint, "SV3PT5");
        assert_eq!(first.set_name.as_deref(), Some("151"));
        assert_eq!(first.number, "6");
        assert_eq!(first.variant, "normal");
        assert_eq!(first.condition, "Near Mint");
        assert_eq!(first.language, "English");
        assert_eq!(first.purchase_price, Some(0.25));
        assert!(first.tags.is_empty());
        assert_eq!(first.source_line, 2);
        assert_eq!(rows[1], *first); // the second copy is identical

        let rare = &rows[2];
        assert_eq!(rare.variant, "reverse_holo");
        assert_eq!(rare.condition, "Lightly Played");
        assert_eq!(rare.tags, vec!["misprint".to_string()]);
        assert_eq!(rare.source_line, 3);
    }

    #[test]
    fn missing_quantity_defaults_to_one() {
        let csv = "Set code,Collector number,Foil\nSV1,1,foil";
        let rows = parse(csv).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].variant, "holo");
    }

    #[test]
    fn missing_required_column_is_an_error() {
        let csv = "Set code,Foil\nSV1,foil";
        assert!(matches!(parse(csv), Err(ImportError::MissingColumn(_))));
    }

    #[test]
    fn non_numeric_quantity_is_a_bad_row() {
        let csv = "Set code,Collector number,Quantity\nSV1,1,lots";
        match parse(csv) {
            Err(ImportError::BadRow { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected BadRow, got {other:?}"),
        }
    }
}
