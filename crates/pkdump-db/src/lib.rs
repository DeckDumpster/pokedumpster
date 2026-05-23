//! `pkdump-db` — SQLite persistence for PokeDumpster.
//!
//! Owns the shared/user database split (PLAN.md §3.1): a read-only shared
//! catalog `ATTACH`ed to a mutable per-user collection database, plus the
//! refinery-embedded schema migrations and application-layer foreign-key
//! checks against the catalog.

pub mod batches;
pub mod binder;
pub mod binders;
pub mod cards;
pub mod catalog;
pub mod collection;
pub mod decks;
pub mod export;
pub mod import;
pub mod orders;
pub mod sealed;
pub mod sets;
pub mod views;
pub mod wishlist;

mod connection;
mod error;
mod migrations;
mod paths;

pub use connection::{attach_shared_readonly, connect_user, open_shared};
pub use error::{DbError, Result};
pub use migrations::{run_shared_migrations, run_user_migrations};
pub use paths::{current_user, pkdump_home, shared_db_path, user_db_path};
