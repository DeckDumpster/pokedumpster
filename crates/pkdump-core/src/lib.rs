//! `pkdump-core` — domain types and pure logic for PokeDumpster.
//!
//! No IO lives here. The collection search query language's parser lives in
//! `query` (the SQL compiler that consumes its AST is in `pkdump-db`); see
//! architecture/SEARCH_QUERY_LANGUAGE.md.

pub mod card;
pub mod import;
pub mod query;
pub mod variant;

pub use card::number_sortable;

/// Crate version string, surfaced by the CLI and server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
