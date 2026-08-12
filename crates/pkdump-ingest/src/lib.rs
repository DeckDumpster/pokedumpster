//! `pkdump-ingest` — upstream catalog ingestion for PokeDumpster.
//!
//! Cache-population only: the pokemontcg.io API client, the
//! `PokemonTCG/pokemon-tcg-data` GitHub importer, and the TCGCSV price
//! pipeline (PLAN.md §2.6, §4). Runtime card lookups never touch the
//! network — see `architecture/CARD_DATA_ACCESS.md`.

pub mod coverage;
mod error;
pub mod japan;
pub mod landing;
pub mod overrides;
pub mod pokemon_tcg_data;
pub mod pokemontcg;
pub mod set_discovery;
pub mod standalone_promos;
pub mod symbols;
pub mod tcgcsv;
/// The in-process fake upstream, behind the `test-support` feature — see
/// [`test_upstream`]. It lives in the library rather than in `tests/` because
/// `pkdump-cli`'s raw-coverage gate drives a whole acquisition phase, whose
/// clients it never constructs and so cannot point anywhere itself.
#[cfg(any(test, feature = "test-support"))]
pub mod test_upstream;
pub mod upstream;

pub use error::{IngestError, Result};
