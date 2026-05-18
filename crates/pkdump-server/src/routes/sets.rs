//! `/api/sets` — the set catalog and binder pages (PLAN.md §5.2, §6).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::DbError;
use pkdump_db::binder::{self, BinderPage};
use pkdump_db::sets::{self, SetAnalytics, SetSummary};

use crate::{AppError, AppState, blocking};

/// Build the set-catalog routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sets", get(list))
        .route("/sets/{code}/binder", get(binder_page))
        .route("/sets/{code}/analytics", get(analytics))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<SetSummary>>, AppError> {
    let summaries = blocking(&state, |c| sets::list_sets(c)).await?;
    Ok(Json(summaries))
}

async fn analytics(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<SetAnalytics>, AppError> {
    let stats = blocking(&state, move |c| {
        sets::analytics(c, &code)?.ok_or_else(|| DbError::NotFound(format!("set {code}")))
    })
    .await?;
    Ok(Json(stats))
}

#[derive(Deserialize)]
struct BinderParams {
    page: Option<i64>,
    layout: Option<i64>,
    secret: Option<bool>,
    subset: Option<bool>,
    promos: Option<bool>,
}

async fn binder_page(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Query(p): Query<BinderParams>,
) -> Result<Json<BinderPage>, AppError> {
    let page = blocking(&state, move |c| {
        binder::get_binder_page(
            c,
            &code,
            p.page.unwrap_or(1),
            p.layout.unwrap_or(9),
            p.secret.unwrap_or(true),
            p.subset.unwrap_or(true),
            p.promos.unwrap_or(false),
        )?
        .ok_or_else(|| DbError::NotFound(format!("set {code}")))
    })
    .await?;
    Ok(Json(page))
}
