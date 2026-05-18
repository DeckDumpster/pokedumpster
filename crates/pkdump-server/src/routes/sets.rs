//! `/api/sets` — the set catalog (PLAN.md §5.2).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use pkdump_db::sets::{self, SetSummary};

use crate::{AppError, AppState, blocking};

/// Build the set-catalog routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new().route("/sets", get(list))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<SetSummary>>, AppError> {
    let summaries = blocking(&state, |c| sets::list_sets(c)).await?;
    Ok(Json(summaries))
}
