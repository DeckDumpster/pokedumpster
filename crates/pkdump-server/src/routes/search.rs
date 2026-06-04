//! `/api/collection/search` and `/api/search/keywords` — the Scryfall-style
//! collection search (see architecture/SEARCH_QUERY_LANGUAGE.md §8).
//!
//! The query is parsed against the registry, compiled, and executed. A parse
//! error returns HTTP 400 with a JSON body `{error, position}` so the frontend
//! can place a caret. The keywords endpoint serves the registry so autocomplete
//! and the help page render from data, not a hardcoded list.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_core::query::parse;
use pkdump_db::search::{self, SearchRow};

use crate::{AppError, AppState, blocking};

/// Mounted inside the `/collection` nest → `/api/collection/search`.
pub fn collection_routes() -> Router<AppState> {
    Router::new().route("/search", get(collection_search))
}

/// Mounted at the `/api` root → `/api/search/keywords`.
pub fn vocabulary_routes() -> Router<AppState> {
    Router::new().route("/search/keywords", get(keywords))
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
    sort: Option<String>,
    dir: Option<String>,
}

/// Run a search. An empty `q` is the default view (all owned printings).
async fn collection_search(
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<SearchRow>>, AppError> {
    let q = p.q.trim();
    let mut compiled = if q.is_empty() {
        search::compile_all()
    } else {
        let ast = parse(q, &state.registry).map_err(|e| {
            AppError::bad_request(
                serde_json::json!({ "error": e.message, "position": e.position }).to_string(),
            )
        })?;
        search::compile(&ast, &state.flags)
    };
    compiled.override_order(p.sort.as_deref(), p.dir.as_deref());

    let rows = blocking(&state, move |c| search::search(c, &compiled)).await?;
    Ok(Json(rows))
}

/// One keyword, for autocomplete + the help page.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
pub struct KeywordInfo {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub operators: Vec<String>,
    pub kind: String,
    pub help: Option<String>,
}

/// One `is:` flag value, for autocomplete.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
pub struct FlagInfo {
    pub flag: String,
    pub help: Option<String>,
}

/// The data-driven search vocabulary served to the frontend.
#[derive(Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SearchVocabulary {
    pub keywords: Vec<KeywordInfo>,
    pub flags: Vec<FlagInfo>,
}

/// Serve the keyword + flag registry (no DB access — read from AppState).
async fn keywords(State(state): State<AppState>) -> Json<SearchVocabulary> {
    let keywords = state
        .registry
        .defs()
        .iter()
        .map(|d| KeywordInfo {
            canonical: d.canonical.clone(),
            aliases: d.aliases.clone(),
            operators: d.operators.clone(),
            kind: d.kind.clone(),
            help: d.help.clone(),
        })
        .collect();
    let flags = state
        .flags
        .iter()
        .map(|f| FlagInfo {
            flag: f.flag.clone(),
            help: f.help.clone(),
        })
        .collect();
    Json(SearchVocabulary { keywords, flags })
}
