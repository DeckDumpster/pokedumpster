//! `pkdump-core` — domain types and pure logic for PokeDumpster.
//!
//! No IO lives here. The collection query-language compiler and the
//! three-layer variant expansion pipeline are added by later M1 tasks
//! (see PLAN.md §2.1, §4).

pub mod card;

pub use card::number_sortable;

/// Crate version string, surfaced by the CLI and server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
