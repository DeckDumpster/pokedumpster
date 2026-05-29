//! `/api/manual-prices` — user-entered price observations for printings
//! that the TCGplayer feed doesn't cover (PLAN.md §5.2 addendum;
//! decision pokedumpster-7bc).
//!
//! Listing is folded into `/api/card/{set}/{number}/prices`; this module
//! only handles writes.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};

use pkdump_db::DbError;
use pkdump_db::manual_prices::{self, ManualPrice, NewManualPrice};

use crate::{AppError, AppState, blocking};

/// Build the manual-prices routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/manual-prices", post(create))
        .route("/manual-prices/by-printing/{printing_id}", get(by_printing))
        .route("/manual-prices/{id}", delete(remove))
}

/// All manual prices for a printing, newest first — drives the modal's
/// history table where the user can delete individual entries.
async fn by_printing(
    State(state): State<AppState>,
    Path(printing_id): Path<String>,
) -> Result<Json<Vec<ManualPrice>>, AppError> {
    let rows = blocking(&state, move |c| {
        manual_prices::list_for_printing(c, &printing_id)
    })
    .await?;
    Ok(Json(rows))
}

/// Add a manual price observation. Returns the new id.
async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewManualPrice>,
) -> Result<(StatusCode, Json<i64>), AppError> {
    let id = blocking(&state, move |c| manual_prices::insert(c, &new)).await?;
    Ok((StatusCode::CREATED, Json(id)))
}

/// Remove a manual price observation.
async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| manual_prices::delete(c, id)).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::from(DbError::NotFound(format!(
            "manual_price {id}"
        ))))
    }
}
