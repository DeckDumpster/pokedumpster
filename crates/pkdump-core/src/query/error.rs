//! Error type for query parsing.

/// A query that could not be tokenized or parsed.
///
/// `position` is the byte offset into the original query string where the
/// problem was detected, so the frontend can place a caret under the
/// offending token. For end-of-input errors it equals the query length.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    pub message: String,
    pub position: usize,
}

impl QueryError {
    pub fn new(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }
}
