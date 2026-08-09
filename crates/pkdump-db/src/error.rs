//! Error type for the `pkdump-db` crate.

use thiserror::Error;

/// Anything that can go wrong opening or querying a database.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

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

    /// An import file could not be parsed. Maps to HTTP 400 at the API
    /// boundary — the caller sent a malformed CSV.
    #[error("import: {0}")]
    Import(String),

    /// A database's `PRAGMA user_version` is higher than the version this
    /// build understands, so it was refused rather than opened
    /// ([`crate::schema_version`]). Its own variant because it is the one
    /// error here that is never the caller's fault and never retryable:
    /// the fix is a different binary, not a different request.
    #[error("schema version: {0}")]
    SchemaVersion(String),

    /// Failed to parse an embedded data file (e.g. `data/variants.json`).
    /// Compile-time `include_str!` means this is a developer-error path —
    /// a malformed JSON file ships in the binary.
    #[error("seed: {0}")]
    Seed(#[from] serde_json::Error),
}

impl From<pkdump_core::import::ImportError> for DbError {
    fn from(e: pkdump_core::import::ImportError) -> Self {
        DbError::Import(e.to_string())
    }
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, DbError>;
