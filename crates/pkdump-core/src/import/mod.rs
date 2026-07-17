//! CSV import parsers (PLAN.md §9). Each format parses a CSV string into
//! `ParsedRow`s — one per physical copy — which the catalog resolver in
//! `pkdump-db` then matches against the card catalogue. Pure: no IO.

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
