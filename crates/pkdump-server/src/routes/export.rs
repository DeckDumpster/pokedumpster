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
        // The whole user database as one portable envelope. Restored with
        // `pkdump import --json` — deliberately not over HTTP, since it
        // replaces the entire collection.
        .route("/export/json", get(export_json))
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

/// Stream the whole user database as a versioned JSON envelope download.
async fn export_json(State(state): State<AppState>) -> Result<Response, AppError> {
    let json = blocking(&state, |c| pkdump_db::json_backup::export(c)).await?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"pokedumpster-collection.json\"".to_string(),
            ),
        ],
        json,
    )
        .into_response())
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
