//! HTTP route modules. One module per API resource (PLAN.md §5.2).

pub mod backup;
pub mod batches;
pub mod binders;
pub mod card;
pub mod collection;
pub mod conditions;
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

/// `/api` routes that require a Cloudflare Access JWT.
///
/// Every route is here unless it appears in [`public_api_router`].
pub fn authenticated_api_router() -> Router<AppState> {
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
        .merge(conditions::routes())
        .merge(orders::routes())
        .merge(wishlist::routes())
        .merge(batches::routes())
        .merge(manual_prices::routes())
        .merge(user_printings::routes())
        .merge(import::routes())
        .merge(export::routes())
}

/// `/api` routes that are exempt from authentication.
///
/// Qualifies for this list: an endpoint that exposes no tenant data and no
/// information useful to an attacker. Every route here must be explicitly
/// reviewed before being added; this is not a dumping ground.
///
/// Current members:
///   - `/backup-status` — backup freshness timestamps only; no tenant data;
///     read by `alarm-status.sh` from plain localhost without a credential.
pub fn public_api_router() -> Router<AppState> {
    Router::new().merge(backup::routes())
}

