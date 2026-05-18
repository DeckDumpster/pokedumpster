//! `/api/views` — saved collection-filter views (PLAN.md §5.2).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};

use pkdump_db::DbError;
use pkdump_db::views::{self, CollectionView, NewView, ViewEdit};

use crate::{AppError, AppState, blocking};

/// Build the saved-view routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/views", get(list).post(create))
        .route("/views/{id}", put(update).delete(remove))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<CollectionView>>, AppError> {
    Ok(Json(blocking(&state, |c| views::list(c)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewView>,
) -> Result<(StatusCode, Json<i64>), AppError> {
    let id = blocking(&state, move |c| views::create(c, &new)).await?;
    Ok((StatusCode::CREATED, Json(id)))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<ViewEdit>,
) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| views::update(c, id, &edit)).await?;
    not_found_unless(ok, id)
}

async fn remove(State(state): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, AppError> {
    let ok = blocking(&state, move |c| views::delete(c, id)).await?;
    not_found_unless(ok, id)
}

fn not_found_unless(ok: bool, id: i64) -> Result<StatusCode, AppError> {
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::from(DbError::NotFound(format!("collection view {id}"))))
    }
}
