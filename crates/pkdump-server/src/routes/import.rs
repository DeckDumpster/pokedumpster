//! `/api/import` — CSV collection import (PLAN.md §5.2, §9).

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::import::{self, CommitResult, ImportFormat, ResolutionReport};

use crate::{AppError, AppState, blocking};

/// Build the import routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/import/csv/preview", post(preview))
        .route("/import/csv/commit", post(commit))
}

/// An import request: the CSV text and which format to parse it as.
#[derive(Deserialize)]
struct ImportRequest {
    format: String,
    content: String,
    /// Optional batch name (commit only) — e.g. the uploaded file name.
    #[serde(default)]
    name: Option<String>,
}

/// Parse + resolve a CSV without writing anything — the import preview.
async fn preview(
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> Result<Json<ResolutionReport>, AppError> {
    let format = ImportFormat::parse(&req.format)?;
    let report = blocking(&state, move |c| import::preview(c, format, &req.content)).await?;
    Ok(Json(report))
}

/// Parse, resolve, and commit a CSV's matched rows under a fresh batch.
async fn commit(
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> Result<Json<CommitResult>, AppError> {
    let format = ImportFormat::parse(&req.format)?;
    let result = blocking(&state, move |c| {
        import::commit(c, format, &req.content, req.name.as_deref())
    })
    .await?;
    Ok(Json(result))
}
