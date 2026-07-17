//! `/api/sets` — the set catalog and binder pages (PLAN.md §5.2, §6).
//!
//! Acts as a container endpoint: returns both real sets and TTBB-style
//! bundles in one list, and dispatches binder/analytics requests to the
//! matching back-end query when the code is a bundle slug
//! (pokedumpster-80q).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use pkdump_db::DbError;
use pkdump_db::binder::{self, BinderPage, BinderQuery, MissingExport};
use pkdump_db::bundles;
use pkdump_db::sets::{self, SetAnalytics, SetSummary};

use crate::{AppError, AppState, blocking};

/// Build the set-catalog routes (mounted under `/api`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sets", get(list))
        .route("/sets/{code}/binder", get(binder_page))
        .route("/sets/{code}/analytics", get(analytics))
        .route("/sets/{code}/tcg-export", get(tcg_export))
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<SetSummary>>, AppError> {
    let summaries = blocking(&state, |c| {
        let mut sets = sets::list_sets(c)?;
        let bundles = bundles::list_bundle_summaries(c)?;
        sets.extend(bundles);
        Ok(sets)
    })
    .await?;
    Ok(Json(summaries))
}

async fn analytics(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<SetAnalytics>, AppError> {
    let stats = blocking(&state, move |c| {
        let resolved = if bundles::is_bundle(c, &code)? {
            bundles::analytics(c, &code)?
        } else {
            sets::analytics(c, &code)?
        };
        resolved.ok_or_else(|| DbError::NotFound(format!("set {code}")))
    })
    .await?;
    Ok(Json(stats))
}

/// Every missing card in a set as TCGplayer Mass Entry lines. Bundles are
/// out of scope (their slots are reprints of cards in other sets with their
/// own codes), so a bundle code resolves to `404`.
async fn tcg_export(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<MissingExport>, AppError> {
    let export = blocking(&state, move |c| {
        if bundles::is_bundle(c, &code)? {
            return Err(DbError::NotFound(format!("set {code}")));
        }
        binder::missing_for_export(c, &code)?
            .ok_or_else(|| DbError::NotFound(format!("set {code}")))
    })
    .await?;
    Ok(Json(export))
}

#[derive(Deserialize)]
struct BinderParams {
    page: Option<i64>,
    layout: Option<i64>,
    secret: Option<bool>,
    subset: Option<bool>,
    promos: Option<bool>,
    /// `number` | `number_desc` | `price` | `name` | `rarity`.
    sort: Option<String>,
    /// In-set card-name search.
    q: Option<String>,
    /// `all` | `have` | `need` | `dupes`.
    filter: Option<String>,
}

async fn binder_page(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Query(p): Query<BinderParams>,
) -> Result<Json<BinderPage>, AppError> {
    let query = BinderQuery {
        page: p.page.unwrap_or(1),
        layout: p.layout.unwrap_or(9),
        include_secret: p.secret.unwrap_or(true),
        include_subset: p.subset.unwrap_or(true),
        include_promos: p.promos.unwrap_or(false),
        sort: p.sort.unwrap_or_else(|| "number".into()),
        search: p.q.unwrap_or_default(),
        filter: p.filter.unwrap_or_else(|| "all".into()),
    };
    let page = blocking(&state, move |c| {
        let resolved = if bundles::is_bundle(c, &code)? {
            bundles::get_bundle_binder(c, &code, &query)?
        } else {
            binder::get_binder_page(c, &code, &query)?
        };
        resolved.ok_or_else(|| DbError::NotFound(format!("set {code}")))
    })
    .await?;
    Ok(Json(page))
}
