//! `/api/card` — card-detail lookups (PLAN.md §5.2).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::DbError;
use pkdump_db::cards::{self, CardDetail, PriceSeries};

use crate::{AppError, AppState, blocking};

/// A `(set_code, number)` pair — the modal's evolution link uses this to
/// resolve a card name to a specific printing.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CardRef {
    pub set_code: String,
    pub number: String,
}

/// Build the card-lookup routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/card/{set}/{number}", get(card_detail))
        .route("/card/{set}/{number}/prices", get(card_prices))
        .route("/cards/by-set-cn", get(by_set_cn))
        .route("/cards/by-name/{name}", get(by_name))
}

/// Find the most-recently-released printing of a card matching `name` —
/// powers the modal's "evolves from / evolves to" clickable names.
async fn by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<CardRef>, AppError> {
    let pair = blocking(&state, move |c| {
        cards::find_first_by_name(c, &name)?
            .ok_or_else(|| DbError::NotFound(format!("no card named '{name}'")))
    })
    .await?;
    Ok(Json(CardRef {
        set_code: pair.0,
        number: pair.1,
    }))
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

/// Time-series price history for every printing of a card. Drives the
/// chart on the card-detail view. v1 emits one series per printing,
/// price_type=`market`; the response shape is condition-agnostic so v2
/// can add per-condition series without breaking the client.
async fn card_prices(
    State(state): State<AppState>,
    Path((set, number)): Path<(String, String)>,
) -> Result<Json<Vec<PriceSeries>>, AppError> {
    let series = blocking(&state, move |c| cards::get_card_prices(c, &set, &number)).await?;
    Ok(Json(series))
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
