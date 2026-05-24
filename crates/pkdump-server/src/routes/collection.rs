//! `/api/collection` — collection CRUD endpoints (PLAN.md §5.2).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::DbError;
use pkdump_db::collection::{self, CollectionRow, CopyEdit, NewCopy};

use crate::{AppError, AppState, blocking};

/// Build the `/api/collection` router.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/bulk", post(bulk_create))
        .route("/bulk-delete", post(bulk_delete))
        .route("/by-printing/{printing_id}", delete(delete_by_printing))
        .route("/{id}", get(get_one).put(update_one).delete(delete_one))
        .route("/{id}/move", put(move_copy))
        .route("/{id}/status", put(set_status))
        .route("/{id}/printing", put(change_printing))
}

/// Delete the most recently added copy of a printing — the binder modal's
/// "−" action, which works without knowing a specific copy id.
async fn delete_by_printing(
    State(state): State<AppState>,
    Path(printing_id): Path<String>,
) -> Result<StatusCode, AppError> {
    blocking(&state, move |c| {
        if collection::delete_latest_for_printing(c, &printing_id)? {
            Ok(())
        } else {
            Err(DbError::NotFound(format!(
                "no copy of printing '{printing_id}'"
            )))
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MoveBody {
    binder_id: Option<i64>,
    deck_id: Option<i64>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
    note: Option<String>,
}

#[derive(Deserialize)]
struct PrintingBody {
    printing_id: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct BulkAdded {
    #[ts(type = "Array<number>")]
    ids: Vec<i64>,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct BulkDeleted {
    #[ts(type = "number")]
    deleted: usize,
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<CollectionRow>>, AppError> {
    // Single-user app — return everything; client-side does filtering,
    // aggregation, and sort. Lazy loading is YAGNI until we feel it.
    let rows = blocking(&state, |c| collection::list_rows(c)).await?;
    Ok(Json(rows))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewCopy>,
) -> Result<(StatusCode, Json<CollectionRow>), AppError> {
    let row = blocking(&state, move |c| {
        let id = collection::add(c, &new)?;
        collection::get_row(c, id)?
            .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<CollectionRow>, AppError> {
    let row = blocking(&state, move |c| {
        collection::get_row(c, id)?
            .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(row))
}

async fn update_one(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<CopyEdit>,
) -> Result<Json<CollectionRow>, AppError> {
    let row = blocking(&state, move |c| {
        if !collection::update(c, id, &edit)? {
            return Err(DbError::NotFound(format!("collection entry {id}")));
        }
        collection::get_row(c, id)?
            .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(row))
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

/// Assign a copy to a binder, a deck, or neither (audited via movement_log).
async fn move_copy(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<MoveBody>,
) -> Result<Json<CollectionRow>, AppError> {
    let row = blocking(&state, move |c| {
        collection::move_to(c, id, body.binder_id, body.deck_id, body.note.as_deref())?;
        collection::get_row(c, id)?
            .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(row))
}

/// Change a copy's lifecycle status (audited via status_log).
async fn set_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<StatusBody>,
) -> Result<Json<CollectionRow>, AppError> {
    let row = blocking(&state, move |c| {
        collection::set_status(c, id, &body.status, body.note.as_deref())?;
        collection::get_row(c, id)?
            .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(row))
}

/// Change a copy's printing — correcting a mis-logged variant.
async fn change_printing(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<PrintingBody>,
) -> Result<Json<CollectionRow>, AppError> {
    let row = blocking(&state, move |c| {
        collection::change_printing(c, id, &body.printing_id)?;
        collection::get_row(c, id)?
            .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))
    })
    .await?;
    Ok(Json(row))
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
