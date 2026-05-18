//! `pkdump-core` — domain types and pure logic for PokeDumpster.
//!
//! No IO lives here. The collection query-language compiler is added by a
//! later M2 task (see PLAN.md §2.1).

pub mod card;
pub mod import;
pub mod variant;

pub use card::number_sortable;

/// Crate version string, surfaced by the CLI and server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
