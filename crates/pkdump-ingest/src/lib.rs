//! `pkdump-ingest` — upstream catalog ingestion for PokeDumpster.
//!
//! Cache-population only: the pokemontcg.io API client, the
//! `PokemonTCG/pokemon-tcg-data` GitHub importer, and the TCGCSV price
//! pipeline (PLAN.md §2.6, §4). Runtime card lookups never touch the
//! network — see `architecture/CARD_DATA_ACCESS.md`.

mod error;
pub mod pokemon_tcg_data;
pub mod pokemontcg;
pub mod tcgcsv;

pub use error::{IngestError, Result};
