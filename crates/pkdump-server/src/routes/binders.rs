//! `/api/binders` — binder CRUD (PLAN.md §5.2, §7).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use pkdump_db::DbError;
use pkdump_db::binders::{self, Binder, BinderEdit, NewBinder};
use pkdump_db::collection::{self, CollectionRow};

use crate::{AppError, AppState, blocking};

/// Build the binder routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/binders", get(list).post(create))
        .route("/binders/{id}", get(detail).put(update).delete(remove))
}

/// A binder plus the cards assigned to it.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct BinderDetail {
    binder: Binder,
    cards: Vec<CollectionRow>,
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Binder>>, AppError> {
    Ok(Json(blocking(&state, |c| binders::list(c)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewBinder>,
) -> Result<(StatusCode, Json<Binder>), AppError> {
    let binder = blocking(&state, move |c| {
        let id = binders::create(c, &new)?;
        binders::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("binder {id}")))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(binder)))
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<BinderDetail>, AppError> {
    let detail = blocking(&state, move |c| {
        let binder =
            binders::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("binder {id}")))?;
        let cards = collection::list_by_binder(c, id)?;
        Ok(BinderDetail { binder, cards })
    })
    .await?;
    Ok(Json(detail))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<BinderEdit>,
) -> Result<Json<Binder>, AppError> {
    let binder = blocking(&state, move |c| {
        if !binders::update(c, id, &edit)? {
            return Err(DbError::NotFound(format!("binder {id}")));
        }
        binders::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("binder {id}")))
    })
    .await?;
    Ok(Json(binder))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    blocking(&state, move |c| {
        if binders::delete(c, id)? {
            Ok(())
        } else {
            Err(DbError::NotFound(format!("binder {id}")))
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
