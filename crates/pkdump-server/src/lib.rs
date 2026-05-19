//! `pkdump-server` — the Axum HTTP application for PokeDumpster.
//!
//! Holds one user-database connection (with the shared catalog attached)
//! behind a mutex; requests are serialised, which is fine for a single-user
//! local server. The JSON API lives under `/api`; every other path is served
//! from the SvelteKit static build, falling back to `index.html` so the SPA
//! handles client-side routing.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use rusqlite::Connection;
use tower_http::services::{ServeDir, ServeFile};

use pkdump_db::DbError;

mod routes;

/// Shared application state: the single user-database connection, guarded by
/// a mutex.
#[derive(Clone)]
pub struct AppState {
    conn: Arc<Mutex<Connection>>,
}

/// An error rendered as an HTTP response. `DbError::NotFound` → 404,
/// `DbError::Conflict` → 409, `DbError::Import` → 400, everything else → 500.
pub struct AppError(StatusCode, String);

impl AppError {
    fn internal(msg: impl Into<String>) -> Self {
        AppError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<DbError> for AppError {
    fn from(e: DbError) -> Self {
        let code = match e {
            DbError::NotFound(_) => StatusCode::NOT_FOUND,
            DbError::Conflict(_) => StatusCode::CONFLICT,
            DbError::Import(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        AppError(code, e.to_string())
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        AppError::internal(format!("background task failed: {e}"))
    }
}

/// Run a blocking database closure on the connection without holding the
/// async executor. The closure receives `&mut Connection` so it works with
/// both read and write repository functions.
async fn blocking<T, F>(state: &AppState, f: F) -> Result<T, AppError>
where
    F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
    T: Send + 'static,
{
    let conn = state.conn.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut guard = conn.lock().expect("connection mutex poisoned");
        f(&mut guard)
    })
    .await?;
    Ok(result?)
}

/// Build the Axum router. `/health` and `/api/*` are handled by Rust; the
/// SvelteKit bundle under `/_app` and `/robots.txt` are served as files;
/// every other path returns the SPA's `index.html` so SvelteKit handles
/// client-side routing.
fn app(state: AppState, static_dir: PathBuf) -> Router {
    let index_html = std::fs::read_to_string(static_dir.join("index.html")).unwrap_or_else(|_| {
        "<!doctype html><title>PokeDumpster</title><body>\
             Frontend not built — run <code>npm run build</code> in frontend/.\
             </body>"
            .to_string()
    });
    let spa = move || {
        let html = index_html.clone();
        async move { axum::response::Html(html) }
    };
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .nest("/api", routes::api_router())
        .nest_service("/_app", ServeDir::new(static_dir.join("_app")))
        .route_service("/robots.txt", ServeFile::new(static_dir.join("robots.txt")))
        .with_state(state)
        .fallback(get(spa))
}

/// Start the HTTP server. Opens the user database (catalog attached) up
/// front — fails fast if the catalog is missing (`pkdump setup` not run).
pub async fn serve(
    user_db: PathBuf,
    shared_db: PathBuf,
    static_dir: PathBuf,
    host: IpAddr,
    port: u16,
) -> anyhow::Result<()> {
    let conn = pkdump_db::connect_user(&user_db, &shared_db)?;
    let state = AppState {
        conn: Arc::new(Mutex::new(conn)),
    };
    let addr = SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("pkdump serving on http://{addr}");
    axum::serve(listener, app(state, static_dir)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A test router whose catalog holds one printing (`sv3pt5-1-normal`) and
    /// whose static dir holds a stub `index.html`.
    fn test_app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = pkdump_db::open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-1-normal', 'sv3pt5-1', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = pkdump_db::connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let static_dir = dir.path().join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(
            static_dir.join("index.html"),
            "<!doctype html><title>PokeDumpster</title>",
        )
        .unwrap();
        let state = AppState {
            conn: Arc::new(Mutex::new(conn)),
        };
        (dir, app(state, static_dir))
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn health_responds() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn serves_spa_index_with_fallback() {
        let (_d, router) = test_app();

        // The index is served at the root.
        let root = router
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::OK);
        assert!(body_string(root).await.contains("PokeDumpster"));

        // An unknown (client-side) route falls back to index.html.
        let spa_route = router
            .oneshot(
                Request::builder()
                    .uri("/collection")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(spa_route.status(), StatusCode::OK);
        assert!(body_string(spa_route).await.contains("PokeDumpster"));
    }

    #[tokio::test]
    async fn collection_endpoints_round_trip() {
        let (_d, router) = test_app();

        let created = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/collection")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"printing_id":"sv3pt5-1-normal","source":"manual_id"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let listed = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/collection")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        assert!(body_string(listed).await.contains("sv3pt5-1-normal"));

        let bad = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/collection")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"printing_id":"sv3pt5-1-nope","source":"manual_id"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::NOT_FOUND);

        let deleted = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/collection/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn card_endpoint_returns_detail_and_404() {
        let (_d, router) = test_app();

        let found = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/card/sv3pt5/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(found.status(), StatusCode::OK);
        assert!(body_string(found).await.contains("Bulbasaur"));

        let missing = router
            .oneshot(
                Request::builder()
                    .uri("/api/card/sv3pt5/999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
