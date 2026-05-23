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

/// SQL `CASE` mapping a printing's `variant` to its TCGplayer price
/// sub-type. Embedded in the price subqueries (`cards`, `binder`); assumes
/// the printings table is aliased `p`. Pattern-overlay variants (ball
/// patterns, energy symbol, team rocket) live as their own TCGplayer
/// products but all price under sub_type "Holofoil"; the per-pattern
/// product_id on the printing is what disambiguates them. Variants with
/// no sub-type mapping (stamps for now) still fall through to NULL.
pub(crate) const VARIANT_PRICE_SUBTYPE: &str = "CASE p.variant \
     WHEN 'normal' THEN 'Normal' \
     WHEN 'holo' THEN 'Holofoil' \
     WHEN 'reverse_holo' THEN 'Reverse Holofoil' \
     WHEN 'first_ed_holo' THEN '1st Edition Holofoil' \
     WHEN 'first_ed_normal' THEN '1st Edition Normal' \
     WHEN 'unlimited_holo' THEN 'Unlimited Holofoil' \
     WHEN 'pokeball_rh' THEN 'Holofoil' \
     WHEN 'masterball_rh' THEN 'Holofoil' \
     WHEN 'quickball_rh' THEN 'Holofoil' \
     WHEN 'duskball_rh' THEN 'Holofoil' \
     WHEN 'loveball_rh' THEN 'Holofoil' \
     WHEN 'friendball_rh' THEN 'Holofoil' \
     WHEN 'energy_symbol_rh' THEN 'Holofoil' \
     WHEN 'team_rocket_rh' THEN 'Holofoil' END";
