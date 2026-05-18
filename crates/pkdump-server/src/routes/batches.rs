//! `/api/batches` — ingest batches (PLAN.md §5.1).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::DbError;
use pkdump_db::batches::{self, Batch, BatchDetail};

use crate::{AppError, AppState, blocking};

/// Build the batch routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/batches", get(list))
        .route("/batches/{id}", get(detail))
}

#[derive(Deserialize)]
struct ListParams {
    limit: Option<i64>,
}

async fn list(
    State(state): State<AppState>,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<Batch>>, AppError> {
    let limit = p.limit.unwrap_or(0).max(0);
    Ok(Json(blocking(&state, move |c| batches::list(c, limit)).await?))
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<BatchDetail>, AppError> {
    let detail = blocking(&state, move |c| {
        batches::get_detail(c, id)?.ok_or_else(|| DbError::NotFound(format!("batch {id}")))
    })
    .await?;
    Ok(Json(detail))
}
