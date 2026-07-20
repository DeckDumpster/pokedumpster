//! `pkdump-db` — SQLite persistence for PokeDumpster.
//!
//! Owns the shared/user database split (PLAN.md §3.1): a read-only shared
//! catalog `ATTACH`ed to a mutable per-user collection database, plus the
//! application-layer foreign-key checks against the catalog. The full
//! schema for each database lives in `schema_shared.sql` / `schema_user.sql`
//! and is re-applied idempotently on every open (pokedumpster-luo).

pub mod batches;
pub mod binder;
pub mod binders;
pub mod bundles;
pub mod cards;
pub mod catalog;
pub mod collection;
pub mod collectr_export;
pub mod decks;
pub mod export;
pub mod import;
pub mod latest_prices;
pub mod manual_prices;
pub mod orders;
pub mod sealed;
pub mod sealed_import;
pub mod search;
pub mod search_meta;
pub mod set_aliases;
pub mod sets;
pub mod sub_type_map;
pub mod unresolved;
pub mod user_printings;
pub mod variants;
pub mod wishlist;

mod connection;
mod error;
mod paths;

pub use connection::{
    attach_shared_readonly, connect_user, init_user_schema, open_shared, restore_db, snapshot_db,
};
pub use error::{DbError, Result};
pub use paths::{current_user, pkdump_home, shared_db_path, user_db_path};
