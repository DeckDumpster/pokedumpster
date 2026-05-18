//! `/api/wishlist` — cards the user wants to acquire (PLAN.md §5.1).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::DbError;
use pkdump_db::wishlist::{self, NewWish, WishEdit, WishlistEntry};

use crate::{AppError, AppState, blocking};

/// Build the wishlist routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/wishlist", get(list).post(create))
        .route("/wishlist/{id}", put(update).delete(remove))
        .route("/wishlist/{id}/fulfill", put(fulfill))
}

#[derive(Deserialize)]
struct ListParams {
    include_fulfilled: Option<bool>,
}

async fn list(
    State(state): State<AppState>,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<WishlistEntry>>, AppError> {
    let include = p.include_fulfilled.unwrap_or(false);
    Ok(Json(blocking(&state, move |c| wishlist::list(c, include)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewWish>,
) -> Result<(StatusCode, Json<i64>), AppError> {
    let id = blocking(&state, move |c| wishlist::add(c, &new)).await?;
    Ok((StatusCode::CREATED, Json(id)))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<WishEdit>,
) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| wishlist::update(c, id, &edit)).await?;
    not_found_unless(ok, id)
}

#[derive(Deserialize)]
struct FulfillBody {
    fulfilled: bool,
}

async fn fulfill(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<FulfillBody>,
) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| wishlist::set_fulfilled(c, id, body.fulfilled)).await?;
    not_found_unless(ok, id)
}

async fn remove(State(state): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| wishlist::delete(c, id)).await?;
    not_found_unless(ok, id)
}

fn not_found_unless(ok: bool, id: i64) -> Result<StatusCode, AppError> {
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::from(DbError::NotFound(format!("wishlist entry {id}"))))
    }
}
