//! `/api/collection/search` and `/api/search/keywords` — the Scryfall-style
//! collection search (see architecture/SEARCH_QUERY_LANGUAGE.md §8).
//!
//! The query is parsed against the registry, compiled, and executed. A parse
//! error returns HTTP 400 with a JSON body `{error, position}` so the frontend
//! can place a caret. The keywords endpoint serves the registry so autocomplete
//! and the help page render from data, not a hardcoded list.
//!
//! The search response is a **page**, not a bare array: `{rows, total, limit,
//! offset}`, where `total` counts the whole result set. `limit` defaults to
//! [`search::DEFAULT_LIMIT`] when absent — bounded on purpose, because the
//! unbounded version of this endpoint shipped a 44 MB body and crashed the tab
//! (pd-jsby). A bad `limit`/`offset` is a 400 with `{error}` and no `position`,
//! which is how a client tells a paging complaint from a query-syntax one.
//!
//! `limit=all` asks for the whole result set instead of a page. It is not the
//! unbounded default coming back: it is a caller stating that it can hold the
//! result, which the collection page can now that it renders a window of the
//! DOM rather than all of it (pd-7z4o). See [`search::Slice`].

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pkdump_core::query::parse;
use pkdump_db::search::{self, SearchPage};

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
    /// `1` widens the result to the whole catalog (owned + unowned), backing
    /// the collection page's "All cards" toggle.
    include_unowned: Option<String>,
    /// Rows to return, `0..=MAX_LIMIT`, or the literal `all` for the whole
    /// result set. Absent means [`search::DEFAULT_LIMIT`] — the default is
    /// bounded on purpose (see the constant's own docs). Taken as a string so
    /// a bad value is our JSON 400 and not Axum's plain text rejection.
    limit: Option<String>,
    /// Rows to skip. Absent means 0. An offset past the end is an empty page
    /// with an honest `total`, not an error.
    offset: Option<String>,
}

/// A paging complaint: HTTP 400 with `error` and no `position`.
fn paging_error(message: String) -> AppError {
    AppError::bad_request(serde_json::json!({ "error": message }).to_string())
}

/// Parse `offset`. Anything that is not a whole number is refused — never
/// clamped, so a caller is told rather than quietly served a different page
/// than it asked for.
fn offset_bound(raw: Option<&str>) -> Result<u32, AppError> {
    let Some(raw) = raw else { return Ok(0) };
    raw.parse()
        .map_err(|_| paging_error("offset must be a whole number".to_string()))
}

/// The literal `limit` that asks for the whole result set rather than a page.
///
/// A word and not a very large number: the client would otherwise have to name
/// a bound it hopes exceeds the catalog, and be silently truncated on the day
/// it doesn't. See [`search::Slice::All`].
const LIMIT_ALL: &str = "all";

/// Which slice of the result the caller asked for.
fn slice_from(p: &SearchParams) -> Result<search::Slice, AppError> {
    let offset = offset_bound(p.offset.as_deref())?;
    if p.limit.as_deref() == Some(LIMIT_ALL) {
        if offset != 0 {
            return Err(paging_error(
                "offset has no meaning with limit=all — the whole result starts at 0".to_string(),
            ));
        }
        return Ok(search::Slice::All);
    }
    let Some(raw) = p.limit.as_deref() else {
        return Ok(search::Slice::Page {
            limit: search::DEFAULT_LIMIT,
            offset,
        });
    };
    let max = search::MAX_LIMIT;
    let refuse = || {
        paging_error(format!(
            "limit must be `{LIMIT_ALL}` or a whole number between 0 and {max}"
        ))
    };
    let limit: u32 = raw.parse().map_err(|_| refuse())?;
    if limit > max {
        return Err(refuse());
    }
    Ok(search::Slice::Page { limit, offset })
}

/// Run a search. An empty `q` is the default view (all owned printings).
async fn collection_search(
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> Result<Json<SearchPage>, AppError> {
    let slice = slice_from(&p)?;
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
    if p.include_unowned.as_deref() == Some("1") {
        compiled.set_catalog_wide(true);
    }
    compiled.override_order(p.sort.as_deref(), p.dir.as_deref());

    let page = blocking(&state, move |c| search::search_page(c, &compiled, slice)).await?;
    Ok(Json(page))
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
