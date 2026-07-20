//! `/api/conditions` — the card-condition value multipliers. Backs the
//! frontend's $lib/conditions.svelte store so `conditionMultiplier` reads the
//! same data the Rust value-history snapshot uses (pokedumpster-e1vo).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use pkdump_db::conditions::{self, Condition};

use crate::{AppError, AppState, blocking};

pub fn routes() -> Router<AppState> {
    Router::new().route("/conditions", get(list))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Condition>>, AppError> {
    let c = blocking(&state, |c| conditions::list_all(c)).await?;
    Ok(Json(c))
}
