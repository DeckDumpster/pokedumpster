//! `pkdump-lake` — the raw landing zone.
//!
//! Every byte PokeDumpster fetches from an upstream lands here, immutably,
//! before anything parses it. Today's refresh fetches, parses into
//! `shared.sqlite`, and throws the bytes away; when a parser turns out to be
//! wrong the only recovery is to re-fetch whatever upstream serves *today*,
//! and yesterday's catalog is gone. This project has shipped two parser
//! defects in one day, so "the parser was wrong" is the normal case.
//!
//! ```text
//! raw/source=<tcgcsv|pokemontcgio|pokemon-tcg-data>/
//!     dataset=<groups|products|prices|sets|cards|bulk>/
//!     ingest_date=YYYY-MM-DD/
//!     run=<ULID>/
//!         part-NNNN.<json|csv|tar.gz>.zst
//!         _manifest.json
//! ```
//!
//! Three things about that layout are load-bearing:
//!
//! - **`run=<ULID>`, not a timestamp.** A ULID sorts chronologically *and*
//!   disambiguates two runs on the same date, so a retry after a partial
//!   failure cannot land on the first attempt's objects.
//! - **A `_manifest.json` per run and dataset**, recording the upstream URL,
//!   HTTP status, byte count and SHA-256 of every part. "Did we actually get
//!   everything" is answerable without re-fetching a byte.
//! - **Nothing here parses.** The bytes stored are the bytes received.
//!
//! ## What is deliberately not landed
//!
//! `images.pokemontcg.io` — set symbols and card art. The retention
//! arithmetic behind this bucket (~4.1 MB/day compressed, ~1.5 GB/year, and
//! therefore no lifecycle rule at all) is for JSON only; landing card art
//! would change it completely. That is its own decision, not something that
//! creeps in under "everything we fetch".
//!
//! ## Retention
//!
//! **There is deliberately no lifecycle rule on `raw/`.** Indefinite
//! retention is the decision, measured rather than assumed: at ~4.1 MB/day
//! compressed that is ~1.5 GB/year and roughly $0.03/month in year one —
//! cheaper than revisiting the decision, and far cheaper than losing the
//! ability to rebuild a date. If you are here to tidy up an unmanaged
//! prefix: this is the one that is meant to be unmanaged.
//!
//! ## Where it writes
//!
//! A bucket named by `~/.config/pkdump/lake.env` — host configuration, no
//! default, deliberately a *different* bucket from the Litestream backup
//! bucket. See [`config`].

pub mod config;
mod error;
pub mod keys;
pub mod manifest;
pub mod sink;
pub mod store;

pub use config::{Backend, LakeConfig};
pub use error::{LakeError, Result};
pub use keys::{Dataset, PartFormat, Source};
pub use manifest::Manifest;
pub use sink::RawLanding;
pub use store::{DirStore, ObjectStore, S3Store};

/// Build the landing zone this host is configured for.
///
/// `ingest_date` is the `YYYY-MM-DD` partition every object lands under.
/// Fails, naming `lake.env`, when no destination is configured — an
/// unconfigured lake refuses rather than silently landing nothing.
pub fn open(ingest_date: &str) -> Result<RawLanding> {
    let config = LakeConfig::load()?;
    let store: Box<dyn ObjectStore> = match config.backend {
        Backend::Dir(path) => Box::new(DirStore::new(path)),
        Backend::S3 {
            bucket,
            region,
            prefix,
            endpoint,
        } => Box::new(S3Store::connect(
            &bucket,
            &region,
            &prefix,
            endpoint.as_deref(),
        )?),
    };
    Ok(RawLanding::new(store, ingest_date))
}
