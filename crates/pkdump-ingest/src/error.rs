//! Error type for the `pkdump-ingest` crate.

use thiserror::Error;

/// Anything that can go wrong fetching or parsing upstream catalog data.
#[derive(Debug, Error)]
pub enum IngestError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("db: {0}")]
    Db(#[from] pkdump_db::DbError),

    #[error("lake: {0}")]
    Lake(#[from] pkdump_lake::LakeError),

    #[error("unexpected upstream data: {0}")]
    BadResponse(String),
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, IngestError>;
