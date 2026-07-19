//! CSV import parsers (PLAN.md §9). Each format parses a CSV string into
//! `ParsedRow`s — one per physical copy — which the catalog resolver in
//! `pkdump-db` then matches against the card catalogue. Pure: no IO.

pub mod collectr;
pub mod manabox;
pub mod pokedumpster;
pub mod tcgplayer;

use serde::Serialize;

/// A single physical copy parsed from an import file, before catalog
/// resolution. `Quantity` columns are already expanded 1:1 — no aggregation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParsedRow {
    /// 1-based source line (counting the header), for preview diagnostics.
    pub source_line: usize,
    /// Set code (or ptcgo code) as written in the file.
    pub set_hint: String,
    /// Set name as written, used as a resolution fallback. May be empty.
    pub set_name: Option<String>,
    /// Card name as written, used only as a global-search fallback when the
    /// set+number lookup fails (e.g. Collectr's "Miscellaneous Cards &
    /// Products" catch-all, where the stated set doesn't resolve). `None`
    /// when the format carries no reliable name column.
    pub name: Option<String>,
    /// Collector number, verbatim.
    pub number: String,
    /// PokeDumpster variant code (the flat enum) the foil column mapped to.
    pub variant: String,
    /// Grading condition, mapped to PokeDumpster's five-tier scale.
    pub condition: String,
    pub language: String,
    pub purchase_price: Option<f64>,
    /// Acquisition date as written (ISO `YYYY-MM-DD`), if the source carries
    /// one (e.g. Collectr's "Date Added"). `None` → the importer stamps now.
    pub acquired_at: Option<String>,
    /// Misprint / altered flags, merged.
    pub tags: Vec<String>,
}

/// A sealed product parsed from an import file, before catalog resolution.
///
/// Deliberately *separate* from [`ParsedRow`]: sealed products and single
/// cards are treated apart end-to-end (the garden wall). Unlike singles —
/// which are strictly one row per physical card — sealed items keep a
/// `quantity` because `sealed_collection` aggregates by count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParsedSealedRow {
    /// 1-based source line (counting the header), for preview diagnostics.
    pub source_line: usize,
    /// Product name as written in the file (matched against the catalog).
    pub name: String,
    /// Set code (or name) as written, used to narrow the catalog match.
    pub set_hint: String,
    /// Set name as written, a resolution fallback. May be empty.
    pub set_name: Option<String>,
    /// Optional product-category hint (e.g. `booster_box`), if the source
    /// carries one. Collectr does not, so this is usually `None`.
    pub category_hint: Option<String>,
    /// Number of sealed items — kept, not expanded 1:1.
    pub quantity: u32,
    pub condition: String,
    /// Per-unit purchase price (Collectr's "Average Cost Paid").
    pub purchase_price: Option<f64>,
    /// Acquisition date as written (ISO `YYYY-MM-DD`), if present.
    pub purchase_date: Option<String>,
    pub notes: Option<String>,
}

/// Anything that can go wrong parsing an import file.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("CSV parse error: {0}")]
    Csv(#[from] csv::Error),
    #[error("missing required column: {0}")]
    MissingColumn(String),
    #[error("line {line}: {message}")]
    BadRow { line: usize, message: String },
}

pub type Result<T> = std::result::Result<T, ImportError>;

// --- Shared column-value mappings, used by every format parser. ---

/// Map a grading condition to PokeDumpster's five-tier scale — the only
/// values the `collection.condition` CHECK constraint accepts. Tolerant of
/// ManaBox snake-case (`near_mint`), TCGplayer title-case (`Near Mint`),
/// and a trailing ` Foil` suffix TCGplayer sometimes appends.
pub(crate) fn map_condition(raw: &str) -> String {
    let c = raw.trim().to_lowercase();
    let c = c.strip_suffix(" foil").unwrap_or(&c);
    match c.replace([' ', '-'], "_").as_str() {
        "excellent" | "good" | "light_played" | "lightly_played" | "lp" => "Lightly Played",
        "played" | "moderately_played" | "mp" => "Moderately Played",
        "heavily_played" | "hp" => "Heavily Played",
        "poor" | "damaged" | "dmg" => "Damaged",
        // "mint", "near_mint", "nm", "" and anything unrecognised.
        _ => "Near Mint",
    }
    .to_string()
}

/// Map a language value to a display language. English-only v1, but a
/// non-English value is preserved so a re-export round-trips.
pub(crate) fn map_language(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "" | "en" | "english" => "English".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
                None => "English".to_string(),
            }
        }
    }
}

/// Whether a boolean-ish CSV cell reads as true.
pub(crate) fn is_true(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "true" | "1" | "yes")
}

/// Reduce a purely-numeric collector number to its canonical unpadded form,
/// so a zero-padded source token (Collectr's `090`) matches the catalog's
/// stored `90`. Only all-ASCII-digit tokens are stripped — alphanumeric
/// numbers (`SWSH123`, `GG01`, `TG01`, `SVP`) are returned unchanged — and at
/// least one digit is always kept (`000` → `0`). The canonical normalizer,
/// shared by the Collectr parser and the catalog resolver so both sides of a
/// `number =` lookup agree.
pub fn normalize_collector_number(raw: &str) -> String {
    let s = raw.trim();
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        let stripped = s.trim_start_matches('0');
        return if stripped.is_empty() {
            "0".to_string()
        } else {
            stripped.to_string()
        };
    }
    s.to_string()
}

#[cfg(test)]
mod number_tests {
    use super::normalize_collector_number;

    #[test]
    fn strips_leading_zeros_only_on_all_digit_tokens() {
        // Zero-padded numerics collapse to the catalog's unpadded form.
        assert_eq!(normalize_collector_number("090"), "90");
        assert_eq!(normalize_collector_number("009"), "9");
        assert_eq!(normalize_collector_number("026"), "26");
        assert_eq!(normalize_collector_number("176"), "176");
        // All-zero keeps a single digit rather than collapsing to empty.
        assert_eq!(normalize_collector_number("000"), "0");
        // Alphanumeric collector numbers are left exactly as written.
        assert_eq!(normalize_collector_number("GG01"), "GG01");
        assert_eq!(normalize_collector_number("TG01"), "TG01");
        assert_eq!(normalize_collector_number("SWSH123"), "SWSH123");
        assert_eq!(normalize_collector_number("SVP"), "SVP");
    }
}
