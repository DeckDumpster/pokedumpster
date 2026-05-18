//! Error type for the `pkdump-db` crate.

use thiserror::Error;

/// Anything that can go wrong opening, migrating, or querying a database.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("migration: {0}")]
    Migration(#[from] refinery::Error),

    #[error("environment: {0}")]
    Env(String),

    /// A referenced row does not exist (a catalog key, or a missing entry).
    /// Maps to HTTP 404 at the API boundary.
    #[error("not found: {0}")]
    NotFound(String),

    /// A request would violate an invariant (e.g. a copy in both a binder
    /// and a deck). Maps to HTTP 409 at the API boundary.
    #[error("conflict: {0}")]
    Conflict(String),
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, DbError>;
