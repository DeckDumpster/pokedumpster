//! CSV import parsers (PLAN.md §9). Each format parses a CSV string into
//! `ParsedRow`s — one per physical copy — which the catalog resolver in
//! `pkdump-db` then matches against the card catalogue. Pure: no IO.

pub mod manabox;

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
