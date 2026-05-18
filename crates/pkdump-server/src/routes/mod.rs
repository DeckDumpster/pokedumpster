//! HTTP route modules. One module per API resource (PLAN.md §5.2).

pub mod card;
pub mod collection;

use axum::Router;

use crate::AppState;

/// The full `/api` router: collection CRUD plus card lookups.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .nest("/collection", collection::routes())
        .merge(card::routes())
}
