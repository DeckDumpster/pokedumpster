//! `/api/card` — card-detail lookups (PLAN.md §5.2).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::DbError;
use pkdump_db::cards::{self, CardDetail};

use crate::{AppError, AppState, blocking};

/// Build the card-lookup routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/card/{set}/{number}", get(card_detail))
        .route("/cards/by-set-cn", get(by_set_cn))
}

async fn lookup(
    state: &AppState,
    set: String,
    number: String,
) -> Result<Json<CardDetail>, AppError> {
    let detail = blocking(state, move |c| {
        cards::get_card_detail(c, &set, &number)?
            .ok_or_else(|| DbError::NotFound(format!("card {set}/{number}")))
    })
    .await?;
    Ok(Json(detail))
}

async fn card_detail(
    State(state): State<AppState>,
    Path((set, number)): Path<(String, String)>,
) -> Result<Json<CardDetail>, AppError> {
    lookup(&state, set, number).await
}

#[derive(Deserialize)]
struct BySetCn {
    set: String,
    cn: String,
}

async fn by_set_cn(
    State(state): State<AppState>,
    Query(q): Query<BySetCn>,
) -> Result<Json<CardDetail>, AppError> {
    lookup(&state, q.set, q.cn).await
}
