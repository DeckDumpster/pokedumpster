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
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, DbError>;
