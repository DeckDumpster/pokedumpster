//! PokeDumpster-native CSV import.
//!
//! The shape the pkmn.gg Tampermonkey export userscript writes. Optimised
//! for Pokémon: a `variant` column carrying the PokeDumpster variant code
//! directly (so the long-tail treatments survive a round-trip the way the
//! ManaBox `Foil` column can't), a `ptcgo_code` column as a backup when
//! `set_code` is unknown, and a `source` column so the import batch records
//! where the rows came from.
//!
//! Columns (case-insensitive, order-tolerant):
//!
//! ```text
//! set_code, ptcgo_code, number, variant, condition, language,
//! quantity, purchase_price, currency, source, notes
//! ```
//!
//! Required: `number`, `variant`, and at least one of `set_code` /
//! `ptcgo_code`. Everything else has a sensible default.

use std::collections::HashMap;

use super::{ImportError, ParsedRow, Result, map_condition, map_language};

/// Parse a PokeDumpster-native CSV into one `ParsedRow` per copy.
pub fn parse(input: &str) -> Result<Vec<ParsedRow>> {
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
    // First header in `names` that is present.
    let first = |names: &[&str]| names.iter().find_map(|n| headers.get(*n).copied());

    let col_number = first(&["number", "collector number"])
        .ok_or_else(|| ImportError::MissingColumn("number".into()))?;
    let col_variant =
        first(&["variant"]).ok_or_else(|| ImportError::MissingColumn("variant".into()))?;
    let col_set_code = first(&["set_code", "set code"]);
    let col_ptcgo = first(&["ptcgo_code", "ptcgo code"]);
    if col_set_code.is_none() && col_ptcgo.is_none() {
        return Err(ImportError::MissingColumn("set_code / ptcgo_code".into()));
    }
    let col_quantity = first(&["quantity", "qty"]);
    let col_condition = first(&["condition"]);
    let col_language = first(&["language"]);
    let col_price = first(&["purchase_price", "purchase price", "price"]);
    let col_notes = first(&["notes"]);

    let mut rows = Vec::new();
    for (rec_i, record) in reader.records().enumerate() {
        let record = record?;
        let line = rec_i + 2; // 1-based; +1 for the consumed header row.
        let get = |c: Option<usize>| c.and_then(|i| record.get(i)).unwrap_or("").trim();

        let number = get(Some(col_number)).to_string();
        if number.is_empty() {
            return Err(ImportError::BadRow {
                line,
                message: "empty number".into(),
            });
        }
        let variant = get(Some(col_variant)).to_string();
        if variant.is_empty() {
            return Err(ImportError::BadRow {
                line,
                message: "empty variant".into(),
            });
        }

        // Prefer set_code if present; ptcgo_code is a fallback. The resolver
        // already tries set_code → ptcgo_code → name, so passing either works.
        let set_code = get(col_set_code);
        let ptcgo = get(col_ptcgo);
        let set_hint = if !set_code.is_empty() {
            set_code
        } else {
            ptcgo
        };
        if set_hint.is_empty() {
            return Err(ImportError::BadRow {
                line,
                message: "empty set_code and ptcgo_code".into(),
            });
        }

        let quantity: u32 = {
            let raw = get(col_quantity);
            if raw.is_empty() {
                1
            } else {
                raw.parse().map_err(|_| ImportError::BadRow {
                    line,
                    message: format!("non-numeric quantity '{raw}'"),
                })?
            }
        };

        let purchase_price = {
            let raw = get(col_price).trim_start_matches('$');
            if raw.is_empty() {
                None
            } else {
                Some(raw.parse().map_err(|_| ImportError::BadRow {
                    line,
                    message: format!("non-numeric purchase_price '{raw}'"),
                })?)
            }
        };

        let condition = map_condition(get(col_condition));
        let language = map_language(get(col_language));
        let notes = get(col_notes);
        let tags = if notes.is_empty() {
            Vec::new()
        } else {
            // Free-form notes ride along as a single-element tag set so they
            // survive into collection.tags (the importer has no notes lane).
            vec![notes.to_string()]
        };

        for _ in 0..quantity {
            rows.push(ParsedRow {
                source_line: line,
                set_hint: set_hint.to_string(),
                set_name: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
set_code,ptcgo_code,number,variant,condition,language,quantity,purchase_price
sv3pt5,MEW,6,normal,Near Mint,en,2,1.50
sv3pt5,MEW,199,reverse_holo,Lightly Played,en,1,42.00
,SSP,57,holo,,en,,";

    #[test]
    fn expands_quantity_one_row_per_copy() {
        let rows = parse(SAMPLE).unwrap();
        assert_eq!(rows.len(), 4); // 2 + 1 + 1 default

        assert_eq!(rows[0].set_hint, "sv3pt5"); // set_code wins when present
        assert_eq!(rows[0].number, "6");
        assert_eq!(rows[0].variant, "normal");
        assert_eq!(rows[0].condition, "Near Mint");
        assert_eq!(rows[0].purchase_price, Some(1.50));
        assert_eq!(rows[1], rows[0]); // second copy identical

        assert_eq!(rows[2].variant, "reverse_holo");
        assert_eq!(rows[2].condition, "Lightly Played");

        // ptcgo_code fallback, missing quantity → 1, blank price → None.
        assert_eq!(rows[3].set_hint, "SSP");
        assert_eq!(rows[3].purchase_price, None);
    }

    #[test]
    fn missing_variant_column_is_an_error() {
        let csv = "set_code,number\nsv3pt5,6";
        assert!(matches!(parse(csv), Err(ImportError::MissingColumn(_))));
    }

    #[test]
    fn missing_set_columns_is_an_error() {
        let csv = "number,variant\n6,normal";
        assert!(matches!(parse(csv), Err(ImportError::MissingColumn(_))));
    }
}
