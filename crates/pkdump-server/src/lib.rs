//! `pkdump-server` — the Axum HTTP application for PokeDumpster.
//!
//! Holds one collection-database connection per tenant (with the shared
//! catalog attached) behind a mutex; a tenant's requests are serialised,
//! which is fine for a personal collection tracker. The JSON API lives under
//! `/api`; every other path is served from the SvelteKit static build,
//! falling back to `index.html` so the SPA handles client-side routing.
//!
//! Which tenant a request is served as is decided by [`tenant`], once, in a
//! middleware — and by default there is only one, exactly as before.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Router, middleware, routing::get};
use rusqlite::Connection;
use tower_http::services::{ServeDir, ServeFile};

use pkdump_core::query::KeywordRegistry;
use pkdump_db::DbError;
use pkdump_db::search_meta::SearchFlag;

mod routes;
pub mod tenant;

use tenant::Tenants;

/// Shared application state: the collection databases this process may serve
/// plus the immutable search registry/flags loaded once at startup.
///
/// Note what is *not* here: a connection. Reaching a database goes through
/// [`Tenants::connection`] with a [`tenant::TenantId`] that only the
/// resolution middleware can mint, so there is no ambient default collection
/// for a handler to pick up by accident.
#[derive(Clone)]
pub struct AppState {
    tenants: Arc<Tenants>,
    registry: Arc<KeywordRegistry>,
    flags: Arc<Vec<SearchFlag>>,
    /// The data dir — read by `/api/backup-status` for the `.backup-last-ok`
    /// freshness marker the host-side Layer 1 checker writes (ivq.5).
    data_dir: Arc<PathBuf>,
}

/// An error rendered as an HTTP response. `DbError::NotFound` → 404,
/// `DbError::Conflict` → 409, `DbError::Import` → 400, everything else → 500.
#[derive(Debug)]
pub struct AppError(StatusCode, String);

impl AppError {
    fn internal(msg: impl Into<String>) -> Self {
        AppError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
    }

    /// A 400 with a body the frontend parses for `{error, position}`.
    fn bad_request(body: impl Into<String>) -> Self {
        AppError(StatusCode::BAD_REQUEST, body.into())
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
///
/// This is the only route from a handler to a database, and it takes the
/// tenant from the request scope rather than from anything the handler
/// passes in — that is what makes cross-tenant access unreachable from
/// route code rather than merely unusual. A request that never went through
/// the resolution middleware errors here; it does not get a default.
async fn blocking<T, F>(state: &AppState, f: F) -> Result<T, AppError>
where
    F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
    T: Send + 'static,
{
    let id = tenant::current()?;
    let tenants = state.tenants.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = tenants.connection(&id)?;
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
fn app(state: AppState, static_dir: PathBuf, data_dir: PathBuf) -> Router {
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
    // `route_layer`, not `layer`: resolution runs for API routes that match,
    // and an unmatched `/api/...` path still falls through to the SPA
    // fallback exactly as it did before.
    let api = routes::api_router()
        .route_layer(middleware::from_fn_with_state(state.clone(), tenant::layer));
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .nest("/api", api)
        .nest_service("/_app", ServeDir::new(static_dir.join("_app")))
        // Static-asset directories carried in by adapter-static. Each needs
        // an explicit nest_service so they don't fall through to the SPA
        // fallback (which would return index.html and the browser would
        // render the icon as a broken HTML "image").
        .nest_service("/rarity", ServeDir::new(static_dir.join("rarity")))
        .nest_service("/energy", ServeDir::new(static_dir.join("energy")))
        .nest_service("/sets", ServeDir::new(static_dir.join("sets")))
        // Trimmed-and-resized set symbol glyphs written by the ingest
        // pipeline's symbols phase. Lives on the data volume so it
        // rebuilds from upstream alongside shared.sqlite, not in the
        // baked-in image static dir.
        .nest_service("/sym", ServeDir::new(data_dir.join("symbols")))
        .route_service("/robots.txt", ServeFile::new(static_dir.join("robots.txt")))
        .with_state(state)
        .fallback(get(spa))
}

/// Everything `pkdump serve` needs to stand the HTTP app up.
pub struct ServeConfig {
    /// The tenant this process serves when `multi_tenant` is off — i.e.
    /// `$PKDUMP_USER` — and the collection database it resolves to.
    pub tenant: String,
    pub user_db: PathBuf,
    /// The directory holding one database per tenant, and the registry that
    /// says which of them a handle is served from. Read only when
    /// `multi_tenant` is on; `tenant`/`user_db` are ignored in that mode.
    pub tenants_dir: PathBuf,
    pub registry_db: PathBuf,
    pub shared_db: PathBuf,
    pub static_dir: PathBuf,
    pub data_dir: PathBuf,
    pub host: IpAddr,
    pub port: u16,
    /// Per-request tenant resolution. **Off unless explicitly switched on.**
    /// There is no authentication in front of it, so an instance with this
    /// on lets any caller name any tenant — see `deploy/TENANTS.md`.
    pub multi_tenant: bool,
}

/// Start the HTTP server. In single-tenant mode the collection database is
/// opened up front — so a missing catalog (`pkdump setup` not run) fails at
/// startup rather than on the first request.
pub async fn serve(cfg: ServeConfig) -> anyhow::Result<()> {
    // Idempotent shared-catalog migration on startup. `pkdump setup`
    // and `pkdump data refresh` normally own shared schema, but a
    // binary upgrade can ship a data-only migration (e.g. seeding a
    // new variant) that must be applied before the server starts
    // serving requests. open_shared runs pending migrations and is a
    // no-op when nothing is pending.
    //
    // The search registry is read off the catalog here too: it is catalog
    // data, the same for every tenant, so it is loaded once from the one
    // shared database rather than through whichever collection happens to
    // be open.
    let (registry, flags) = {
        let mut shared = pkdump_db::open_shared(&cfg.shared_db)?;
        // Seed the search query metadata (keywords, rarity ranks, is:-flags)
        // on every startup. Unlike the upstream catalog, these are pure
        // embedded data (include_str!), so reconciling here — not just at
        // `pkdump setup`/`data refresh` — means a fresh deploy never serves an
        // empty keyword registry (which would reject every keyword query).
        pkdump_db::search_meta::reconcile(&mut shared)?;
        (
            Arc::new(pkdump_db::search_meta::load_registry(&shared)?),
            Arc::new(pkdump_db::search_meta::load_flags(&shared)?),
        )
    };
    let tenants = if cfg.multi_tenant {
        println!(
            "pkdump: MULTI-TENANT resolution is ON — every request names its tenant in \
             `{}`, and nothing authenticates that claim. Do not expose this instance.",
            tenant::TENANT_HEADER
        );
        Tenants::multi(cfg.tenants_dir, cfg.shared_db, &cfg.registry_db)?
    } else {
        Tenants::single(&cfg.tenant, cfg.user_db, cfg.shared_db)?
    };
    let state = AppState {
        tenants: Arc::new(tenants),
        registry,
        flags,
        data_dir: Arc::new(cfg.data_dir.clone()),
    };
    let addr = SocketAddr::new(cfg.host, cfg.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("pkdump serving on http://{addr}");
    axum::serve(listener, app(state, cfg.static_dir, cfg.data_dir)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    /// A catalog holding one printing (`sv3pt5-1-normal`), plus the static
    /// dir the router serves the SPA shell from.
    fn seed(dir: &std::path::Path) -> PathBuf {
        let shared = dir.join("shared.sqlite");
        let mut c = pkdump_db::open_shared(&shared).unwrap();
        pkdump_db::search_meta::reconcile(&mut c).unwrap();
        c.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('sv3pt5', 'MEW', '151', 'Scarlet & Violet')",
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

        let static_dir = dir.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(
            static_dir.join("index.html"),
            "<!doctype html><title>PokeDumpster</title>",
        )
        .unwrap();
        shared
    }

    fn router_for(dir: &std::path::Path, shared: &std::path::Path, tenants: Tenants) -> Router {
        let registry = {
            let c = pkdump_db::open_shared(shared).unwrap();
            Arc::new(pkdump_db::search_meta::load_registry(&c).unwrap())
        };
        let flags = {
            let c = pkdump_db::open_shared(shared).unwrap();
            Arc::new(pkdump_db::search_meta::load_flags(&c).unwrap())
        };
        let data_dir = dir.to_path_buf();
        let state = AppState {
            tenants: Arc::new(tenants),
            registry,
            flags,
            data_dir: Arc::new(data_dir.clone()),
        };
        app(state, dir.join("static"), data_dir)
    }

    /// A single-tenant test router — the production shape, and the one every
    /// pre-existing test below exercises.
    fn test_app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().unwrap();
        let shared = seed(dir.path());
        let tenants = Tenants::single(
            "collection",
            dir.path().join("tenants").join("collection.sqlite"),
            shared.clone(),
        )
        .unwrap();
        let router = router_for(dir.path(), &shared, tenants);
        (dir, router)
    }

    /// A multi-tenant test router with `handles` provisioned, as
    /// `pkdump tenant create` would leave them: a registry row per user, and
    /// one database per user named by the `database_id` that row issued —
    /// *not* by the handle.
    fn multi_tenant_app(handles: &[&str]) -> (tempfile::TempDir, Router, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let shared = seed(dir.path());
        let tenants_dir = dir.path().join("tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let registry_db = dir.path().join("registry.sqlite");
        let registry = pkdump_db::open_registry(&registry_db).unwrap();
        for handle in handles {
            let user = pkdump_db::registry::insert(&registry, handle).unwrap();
            pkdump_db::open_user(
                &pkdump_db::tenant_db_file(&tenants_dir, &user.database_id).unwrap(),
            )
            .unwrap();
        }
        let router = router_for(
            dir.path(),
            &shared,
            Tenants::multi(tenants_dir.clone(), shared.clone(), &registry_db).unwrap(),
        );
        (dir, router, tenants_dir)
    }

    /// `GET`/`POST`/`DELETE` helpers that optionally name a tenant.
    fn request(method: &str, uri: &str, tenant: Option<&str>, body: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = tenant {
            b = b.header(tenant::TENANT_HEADER, t);
        }
        match body {
            Some(json) => b
                .header("content-type", "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    }

    const ADD_CARD: &str = r#"{"printing_id":"sv3pt5-1-normal","source":"manual_id"}"#;

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
    async fn backup_status_no_marker_is_not_stale() {
        // No `.backup-last-ok` on the data dir (dev/test/unarmed Layer 1):
        // status reports unknown, never stale (the off-box monitor owns the
        // never-configured case, not the in-app banner).
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/backup-status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("\"last_ok_epoch\":null"), "body: {body}");
        assert!(body.contains("\"stale\":false"), "body: {body}");
    }

    #[tokio::test]
    async fn backup_status_old_marker_is_stale() {
        // A marker that exists but is far past the threshold flips `stale`.
        let (dir, router) = test_app();
        let old = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 100 * 3600; // 100h ago — well past the 12h default threshold
        std::fs::write(dir.path().join(".backup-last-ok"), old.to_string()).unwrap();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/backup-status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("\"stale\":true"), "body: {body}");
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

    #[tokio::test]
    async fn search_owned_and_missing() {
        let (_d, router) = test_app();

        // Add a copy so the default (owned) search returns it.
        router
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

        // Empty query → owned default view includes the owned printing.
        let owned = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/collection/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(owned.status(), StatusCode::OK);
        let body = body_string(owned).await;
        assert!(body.contains("sv3pt5-1-normal"), "owned search: {body}");
        assert!(body.contains("\"owned\":true"), "owned flag: {body}");

        // A card-level filter that excludes it returns nothing in owned mode.
        let none = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/collection/search?q=t:fire")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(none.status(), StatusCode::OK);
        assert_eq!(body_string(none).await.trim(), "[]");
    }

    #[tokio::test]
    async fn search_rejects_unknown_keyword_with_position() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/collection/search?q=xyz:1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_string(resp).await;
        assert!(body.contains("position"), "expected position in {body}");
        assert!(body.contains("xyz"), "expected keyword in {body}");
    }

    #[tokio::test]
    async fn search_keywords_endpoint_serves_registry() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/search/keywords")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("energy_type"), "keywords: {body}");
        assert!(body.contains("holo"), "flags: {body}");
    }

    #[tokio::test]
    async fn export_json_serves_a_downloadable_envelope() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/export/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains("pokedumpster-collection.json")
        );

        let envelope: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(
            envelope["format"],
            serde_json::Value::from(pkdump_db::json_backup::FORMAT)
        );
        // Collection data is in; catalog data (served from the attached
        // read-only shared database) is not.
        assert!(envelope.get("collection").is_some());
        assert!(envelope.get("printings").is_none());
    }

    // ---------------------------------------------------------------------
    // Tenant resolution and isolation.
    //
    // These assert the NEGATIVE. "Alice can see Alice's cards" is not the
    // property that matters and would pass just as happily with no resolver
    // at all; what matters is that nothing Bob sends reaches Alice's
    // collection. Each test below is written so that removing or bypassing
    // the resolver breaks it — verified by mutation, see `pd-5emg`.
    // ---------------------------------------------------------------------

    /// **The load-bearing test.** A request resolved as Bob cannot read, and
    /// cannot destroy, anything belonging to Alice.
    ///
    /// Break the resolver — have `Tenants::resolve` ignore the header and
    /// return a fixed tenant, or have `blocking` reach for some ambient
    /// connection — and Bob's list comes back holding Alice's card, or his
    /// DELETE succeeds. Either way this fails.
    #[tokio::test]
    async fn one_tenant_cannot_reach_another_tenants_collection() {
        let (_d, router, _dir) = multi_tenant_app(&["alice", "bob"]);

        // Alice registers a card.
        let created = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some("alice"),
                Some(ADD_CARD),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        // Bob's collection is empty. This is the assertion the whole epic
        // is for.
        let bobs = router
            .clone()
            .oneshot(request("GET", "/api/collection", Some("bob"), None))
            .await
            .unwrap();
        assert_eq!(bobs.status(), StatusCode::OK);
        let body = body_string(bobs).await;
        assert_eq!(
            body.trim(),
            "[]",
            "bob's collection must not contain alice's card: {body}"
        );

        // Nor can Bob reach it by id — the row exists, but not for him.
        let stolen = router
            .clone()
            .oneshot(request("DELETE", "/api/collection/1", Some("bob"), None))
            .await
            .unwrap();
        assert_eq!(
            stolen.status(),
            StatusCode::NOT_FOUND,
            "bob deleted a row out of alice's collection"
        );

        // And Alice still has it — the negative above is not just "nobody
        // can see anything".
        let alices = router
            .oneshot(request("GET", "/api/collection", Some("alice"), None))
            .await
            .unwrap();
        assert!(body_string(alices).await.contains("sv3pt5-1-normal"));
    }

    /// Multi-tenant with no tenant named is refused. There is no ambient
    /// tenant to fall back to — falling back would serve one person's
    /// collection to an anonymous caller.
    #[tokio::test]
    async fn a_request_that_names_no_tenant_is_refused() {
        let (_d, router, _dir) = multi_tenant_app(&["alice"]);
        router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some("alice"),
                Some(ADD_CARD),
            ))
            .await
            .unwrap();

        let anon = router
            .oneshot(request("GET", "/api/collection", None, None))
            .await
            .unwrap();
        assert_eq!(anon.status(), StatusCode::BAD_REQUEST);
        let body = body_string(anon).await;
        assert!(
            !body.contains("sv3pt5-1-normal"),
            "an unresolved request was served a collection: {body}"
        );
    }

    /// Naming a handle nobody registered is a 404 — it does not provision
    /// one. A resolver that opened whatever it was handed would let any
    /// caller create tenants by guessing names.
    #[tokio::test]
    async fn an_unknown_tenant_is_a_404_and_creates_nothing() {
        let (_d, router, tenants_dir) = multi_tenant_app(&["alice"]);
        let before = std::fs::read_dir(&tenants_dir).unwrap().count();
        let resp = router
            .oneshot(request("GET", "/api/collection", Some("mallory"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(std::fs::read_dir(&tenants_dir).unwrap().count(), before);
    }

    /// **`pd-rqgv`, end to end.** The header is a lookup key, so a handle
    /// that names a *file* — one in `tenants/`, or one reached by climbing
    /// out of it — resolves to nothing. There is no path to escape from,
    /// because no path is built from what the header carries.
    ///
    /// The first case is the mutation canary: `ghost.sqlite` is a real
    /// collection with a card in it, and under the old
    /// `dir.join(format!("{name}.sqlite"))` the header `ghost` would have
    /// been served that card.
    #[tokio::test]
    async fn a_handle_that_names_a_file_resolves_to_nothing() {
        let (_d, router, tenants_dir) = multi_tenant_app(&["alice"]);

        // A database in `tenants/` that the registry does not know about.
        let ghost = tenants_dir.join("ghost.sqlite");
        pkdump_db::open_user(&ghost).unwrap();

        for handle in ["ghost", "../shared", "../../etc/passwd", "alice/../ghost"] {
            let resp = router
                .clone()
                .oneshot(request("GET", "/api/collection", Some(handle), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{handle:?}");
        }
    }

    /// A user's `database_id` is not a second way in. Only the `handle`
    /// column resolves, so knowing where someone's bytes live does not let
    /// a caller ask to be served from them.
    #[tokio::test]
    async fn a_database_id_is_not_a_handle() {
        let (d, router, _dir) = multi_tenant_app(&["alice"]);
        let registry = pkdump_db::open_registry(&d.path().join("registry.sqlite")).unwrap();
        let alice = pkdump_db::registry::lookup(&registry, "alice")
            .unwrap()
            .unwrap();

        let resp = router
            .oneshot(request(
                "GET",
                "/api/collection",
                Some(&alice.database_id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// **Multitenancy is invisible when it is off.** The default build reads
    /// no tenant header at all, so a request cannot switch collections by
    /// sending one — the opt-in flag is the only switch there is.
    #[tokio::test]
    async fn with_the_flag_off_the_header_does_nothing() {
        let (_d, router) = test_app();

        // Register a card without naming a tenant, as the SPA does.
        let created = router
            .clone()
            .oneshot(request("POST", "/api/collection", None, Some(ADD_CARD)))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        // The same instance, asked for some other tenant, serves the one
        // collection it has — it does not 404, and it does not switch.
        let claimed = router
            .oneshot(request("GET", "/api/collection", Some("bob"), None))
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK);
        assert!(body_string(claimed).await.contains("sv3pt5-1-normal"));
    }

    /// Every route reaches its database through `blocking`, which takes the
    /// tenant from the request scope. Outside a resolved request there is no
    /// connection to be had — not a default one, none.
    #[tokio::test]
    async fn there_is_no_database_outside_a_resolved_request() {
        let dir = tempfile::tempdir().unwrap();
        let shared = seed(dir.path());
        let state = AppState {
            tenants: Arc::new(
                Tenants::single(
                    "collection",
                    dir.path().join("tenants").join("collection.sqlite"),
                    shared,
                )
                .unwrap(),
            ),
            registry: Arc::new(Default::default()),
            flags: Arc::new(Vec::new()),
            data_dir: Arc::new(dir.path().to_path_buf()),
        };

        let unscoped = blocking(&state, |c| {
            Ok(c.query_row("SELECT count(*) FROM collection", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .await;
        assert!(unscoped.is_err(), "a connection without a resolved tenant");

        // In scope, the same call works — the failure above is the missing
        // tenant, not a broken query.
        let scoped = tenant::test_support::as_tenant(
            "collection",
            blocking(&state, |c| {
                Ok(c.query_row("SELECT count(*) FROM collection", [], |r| {
                    r.get::<_, i64>(0)
                })?)
            }),
        )
        .await;
        assert_eq!(scoped.unwrap(), 0);
    }
}
