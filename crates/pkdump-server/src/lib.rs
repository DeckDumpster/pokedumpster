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
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate, SizeAbove};
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

    /// A 400 whose body is JSON the frontend reads: `{error, position}` for a
    /// query-language parse error, `{error}` alone for everything else.
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

/// Responses smaller than this are sent uncompressed. (A response of exactly
/// this size is compressed — `SizeAbove` is inclusive — and a response whose
/// size is not known up front is compressed too, since there is nothing to
/// compare.)
///
/// Compression is not free — it costs CPU on this box and a decompress on the
/// client, and it replaces a known `content-length` with a chunked body. Under
/// ~1 KB there is nothing to buy with that: the response already fits inside a
/// single ~1460-byte TCP segment, so squeezing it does not remove a round trip,
/// and gzip's own header/trailer can make a very short body *larger*. 1 KB is
/// the first size where the saving is real, and it leaves every small JSON
/// answer in the API (`/api/backup-status`, a 204, an error envelope)
/// untouched.
///
/// The big payloads this exists for are three to four orders of magnitude
/// above it — the catalog-wide `/api/collection/search` is ~44 MB raw — so the
/// exact threshold only decides the fate of responses where it does not matter.
const COMPRESS_MIN_BYTES: u16 = 1024;

/// Response compression for the whole app.
///
/// Every response left this process uncompressed until now, including the
/// catalog-wide search body. JSON of that shape compresses about 9x, which is
/// the difference between a result set the browser can hold and one it cannot.
///
/// Three decisions are baked in here:
///
/// * **Algorithms: gzip and br, chosen by the client.** The layer reads
///   `Accept-Encoding` and picks; it never forces one. Brotli usually beats
///   gzip on JSON and every current browser offers it, while gzip is the
///   universal floor for anything else (`curl`, a script, an old client). A
///   request that offers neither — or no `Accept-Encoding` at all — gets the
///   same valid uncompressed bytes it got before. zstd and deflate are
///   deliberately absent: deflate is redundant with gzip and nothing prefers
///   it, and zstd's marginal win over br does not pay for a second native
///   dependency.
/// * **Never images.** `/sym`, `/rarity` and the rest serve PNG and JPEG,
///   which are already compressed; running them through gzip spends CPU to
///   make the payload *bigger*. `NotForContentType::IMAGES` excludes anything
///   `image/*`, which sweeps up `image/svg+xml` too — those are text and would
///   compress well, but every SVG shipped here is under a kilobyte and would
///   fall below the size floor anyway.
/// * **A size floor.** See [`COMPRESS_MIN_BYTES`].
///
/// It sits on the outermost router, so it covers the SPA shell and the
/// SvelteKit bundle under `/_app` as well as `/api`. Nothing here buffers a
/// whole body to compress it — the layer wraps the body and compresses as it
/// streams — so `/api/export/*`, which hands back a full collection dump,
/// costs no more memory than it did before.
// The predicate type is an opaque `And` tower of the four rules below; naming
// it buys nothing. `Clone`/`Send`/`Sync` come from `Predicate` itself.
fn compression() -> CompressionLayer<impl Predicate + 'static> {
    CompressionLayer::new().gzip(true).br(true).compress_when(
        SizeAbove::new(COMPRESS_MIN_BYTES)
            .and(NotForContentType::IMAGES)
            .and(NotForContentType::SSE)
            .and(NotForContentType::GRPC),
    )
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
        // Outermost, so it sees the final response of every route above.
        .layer(compression())
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
    /// The second opt-in that lets `multi_tenant` bind somewhere other than
    /// loopback. Off unless explicitly set; see `check_bind`.
    pub allow_insecure_bind: bool,
}

/// Refuse the one combination that has no defence: per-request tenant
/// resolution, reachable from off-box.
///
/// `multi_tenant` takes the tenant from a header nothing authenticates, so on
/// a non-loopback bind every collection belongs to whoever can reach the
/// port. Every other guardrail around that flag — a default of off, an env
/// parse that rejects `0`, a `deploy/` that never sets it — is a convention
/// plus a printed warning. This is the mechanism: the process does not start.
///
/// Whoever genuinely wants that combination later (behind a reverse proxy
/// that does authenticate, say) says so a second time, explicitly, with
/// `PKDUMP_MULTITENANT_INSECURE_BIND`.
///
/// Single-tenant mode is not touched at any address — the tenant is fixed at
/// startup, no request can change it, and the container deployment binds
/// `0.0.0.0`.
fn check_bind(multi_tenant: bool, host: IpAddr, allow_insecure_bind: bool) -> anyhow::Result<()> {
    if !multi_tenant || host.is_loopback() || allow_insecure_bind {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to start: multi-tenant resolution is on and --host {host} is not loopback.\n\
         \n\
         In multi-tenant mode the tenant is whatever the request's `{}` header claims, and \
         nothing authenticates that claim — there is no login, no session, no token. Bound \
         anywhere but loopback, this process hands every tenant's collection to anyone who can \
         reach the port and name a tenant.\n\
         \n\
         Bind 127.0.0.1 (or ::1), or drop --multi-tenant / PKDUMP_MULTITENANT. If you really do \
         mean to expose it — behind something that authenticates for it — say so a second time \
         with PKDUMP_MULTITENANT_INSECURE_BIND=1.",
        tenant::TENANT_HEADER
    )
}

/// Start the HTTP server. In single-tenant mode the collection database is
/// opened up front — so a missing catalog (`pkdump setup` not run) fails at
/// startup rather than on the first request.
pub async fn serve(cfg: ServeConfig) -> anyhow::Result<()> {
    // Before anything is opened or bound: an unauthenticated resolver must
    // not become reachable from off-box.
    check_bind(cfg.multi_tenant, cfg.host, cfg.allow_insecure_bind)?;
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
        if !cfg.host.is_loopback() {
            // Reachable only via the second opt-in — `check_bind` above
            // refused otherwise.
            println!(
                "pkdump: PKDUMP_MULTITENANT_INSECURE_BIND is set and this instance is bound to \
                 {} — every tenant's collection is readable and writable by anyone who can \
                 reach this port.",
                cfg.host
            );
        }
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

    // ---- response compression (pd-2r0p) ----------------------------------
    //
    // Every response left this process uncompressed before `compression()`
    // landed. These five tests pin the whole contract: what gets compressed,
    // what deliberately does not, and that compressing never changes the
    // bytes the client ends up with.

    async fn body_bytes(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    /// `GET uri`, optionally offering `Accept-Encoding`. Returns the response
    /// with its `content-encoding` (as an owned `String`) alongside the body,
    /// because reading the body consumes the response.
    async fn get_encoding(router: &Router, uri: &str, accept: Option<&str>) -> (String, Vec<u8>) {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = accept {
            b = b.header(header::ACCEPT_ENCODING, a);
        }
        let resp = router
            .clone()
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "uri: {uri}");
        let encoding = resp
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (encoding, body_bytes(resp).await)
    }

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn unbrotli(bytes: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let mut out = Vec::new();
        brotli::Decompressor::new(bytes, 4096)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    /// The keyword registry — the smallest endpoint in the seeded catalog
    /// that clears [`COMPRESS_MIN_BYTES`], and JSON of exactly the shape the
    /// big search payload is made of.
    const BULKY_JSON: &str = "/api/search/keywords";

    #[tokio::test]
    async fn json_gzips_and_decodes_to_the_uncompressed_bytes() {
        let (_d, router) = test_app();

        let (plain_encoding, plain) = get_encoding(&router, BULKY_JSON, None).await;
        // A client that asks for nothing still gets valid, unencoded JSON.
        assert_eq!(plain_encoding, "");
        serde_json::from_slice::<serde_json::Value>(&plain).unwrap();
        assert!(
            plain.len() > COMPRESS_MIN_BYTES as usize,
            "fixture too small to exercise the threshold: {} bytes",
            plain.len()
        );

        let (encoding, compressed) = get_encoding(&router, BULKY_JSON, Some("gzip")).await;
        assert_eq!(encoding, "gzip");
        assert!(
            compressed.len() < plain.len(),
            "gzip grew the body: {} -> {}",
            plain.len(),
            compressed.len()
        );
        // The point of the whole change: same bytes, fewer of them on the wire.
        assert_eq!(gunzip(&compressed), plain);
    }

    #[tokio::test]
    async fn brotli_is_used_when_the_client_offers_it() {
        // Accept-Encoding is honoured, not overridden: offered both, the layer
        // picks br, and a client that only speaks gzip still gets gzip.
        let (_d, router) = test_app();
        let (_, plain) = get_encoding(&router, BULKY_JSON, None).await;

        let (encoding, compressed) =
            get_encoding(&router, BULKY_JSON, Some("gzip, deflate, br")).await;
        assert_eq!(encoding, "br");
        assert_eq!(unbrotli(&compressed), plain);
    }

    #[tokio::test]
    async fn responses_below_the_threshold_are_not_compressed() {
        // `/api/backup-status` is a handful of fields; compressing it would
        // cost CPU and a chunked body to save nothing.
        let (_d, router) = test_app();
        let (encoding, body) = get_encoding(&router, "/api/backup-status", Some("gzip, br")).await;
        assert!(
            body.len() < COMPRESS_MIN_BYTES as usize,
            "no longer a small response: {} bytes",
            body.len()
        );
        assert_eq!(encoding, "");
        serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    }

    #[tokio::test]
    async fn images_are_never_recompressed() {
        // Set symbols are PNG — already compressed. Re-encoding them spends
        // CPU to make the payload bigger. This one is deliberately far above
        // the size floor and trivially compressible, so if the content-type
        // rule were dropped the layer would certainly compress it.
        let (dir, router) = test_app();
        let symbols = dir.path().join("symbols");
        std::fs::create_dir_all(&symbols).unwrap();
        let png = vec![b'P'; 8 * 1024];
        std::fs::write(symbols.join("sv3pt5.png"), &png).unwrap();

        let (encoding, body) = get_encoding(&router, "/sym/sv3pt5.png", Some("gzip, br")).await;
        assert_eq!(encoding, "");
        assert_eq!(body, png);
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
        let body = body_string(none).await;
        assert!(body.contains("\"rows\":[]"), "no matches: {body}");
        assert!(body.contains("\"total\":0"), "no matches: {body}");
    }

    /// pd-jsby. The body is a page envelope, not a bare array, and the page it
    /// describes is bounded even when the caller names no bounds.
    #[tokio::test]
    async fn search_returns_a_page_envelope_with_a_bounded_default() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/collection/search?include_unowned=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(body["total"], 1, "total counts the whole result set");
        assert_eq!(
            body["limit"],
            pkdump_db::search::DEFAULT_LIMIT,
            "an absent limit is the bounded default, not unbounded"
        );
        assert_eq!(body["offset"], 0);
    }

    /// The page bounds reach the query, and `total` stays the size of the whole
    /// result rather than of the page returned.
    #[tokio::test]
    async fn search_honours_limit_and_offset() {
        let (_d, router) = test_app();
        let page = |uri: &'static str| {
            let router = router.clone();
            async move {
                let resp = router
                    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            }
        };

        let empty = page("/api/collection/search?include_unowned=1&limit=0").await;
        assert!(empty["rows"].as_array().unwrap().is_empty());
        assert_eq!(empty["total"], 1, "limit=0 is a count-only request");

        let past_end = page("/api/collection/search?include_unowned=1&limit=10&offset=5").await;
        assert!(past_end["rows"].as_array().unwrap().is_empty());
        assert_eq!(
            past_end["total"], 1,
            "an offset past the end is not an error"
        );
        assert_eq!(past_end["limit"], 10);
        assert_eq!(past_end["offset"], 5);
    }

    /// pd-7z4o. `limit=all` is the whole result set, and says so in the
    /// envelope: the echoed `limit` is the row count, which is what makes the
    /// response describe itself as unpaged rather than as a page that happened
    /// to fit.
    #[tokio::test]
    async fn search_serves_the_whole_result_for_limit_all() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/collection/search?include_unowned=1&limit=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let rows = body["rows"].as_array().unwrap().len();
        assert_eq!(body["total"], rows, "every matching row was served");
        assert_eq!(body["limit"], rows, "the echoed limit is what was served");
        assert_eq!(body["offset"], 0);
    }

    /// Skipping rows out of a result you asked for in full is a contradiction,
    /// so the pair is refused rather than one half of it quietly winning.
    #[tokio::test]
    async fn search_refuses_an_offset_alongside_limit_all() {
        let (_d, router) = test_app();
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/collection/search?limit=all&offset=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(body["error"].as_str().unwrap().contains("limit=all"));
        assert!(body["position"].is_null(), "not a query error");
    }

    /// A paging bound that is not a whole number in range is refused — never
    /// honoured, never clamped to something the caller did not ask for. The 400
    /// body carries `error` and no `position`, which is how the client tells a
    /// paging complaint from a query-syntax one.
    #[tokio::test]
    async fn search_refuses_bad_paging_bounds() {
        let (_d, router) = test_app();
        let over_max = pkdump_db::search::MAX_LIMIT + 1;
        for uri in [
            "/api/collection/search?limit=-1".to_string(),
            "/api/collection/search?limit=abc".to_string(),
            "/api/collection/search?limit=1.5".to_string(),
            format!("/api/collection/search?limit={over_max}"),
            "/api/collection/search?limit=99999999999999999999".to_string(),
            "/api/collection/search?offset=-1".to_string(),
            "/api/collection/search?offset=nope".to_string(),
        ] {
            let resp = router
                .clone()
                .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
            assert!(body["error"].is_string(), "{uri}: {body}");
            assert!(body["position"].is_null(), "not a query error: {uri}");
        }
    }

    /// `value` and `adj` left the sort surface for good (pd-tjym): each ordered
    /// through a subquery joining the tenant's `collection` to the shared
    /// catalog's prices across the `ATTACH` boundary, and SQLite cannot index
    /// across attached databases — so ordering by either had to materialise and
    /// sort the whole match set before `LIMIT` could discard any of it.
    ///
    /// A client still asking is **told**. Falling back to name order would hand
    /// it a result that looks sorted and isn't, which is exactly how a caller
    /// stops noticing it is asking for a column that no longer exists.
    #[tokio::test]
    async fn search_refuses_a_sort_it_cannot_satisfy() {
        let (_d, router) = test_app();
        for key in ["value", "adj", "nonsense"] {
            let uri = format!("/api/collection/search?sort={key}");
            let resp = router
                .clone()
                .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
            let message = body["error"].as_str().unwrap_or_default();
            // The refusal names what IS sortable — a bare "bad sort" leaves the
            // caller guessing at a vocabulary it cannot see.
            for named in pkdump_db::search::SORT_KEYS {
                assert!(message.contains(named), "{uri}: {message} omits {named}");
            }
            assert!(
                body["position"].is_null(),
                "a sort complaint is not a query-syntax error: {uri}"
            );
        }
    }

    /// And every key the endpoint advertises is served, so the refusal above is
    /// about the key rather than about `sort=` having stopped being read.
    #[tokio::test]
    async fn search_serves_every_advertised_sort_key() {
        let (_d, router) = test_app();
        for key in pkdump_db::search::SORT_KEYS {
            let uri = format!("/api/collection/search?sort={key}&include_unowned=1");
            let resp = router
                .clone()
                .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        }
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
    /// `ghost` is the mutation canary: `ghost.sqlite` is a real collection,
    /// and under the old `dir.join(format!("{name}.sqlite"))` that header
    /// would have been served it. It is a perfectly well-formed handle, so
    /// the boundary check is not what stops it — the lookup is, and this is
    /// the 404 half of the distinction `pd-4g7c` draws.
    #[tokio::test]
    async fn a_handle_that_names_a_file_resolves_to_nothing() {
        let (_d, router, tenants_dir) = multi_tenant_app(&["alice"]);

        // A database in `tenants/` that the registry does not know about.
        let ghost = tenants_dir.join("ghost.sqlite");
        pkdump_db::open_user(&ghost).unwrap();

        let resp = router
            .clone()
            .oneshot(request("GET", "/api/collection", Some("ghost"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // The traversing ones do not even get that far: they are not handles.
        for handle in ["../shared", "../../etc/passwd", "alice/../ghost"] {
            let resp = router
                .clone()
                .oneshot(request("GET", "/api/collection", Some(handle), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{handle:?}");
        }
    }

    /// **`pd-4g7c`, over HTTP.** The two refusals are different answers, and
    /// a client can tell them apart: a header that is not a handle is a 400
    /// naming the rule, and a handle nobody holds is a 404.
    ///
    /// Asserted here rather than only against `Tenants::resolve` because the
    /// status code is the deliverable — a 400 the middleware swallowed into a
    /// 404 on the way out would satisfy the unit test and fail the caller.
    /// Remove the `validate_tenant_name` call in `tenant::resolve` and the
    /// first half of this becomes 404s.
    #[tokio::test]
    async fn a_malformed_handle_is_a_400_and_an_unknown_one_a_404() {
        let (d, router, _dir) = multi_tenant_app(&["alice"]);

        for malformed in ["Alice", "-flag", "a/b", "alice.sqlite", "has space"] {
            let resp = router
                .clone()
                .oneshot(request("GET", "/api/collection", Some(malformed), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{malformed:?}");
            let body = body_string(resp).await;
            assert!(
                body.contains(pkdump_db::HANDLE_RULE),
                "the 400 must say what a handle may be: {body}"
            );
            assert!(
                !body.contains(malformed),
                "the 400 echoed the header back: {body}"
            );
        }

        // Well-formed and unregistered, well-formed and detached: both 404,
        // and neither is a 400. A detached handle is a name that WAS held, so
        // it is the sharpest case that the two answers are decided by
        // different questions.
        let registry = pkdump_db::open_registry(&d.path().join("registry.sqlite")).unwrap();
        pkdump_db::registry::detach(&registry, "alice").unwrap();
        for known_shaped in ["mallory", "alice"] {
            let resp = router
                .clone()
                .oneshot(request("GET", "/api/collection", Some(known_shaped), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{known_shaped:?}");
        }
    }

    /// A user's `database_id` is not a second way in. Only the `handle`
    /// column resolves, so knowing where someone's bytes live does not let
    /// a caller ask to be served from them — and a ULID is not even a
    /// well-formed handle, so it is refused before the lookup.
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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

    // ---------------------------------------------------------------------
    // Refusing an exposed unauthenticated resolver (`check_bind`).
    //
    // The flag defaulting to off, the env parse that rejects "0", and the
    // startup warning are all conventions. These four assert the mechanism:
    // the process does not start. Delete the `anyhow::bail!` in `check_bind`
    // and `multi_tenant_refuses_a_non_loopback_bind` fails.
    // ---------------------------------------------------------------------

    /// **The load-bearing test.** Multi-tenant plus a bind anyone can reach
    /// is refused, and the refusal says why rather than just what.
    #[test]
    fn multi_tenant_refuses_a_non_loopback_bind() {
        for host in ["0.0.0.0", "::", "192.168.1.10", "10.0.0.2"] {
            let err = match check_bind(true, host.parse().unwrap(), false) {
                Ok(()) => panic!("{host} must not serve an unauthenticated resolver"),
                Err(e) => e,
            };
            let msg = err.to_string();
            assert!(msg.contains(host), "the error names the address: {msg}");
            assert!(
                msg.contains("nothing authenticates"),
                "the error says WHY, not just what: {msg}"
            );
            assert!(
                msg.contains("PKDUMP_MULTITENANT_INSECURE_BIND"),
                "the error names the way out: {msg}"
            );
        }
    }

    /// Loopback is the mode's intended shape — a developer, a demo, an SSH
    /// tunnel — and stays allowed.
    #[test]
    fn multi_tenant_on_loopback_still_starts() {
        for host in ["127.0.0.1", "::1", "127.0.0.5"] {
            check_bind(true, host.parse().unwrap(), false)
                .unwrap_or_else(|e| panic!("{host} is loopback and must serve: {e}"));
        }
    }

    /// The escape hatch works — for whoever puts authentication in front of
    /// it later — and it is the *only* thing that opens the refused case.
    #[test]
    fn the_explicit_opt_in_allows_the_insecure_bind() {
        check_bind(true, "0.0.0.0".parse().unwrap(), true).unwrap();
    }

    /// **Single-tenant is untouched at any address.** It is the shipped
    /// default and `deploy/pkdump.container` binds `0.0.0.0`; a refusal that
    /// caught it would break production.
    #[test]
    fn single_tenant_is_unaffected_at_any_host() {
        for host in ["0.0.0.0", "::", "127.0.0.1", "192.168.1.10"] {
            check_bind(false, host.parse().unwrap(), false)
                .unwrap_or_else(|e| panic!("single-tenant on {host} must serve: {e}"));
        }
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
