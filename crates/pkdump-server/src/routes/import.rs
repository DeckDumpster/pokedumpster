//! `/api/import` — CSV collection import (PLAN.md §5.2, §9).

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::import::{
    self, CombinedCommitResult, CombinedReport, CommitResult, ImportFormat, ResolutionReport,
};
use pkdump_db::unresolved::{self, UnresolvedRow};

/// A selected-row single-card commit: `include` lists the `source_line`s the
/// user left checked in the preview. (pokedumpster-oq3i.4)
///
/// `park_unmatched` sends the leftover unmatched rows to the dead-letter queue
/// (pokedumpster-oq3i.5).
#[derive(Deserialize)]
struct SelectedCommitRequest {
    format: String,
    content: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    include: Vec<u32>,
    #[serde(default)]
    park_unmatched: bool,
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
    #[serde(default)]
    park_unmatched: bool,
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
        // Dead-letter (unresolved) queue (pokedumpster-oq3i.5).
        .route("/import/unresolved", get(list_unresolved))
        .route("/import/unresolved/{id}/resolve", post(resolve_unresolved))
        .route("/import/unresolved/{id}/dismiss", post(dismiss_unresolved))
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
        import::commit_selected(
            c,
            format,
            &req.content,
            &req.include,
            req.name.as_deref(),
            req.park_unmatched,
        )
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
            req.park_unmatched,
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

// --- Dead-letter (unresolved) queue (pokedumpster-oq3i.5) ---

/// A manual resolution: exactly one of `printing_id` (single) or `product_id`
/// (sealed) identifies the catalog item the parked row should become.
#[derive(Deserialize)]
struct ResolveRequest {
    #[serde(default)]
    printing_id: Option<String>,
    #[serde(default)]
    product_id: Option<i64>,
}

/// The outcome of resolving a parked row: which side it landed on and the id
/// of the created collection / sealed row.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UnresolvedResolveResult {
    /// `single` | `sealed`.
    pub kind: String,
    /// The created `collection.id` (single) or `sealed_collection.id` (sealed).
    #[ts(type = "number")]
    pub id: i64,
}

/// List the open dead-letter queue.
async fn list_unresolved(
    State(state): State<AppState>,
) -> Result<Json<Vec<UnresolvedRow>>, AppError> {
    let rows = blocking(&state, move |c| unresolved::list_open(c)).await?;
    Ok(Json(rows))
}

/// Resolve one parked row to a chosen printing (single) or product (sealed).
async fn resolve_unresolved(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ResolveRequest>,
) -> Result<Json<UnresolvedResolveResult>, AppError> {
    let result = match (req.printing_id, req.product_id) {
        (Some(printing_id), None) => {
            let row = blocking(&state, move |c| {
                unresolved::resolve_single(c, id, &printing_id)
            })
            .await?;
            UnresolvedResolveResult {
                kind: "single".to_string(),
                id: row.id,
            }
        }
        (None, Some(product_id)) => {
            let entry = blocking(&state, move |c| {
                unresolved::resolve_sealed(c, id, product_id)
            })
            .await?;
            UnresolvedResolveResult {
                kind: "sealed".to_string(),
                id: entry.id,
            }
        }
        _ => {
            return Err(AppError::bad_request(
                "resolve needs exactly one of printing_id or product_id",
            ));
        }
    };
    Ok(Json(result))
}

/// Dismiss one parked row without writing a copy.
async fn dismiss_unresolved(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<()>, AppError> {
    blocking(&state, move |c| unresolved::dismiss(c, id)).await?;
    Ok(Json(()))
}
