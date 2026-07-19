//! `/api/import` — CSV collection import (PLAN.md §5.2, §9).

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::import::{
    self, CombinedCommitResult, CombinedReport, CommitResult, ImportFormat, ResolutionReport,
};

/// A selected-row single-card commit: `include` lists the `source_line`s the
/// user left checked in the preview. (pokedumpster-oq3i.4)
#[derive(Deserialize)]
struct SelectedCommitRequest {
    format: String,
    content: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    include: Vec<u32>,
}

/// A selected-row Collectr commit — separate include lists per pane so the
/// garden wall holds. (pokedumpster-oq3i.4)
#[derive(Deserialize)]
struct CollectrSelectedRequest {
    content: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    include_singles: Vec<u32>,
    #[serde(default)]
    include_sealed: Vec<u32>,
}

use crate::{AppError, AppState, blocking};

/// Build the import routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/import/csv/preview", post(preview))
        .route("/import/csv/commit", post(commit))
        // Selected-row commit: only the rows the user left checked in the
        // preview are written (pokedumpster-oq3i.4).
        .route("/import/csv/commit-selected", post(commit_selected))
        // Collectr yields singles + sealed in one file; its own endpoints
        // return both resolutions separately (the garden wall).
        .route("/import/collectr/preview", post(preview_collectr))
        .route("/import/collectr/commit", post(commit_collectr))
        .route(
            "/import/collectr/commit-selected",
            post(commit_collectr_selected),
        )
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

/// Commit only the matched rows the user selected in the preview.
async fn commit_selected(
    State(state): State<AppState>,
    Json(req): Json<SelectedCommitRequest>,
) -> Result<Json<CommitResult>, AppError> {
    let format = ImportFormat::parse(&req.format)?;
    let result = blocking(&state, move |c| {
        import::commit_selected(c, format, &req.content, &req.include, req.name.as_deref())
    })
    .await?;
    Ok(Json(result))
}

/// Commit only the selected rows of a Collectr import (per-pane include lists).
async fn commit_collectr_selected(
    State(state): State<AppState>,
    Json(req): Json<CollectrSelectedRequest>,
) -> Result<Json<CombinedCommitResult>, AppError> {
    let result = blocking(&state, move |c| {
        import::commit_collectr_selected(
            c,
            &req.content,
            &req.include_singles,
            &req.include_sealed,
            req.name.as_deref(),
        )
    })
    .await?;
    Ok(Json(result))
}

/// Preview a Collectr import — singles and sealed resolved separately, plus
/// any skipped (non-Pokémon) rows. Writes nothing. The `format` field is
/// ignored; this endpoint is Collectr-specific.
async fn preview_collectr(
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> Result<Json<CombinedReport>, AppError> {
    let report = blocking(&state, move |c| import::preview_collectr(c, &req.content)).await?;
    Ok(Json(report))
}

/// Commit a Collectr import: singles to `collection` under a batch, sealed
/// to `sealed_collection`, kept strictly apart.
async fn commit_collectr(
    State(state): State<AppState>,
    Json(req): Json<ImportRequest>,
) -> Result<Json<CombinedCommitResult>, AppError> {
    let result = blocking(&state, move |c| {
        import::commit_collectr(c, &req.content, req.name.as_deref())
    })
    .await?;
    Ok(Json(result))
}
