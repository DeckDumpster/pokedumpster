//! `pkdump-server` — the Axum HTTP application for PokeDumpster.
//!
//! Currently a sanity homepage showing catalog counts. The JSON API under
//! `/api` and the SvelteKit static build are served from here as later
//! tasks land (PLAN.md §5).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{Router, extract::State, response::Html, routing::get};

#[derive(Clone)]
struct AppState {
    db_path: Arc<PathBuf>,
}

/// Catalog row counts shown on the homepage.
struct Counts {
    sets: i64,
    cards: i64,
    printings: i64,
}

/// Read catalog counts from the shared database (read-only).
fn catalog_counts(path: &Path) -> rusqlite::Result<Counts> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
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
    let path = state.db_path.clone();
    let counts = tokio::task::spawn_blocking(move || catalog_counts(&path))
        .await
        .ok()
        .and_then(Result::ok);
    Html(render_homepage(counts))
}

/// Build the Axum router.
fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(homepage))
        .route("/health", get(|| async { "ok" }))
        .with_state(state)
}

/// Start the HTTP server on `127.0.0.1:port`, reading the catalog at
/// `db_path`. Local-only by design — remote access goes through WireGuard.
pub async fn serve(db_path: PathBuf, port: u16) -> anyhow::Result<()> {
    let state = AppState {
        db_path: Arc::new(db_path),
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("pkdump serving on http://{addr}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn counts_reads_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.sqlite");
        {
            let conn = pkdump_db::open_shared(&path).unwrap();
            conn.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
        }
        let c = catalog_counts(&path).unwrap();
        assert_eq!(c.sets, 1);
        assert_eq!(c.cards, 0);
        assert_eq!(c.printings, 0);
    }

    #[tokio::test]
    async fn health_and_homepage_respond() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState {
            db_path: Arc::new(dir.path().join("missing.sqlite")),
        };
        let router = app(state);

        let health = router
            .clone()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        // Homepage responds 200 even with no catalog — it shows the
        // "run pkdump setup" message rather than erroring.
        let home = router
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(home.status(), StatusCode::OK);
    }

    #[test]
    fn homepage_renders_both_states() {
        assert!(render_homepage(None).contains("pkdump setup"));
        let loaded = render_homepage(Some(Counts {
            sets: 170,
            cards: 19000,
            printings: 52000,
        }));
        assert!(loaded.contains("170 sets"));
        assert!(loaded.contains("52000 printings"));
    }
}
