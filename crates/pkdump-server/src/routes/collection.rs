//! `/api/collection` — collection CRUD endpoints (PLAN.md §5.2).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::DbError;
use pkdump_db::collection::{self, CollectionEntry, CopyEdit, NewCopy};

use crate::{AppError, AppState, blocking};

/// Build the `/api/collection` router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/bulk", post(bulk_create))
        .route("/bulk-delete", post(bulk_delete))
        .route("/{id}", get(get_one).put(update_one).delete(delete_one))
}

#[derive(Deserialize)]
struct ListParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct BulkAdded {
    ids: Vec<i64>,
}

#[derive(Serialize)]
struct BulkDeleted {
    deleted: usize,
}

async fn list(
    State(state): State<AppState>,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<CollectionEntry>>, AppError> {
    let limit = p.limit.unwrap_or(100).clamp(1, 1000);
    let offset = p.offset.unwrap_or(0).max(0);
    let entries = blocking(&state, move |c| collection::list(c, limit, offset)).await?;
    Ok(Json(entries))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewCopy>,
) -> Result<(StatusCode, Json<CollectionEntry>), AppError> {
    let entry = blocking(&state, move |c| {
        let id = collection::add(c, &new)?;
        collection::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<CollectionEntry>, AppError> {
    let entry = blocking(&state, move |c| {
        collection::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(entry))
}

async fn update_one(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<CopyEdit>,
) -> Result<Json<CollectionEntry>, AppError> {
    let entry = blocking(&state, move |c| {
        if !collection::update(c, id, &edit)? {
            return Err(DbError::NotFound(format!("collection entry {id}")));
        }
        collection::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(entry))
}

async fn delete_one(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    blocking(&state, move |c| {
        if collection::delete(c, id)? {
            Ok(())
        } else {
            Err(DbError::NotFound(format!("collection entry {id}")))
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn bulk_create(
    State(state): State<AppState>,
    Json(items): Json<Vec<NewCopy>>,
) -> Result<Json<BulkAdded>, AppError> {
    let ids = blocking(&state, move |c| {
        let mut ids = Vec::with_capacity(items.len());
        for item in &items {
            ids.push(collection::add(c, item)?);
        }
        Ok(ids)
    })
    .await?;
    Ok(Json(BulkAdded { ids }))
}

async fn bulk_delete(
    State(state): State<AppState>,
    Json(ids): Json<Vec<i64>>,
) -> Result<Json<BulkDeleted>, AppError> {
    let deleted = blocking(&state, move |c| {
        let mut n = 0usize;
        for id in &ids {
            if collection::delete(c, *id)? {
                n += 1;
            }
        }
        Ok(n)
    })
    .await?;
    Ok(Json(BulkDeleted { deleted }))
}
