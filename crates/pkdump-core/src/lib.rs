//! `pkdump-core` — domain types and pure logic for PokeDumpster.
//!
//! No IO lives here. Card/printing/variant types, the three-layer variant
//! expansion pipeline, and the collection query-language compiler are added
//! by later M1/M2 tasks (see PLAN.md §2.1, §4).

/// Crate version string, surfaced by the CLI and server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
