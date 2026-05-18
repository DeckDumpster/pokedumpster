//! `/api/export` — collection CSV export (PLAN.md §9).

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::{AppError, AppState, blocking};

/// Build the export routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new().route("/export/csv", get(export_csv))
}

/// Stream the collection as a ManaBox-shaped CSV download.
async fn export_csv(State(state): State<AppState>) -> Result<Response, AppError> {
    let csv = blocking(&state, |c| pkdump_db::export::manabox_csv(c)).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"pokedumpster-collection.csv\"",
            ),
        ],
        csv,
    )
        .into_response())
}
