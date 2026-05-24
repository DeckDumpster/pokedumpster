//! `/api/variants` — the variants display-metadata table. Backs the
//! frontend's $lib/variants.svelte store; replaces the ad-hoc
//! variantLabel/Rank/Color/Tag heuristics that used to live in TS.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use pkdump_db::variants::{self, Variant};

use crate::{AppError, AppState, blocking};

pub fn routes() -> Router<AppState> {
    Router::new().route("/variants", get(list))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Variant>>, AppError> {
    let v = blocking(&state, |c| variants::list_all(c)).await?;
    Ok(Json(v))
}
