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
pub mod conditions;
pub mod decks;
pub mod export;
pub mod import;
pub mod json_backup;
pub mod latest_prices;
pub mod manual_prices;
pub mod orders;
pub mod registry;
pub mod schema_version;
pub mod sealed;
pub mod sealed_import;
pub mod search;
pub mod search_meta;
pub mod set_aliases;
pub mod sets;
pub mod sub_type_map;
pub mod tenants;
pub mod unresolved;
pub mod user_printings;
pub mod value_history;
pub mod variants;
pub mod wishlist;

mod connection;
mod error;
mod paths;

pub use connection::{
    attach_shared_readonly, connect_user, init_user_schema, open_registry, open_shared, open_user,
    restore_db, snapshot_db,
};
pub use error::{DbError, Result};
pub use paths::{
    HANDLE_RULE, TENANTS_DIR, current_user, legacy_user_db_path, pkdump_home, registry_db_path,
    shared_db_path, tenant_db_file, tenant_db_path, tenants_dir, validate_database_id,
    validate_tenant_name,
};
/// Which collection single-tenant mode serves for a handle. Not in `paths`
/// because it is not a path calculation: it reads the user registry, and the
/// answer for a migrated data directory is a file whose name the handle does
/// not appear in. See [`tenants::resolve`].
pub use tenants::resolve as resolve_collection;
