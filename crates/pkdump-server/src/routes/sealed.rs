//! `/api/sealed` — sealed-product catalog search and collection (PLAN.md §8).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::DbError;
use pkdump_db::sealed::{self, NewSealed, SealedEdit, SealedEntry, SealedProduct};

use crate::{AppError, AppState, blocking};

/// Build the sealed routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sealed/products", get(search))
        .route("/sealed/collection", get(list).post(create))
        .route("/sealed/collection/{id}", put(update).delete(remove))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
}

async fn search(
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<SealedProduct>>, AppError> {
    let query = p.q.unwrap_or_default();
    let results = blocking(&state, move |c| sealed::search_products(c, &query, 50)).await?;
    Ok(Json(results))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<SealedEntry>>, AppError> {
    Ok(Json(blocking(&state, |c| sealed::list(c)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewSealed>,
) -> Result<(StatusCode, Json<SealedEntry>), AppError> {
    let entry = blocking(&state, move |c| {
        let id = sealed::add(c, &new)?;
        sealed::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("sealed entry {id}")))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<SealedEdit>,
) -> Result<Json<SealedEntry>, AppError> {
    let entry = blocking(&state, move |c| {
        if !sealed::update(c, id, &edit)? {
            return Err(DbError::NotFound(format!("sealed entry {id}")));
        }
        sealed::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("sealed entry {id}")))
    })
    .await?;
    Ok(Json(entry))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    blocking(&state, move |c| {
        if sealed::delete(c, id)? {
            Ok(())
        } else {
            Err(DbError::NotFound(format!("sealed entry {id}")))
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
