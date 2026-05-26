//! `/api/bundles` — TTBB-style logical-set views. Compose
//! `tcgcsv_products` joined to bridged `printings` so the user can
//! enter a whole pack from one page. See pokedumpster-qfz.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use pkdump_db::DbError;
use pkdump_db::bundles::{self, Bundle, BundleDetail};

use crate::{AppError, AppState, blocking};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/bundles", get(list))
        .route("/bundles/{slug}", get(detail))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Bundle>>, AppError> {
    let bundles = blocking(&state, |c| bundles::list_bundles(c)).await?;
    Ok(Json(bundles))
}

async fn detail(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<BundleDetail>, AppError> {
    let detail = blocking(&state, move |c| {
        bundles::get_bundle(c, &slug)?.ok_or_else(|| DbError::NotFound(format!("bundle {slug}")))
    })
    .await?;
    Ok(Json(detail))
}
