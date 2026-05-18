//! `/api/decks` — deck CRUD (PLAN.md §5.2, §7).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use pkdump_db::DbError;
use pkdump_db::collection::{self, CollectionRow};
use pkdump_db::decks::{self, Deck, DeckEdit, NewDeck};

use crate::{AppError, AppState, blocking};

/// Build the deck routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/decks", get(list).post(create))
        .route("/decks/{id}", get(detail).put(update).delete(remove))
}

/// A deck plus the cards assigned to it.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
struct DeckDetail {
    deck: Deck,
    cards: Vec<CollectionRow>,
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Deck>>, AppError> {
    Ok(Json(blocking(&state, |c| decks::list(c)).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(new): Json<NewDeck>,
) -> Result<(StatusCode, Json<Deck>), AppError> {
    let deck = blocking(&state, move |c| {
        let id = decks::create(c, &new)?;
        decks::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("deck {id}")))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(deck)))
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<DeckDetail>, AppError> {
    let detail = blocking(&state, move |c| {
        let deck = decks::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("deck {id}")))?;
        let cards = collection::list_by_deck(c, id)?;
        Ok(DeckDetail { deck, cards })
    })
    .await?;
    Ok(Json(detail))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(edit): Json<DeckEdit>,
) -> Result<Json<Deck>, AppError> {
    let deck = blocking(&state, move |c| {
        if !decks::update(c, id, &edit)? {
            return Err(DbError::NotFound(format!("deck {id}")));
        }
        decks::get(c, id)?.ok_or_else(|| DbError::NotFound(format!("deck {id}")))
    })
    .await?;
    Ok(Json(deck))
}

async fn remove(State(state): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, AppError> {
    blocking(&state, move |c| {
        if decks::delete(c, id)? {
            Ok(())
        } else {
            Err(DbError::NotFound(format!("deck {id}")))
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
