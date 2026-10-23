//! `/api/deck-groups` — deck groups: related decks (a cube's packs) as one
//! managed unit. Full CRUDL, release-all, and a delete that chooses between
//! ungrouping the decks and deleting them with the group.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_db::DbError;
use pkdump_db::deck_groups::{self, DeckGroup, DeckGroupEdit, DeleteMode, NewDeckGroup};
use pkdump_db::decks::{self, Deck};

use crate::{AppError, AppState, blocking};

/// Build the deck-group routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/deck-groups", get(list).post(create))
        .route("/deck-groups/{id}", get(detail).put(update).delete(remove))
        .route("/deck-groups/{id}/release", post(release))
        .route("/decks/{id}/group", axum::routing::put(set_deck_group))
}

/// A group plus its decks.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct DeckGroupDetail {
    group: DeckGroup,
    decks: Vec<Deck>,
}

/// The result of releasing a group's decks.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct ReleaseResult {
    #[ts(type = "number")]
    released: i64,
}

/// `PUT /api/decks/{id}/group` body: the group to move into, or null for none.
#[derive(Deserialize, ts_rs::TS)]
#[ts(export)]
struct SetDeckGroup {
    #[ts(type = "number | null")]
    group_id: Option<i64>,
}

#[derive(Deserialize)]
struct DeleteQuery {
    /// `ungroup` (default) keeps the decks; `decks` deletes them with the group.
    mode: Option<DeleteMode>,
}

fn not_found(id: i64) -> DbError {
    DbError::NotFound(format!("deck group {id}"))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<DeckGroup>>, AppError> {
    Ok(Json(blocking(&state, |c| deck_groups::list(c)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewDeckGroup>,
) -> Result<(StatusCode, Json<DeckGroup>), AppError> {
    let group = blocking(&state, move |c| {
        let id = deck_groups::create(c, &new)?;
        deck_groups::get(c, id)?.ok_or_else(|| not_found(id))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(group)))
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<DeckGroupDetail>, AppError> {
    let detail = blocking(&state, move |c| {
        let group = deck_groups::get(c, id)?.ok_or_else(|| not_found(id))?;
        let decks = deck_groups::decks_of(c, id)?;
        Ok(DeckGroupDetail { group, decks })
    })
    .await?;
    Ok(Json(detail))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<DeckGroupEdit>,
) -> Result<Json<DeckGroup>, AppError> {
    let group = blocking(&state, move |c| {
        if !deck_groups::update(c, id, &edit)? {
            return Err(not_found(id));
        }
        deck_groups::get(c, id)?.ok_or_else(|| not_found(id))
    })
    .await?;
    Ok(Json(group))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, AppError> {
    let mode = q.mode.unwrap_or(DeleteMode::Ungroup);
    blocking(&state, move |c| {
        if deck_groups::delete(c, id, mode)? {
            Ok(())
        } else {
            Err(not_found(id))
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn release(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ReleaseResult>, AppError> {
    let released = blocking(&state, move |c| deck_groups::release_all(c, id)).await?;
    Ok(Json(ReleaseResult {
        released: released as i64,
    }))
}

async fn set_deck_group(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SetDeckGroup>,
) -> Result<Json<Deck>, AppError> {
    let deck = blocking(&state, move |c| {
        if !decks::set_group(c, id, body.group_id)? {
            return Err(DbError::NotFound(format!("deck {id}")));
        }
        decks::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("deck {id}")))
    })
    .await?;
    Ok(Json(deck))
}
