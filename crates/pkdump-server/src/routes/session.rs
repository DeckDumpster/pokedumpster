//! `/api/session` — who the app signed in, so the frontend can offer "Sign out" only where
//! there is a session to end. `signed_in_as` is `null` when native sign-in is not configured.

use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;

use crate::AppState;
use crate::oidc::SessionUser;

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Session {
    pub signed_in_as: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/session", get(session))
}

async fn session(user: Option<Extension<SessionUser>>) -> Json<Session> {
    Json(Session {
        signed_in_as: user.map(|Extension(u)| u.email),
    })
}
