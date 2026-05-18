//! `pkdump-server` — the Axum HTTP application for PokeDumpster.
//!
//! Holds one user-database connection (with the shared catalog attached)
//! behind a mutex; requests are serialised, which is fine for a single-user
//! local server. The JSON API lives under `/api` (PLAN.md §5).

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::{Router, extract::State, routing::get};
use rusqlite::Connection;

use pkdump_db::DbError;

mod routes;

/// Shared application state: the single user-database connection, guarded by
/// a mutex.
#[derive(Clone)]
pub struct AppState {
    conn: Arc<Mutex<Connection>>,
}

/// An error rendered as an HTTP response. `DbError::NotFound` → 404,
/// `DbError::Conflict` → 409, everything else → 500.
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

/// Catalog row counts shown on the homepage.
struct Counts {
    sets: i64,
    cards: i64,
    printings: i64,
}

fn catalog_counts(conn: &Connection) -> rusqlite::Result<Counts> {
    let count = |table: &str| -> rusqlite::Result<i64> {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
    };
    Ok(Counts {
        sets: count("sets")?,
        cards: count("cards")?,
        printings: count("printings")?,
    })
}

fn render_homepage(counts: Option<Counts>) -> String {
    let body = match counts {
        Some(c) => format!(
            "<p>Catalog loaded:</p>\
             <ul><li>{} sets</li><li>{} cards</li><li>{} printings</li></ul>",
            c.sets, c.cards, c.printings,
        ),
        None => "<p>Catalog not built yet — run <code>pkdump setup</code>.</p>".to_string(),
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>PokeDumpster</title></head>\
         <body><h1>PokeDumpster</h1>{body}</body></html>"
    )
}

async fn homepage(State(state): State<AppState>) -> Html<String> {
    let counts = blocking(&state, |conn| Ok(catalog_counts(conn).ok()))
        .await
        .ok()
        .flatten();
    Html(render_homepage(counts))
}

/// Build the Axum router.
fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(homepage))
        .route("/health", get(|| async { "ok" }))
        .nest("/api/collection", routes::collection::routes())
        .with_state(state)
}

/// Start the HTTP server. Opens the user database (catalog attached) up
/// front — fails fast if the catalog is missing (`pkdump setup` not run).
pub async fn serve(
    user_db: PathBuf,
    shared_db: PathBuf,
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
    axum::serve(listener, app(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A test AppState whose catalog holds one printing, `sv3pt5-1-normal`.
    fn test_state() -> (tempfile::TempDir, AppState) {
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
        (
            dir,
            AppState {
                conn: Arc::new(Mutex::new(conn)),
            },
        )
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn catalog_counts_reads_attached_catalog() {
        let (_d, state) = test_state();
        let conn = state.conn.lock().unwrap();
        let c = catalog_counts(&conn).unwrap();
        assert_eq!(c.sets, 1);
        assert_eq!(c.cards, 1);
        assert_eq!(c.printings, 1);
    }

    #[tokio::test]
    async fn health_and_homepage_respond() {
        let (_d, state) = test_state();
        let router = app(state);

        let health = router
            .clone()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let home = router
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(home.status(), StatusCode::OK);
        assert!(body_string(home).await.contains("1 sets"));
    }

    #[tokio::test]
    async fn collection_endpoints_round_trip() {
        let (_d, state) = test_state();
        let router = app(state);

        // POST a copy.
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

        // GET the list — the copy is there.
        let listed = router
            .clone()
            .oneshot(Request::builder().uri("/api/collection").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        assert!(body_string(listed).await.contains("sv3pt5-1-normal"));

        // POST a copy with an unknown printing — 404.
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

        // DELETE entry 1.
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
}
