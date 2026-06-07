//! HTTP route modules. One module per API resource (PLAN.md §5.2).

pub mod backup;
pub mod batches;
pub mod binders;
pub mod card;
pub mod collection;
pub mod decks;
pub mod export;
pub mod import;
pub mod manual_prices;
pub mod orders;
pub mod sealed;
pub mod search;
pub mod sets;
pub mod user_printings;
pub mod variants;
pub mod wishlist;

use axum::Router;

use crate::AppState;

/// The full `/api` router: collection CRUD, card lookups, set catalog,
/// binders, decks, sealed products, orders, wishlist, batches.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .nest(
            "/collection",
            collection::routes().merge(search::collection_routes()),
        )
        .merge(search::vocabulary_routes())
        .merge(card::routes())
        .merge(sets::routes())
        .merge(binders::routes())
        .merge(decks::routes())
        .merge(sealed::routes())
        .merge(variants::routes())
        .merge(orders::routes())
        .merge(wishlist::routes())
        .merge(batches::routes())
        .merge(manual_prices::routes())
        .merge(user_printings::routes())
        .merge(import::routes())
        .merge(export::routes())
        .merge(backup::routes())
}
