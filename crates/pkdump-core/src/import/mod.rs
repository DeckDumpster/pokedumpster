//! CSV import parsers (PLAN.md §9). Each format parses a CSV string into
//! `ParsedRow`s — one per physical copy — which the catalog resolver in
//! `pkdump-db` then matches against the card catalogue. Pure: no IO.

pub mod manabox;
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
    /// Collector number, verbatim.
    pub number: String,
    /// PokeDumpster variant code (the flat enum) the foil column mapped to.
    pub variant: String,
    /// Grading condition, mapped to PokeDumpster's five-tier scale.
    pub condition: String,
    pub language: String,
    pub purchase_price: Option<f64>,
    /// Misprint / altered flags, merged.
    pub tags: Vec<String>,
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
