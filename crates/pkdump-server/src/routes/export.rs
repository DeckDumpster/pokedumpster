//! `/api/export` — collection CSV export (PLAN.md §9).

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::{AppError, AppState, blocking};

/// Build the export routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/export/csv", get(export_csv))
        // Collectr-shaped exports — singles and sealed kept as separate
        // files (the garden wall), each a valid Collectr import on its own.
        .route("/export/collectr/singles.csv", get(export_collectr_singles))
        .route("/export/collectr/sealed.csv", get(export_collectr_sealed))
}

/// Stream the collection as a ManaBox-shaped CSV download.
async fn export_csv(State(state): State<AppState>) -> Result<Response, AppError> {
    let csv = blocking(&state, |c| pkdump_db::export::manabox_csv(c)).await?;
    Ok(csv_download(csv, "pokedumpster-collection.csv"))
}

/// Stream owned single cards as a Collectr-shaped CSV download.
async fn export_collectr_singles(State(state): State<AppState>) -> Result<Response, AppError> {
    let csv = blocking(&state, |c| {
        pkdump_db::collectr_export::collectr_singles_csv(c)
    })
    .await?;
    Ok(csv_download(csv, "pokedumpster-collectr-singles.csv"))
}

/// Stream owned sealed products as a Collectr-shaped CSV download.
async fn export_collectr_sealed(State(state): State<AppState>) -> Result<Response, AppError> {
    let csv = blocking(&state, |c| {
        pkdump_db::collectr_export::collectr_sealed_csv(c)
    })
    .await?;
    Ok(csv_download(csv, "pokedumpster-collectr-sealed.csv"))
}

/// Wrap CSV text in an attachment download response.
fn csv_download(csv: String, filename: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        csv,
    )
        .into_response()
}
