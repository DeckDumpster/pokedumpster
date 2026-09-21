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

pub mod access;
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
    /// Cloudflare Access JWT validation state (JWKS cache + config). `None`
    /// when Access is not configured — the layer passes all requests through.
    pub(crate) access: Option<Arc<access::AccessState>>,
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
            DbError::Invalid(_) => StatusCode::BAD_REQUEST,
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
    //
    // Layer ordering: the outermost `route_layer` runs first. The access
    // layer (outermost) verifies the JWT and installs the identity; the
    // tenant layer (inner) maps that identity to a database.
    let api = routes::api_router()
        .route_layer(middleware::from_fn_with_state(state.clone(), tenant::layer))
        .route_layer(middleware::from_fn_with_state(state.clone(), access::layer));
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
    /// Cloudflare Access configuration. `None` when Access is not configured —
    /// the authentication layer is skipped and the server starts without it.
    /// When `Some`, all three fields are required and a JWKS fetch failure is a
    /// startup failure. See `access::AccessConfig::from_env_if_configured`.
    pub access: Option<access::AccessConfig>,
}

/// Refuse multi-tenant mode when Cloudflare Access is not configured.
///
/// Multi-tenant resolution now requires Cloudflare Access: every request must
/// carry a valid JWT and the verified email must be bound to a tenant in the
/// registry. Without an Access configuration the JWT layer accepts nothing, so
/// multi-tenant mode would refuse every request rather than serving any tenant.
///
/// Single-tenant mode is unaffected — the tenant is fixed at startup and no
/// JWT is required.
fn check_multitenant_access(multi_tenant: bool, access_configured: bool) -> anyhow::Result<()> {
    if !multi_tenant || access_configured {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to start: multi-tenant resolution is on but Cloudflare Access is not \
         configured.\n\
         \n\
         In multi-tenant mode every request must carry a valid Cloudflare Access JWT; the \
         verified email in the JWT determines which tenant's collection is served. Without \
         Access configured the JWT layer will reject every request.\n\
         \n\
         Set {} and {} (and optionally {}), or drop --multi-tenant / PKDUMP_MULTITENANT.",
        access::TEAM_DOMAIN_ENV,
        access::AUD_ENV,
        access::JWKS_URL_ENV,
    )
}

/// Start the HTTP server. In single-tenant mode the collection database is
/// opened up front — so a missing catalog (`pkdump setup` not run) fails at
/// startup rather than on the first request.
pub async fn serve(cfg: ServeConfig) -> anyhow::Result<()> {
    // Access config must be present when multi-tenant is on.
    check_multitenant_access(cfg.multi_tenant, cfg.access.is_some())?;
    // Idempotent shared-catalog convergence on startup. `pkdump setup` and
    // the nightly `pkdump-lake-derive shared` normally own shared schema —
    // the refresh used to be on that list and is not since pd-lunn, because
    // it opens the catalog read-only now — but a binary upgrade can ship a
    // data-only migration (e.g. seeding a new variant) that must be applied
    // before the server starts serving requests.
    //
    // `open_shared_for_serving`, not `open_shared`: it asks READ-ONLY whether
    // this catalog already carries this build's convergence fingerprint and,
    // on a match, never takes the write lock at all (pd-dzu5). It used to,
    // unconditionally and on every start — the search-metadata reconcile it
    // used to make here was a DELETE and a few hundred INSERTs each time —
    // which put a restart in a race with the nightly derive it can only lose.
    // That reconcile is now part of the convergence itself, so it happens on
    // exactly the starts that have something to converge.
    //
    // The search registry is read off the catalog here too: it is catalog
    // data, the same for every tenant, so it is loaded once from the one
    // shared database rather than through whichever collection happens to
    // be open.
    let (registry, flags) = {
        let shared = pkdump_db::open_shared_for_serving(&cfg.shared_db)?;
        (
            Arc::new(pkdump_db::search_meta::load_registry(&shared)?),
            Arc::new(pkdump_db::search_meta::load_flags(&shared)?),
        )
    };
    // Cloudflare Access JWT validation — opt-in by configuration. When the
    // three Access env vars are set, primes the JWKS cache; a failed fetch is a
    // startup failure. When not configured, `None` here makes the layer a
    // passthrough and the server starts without authentication.
    let access = match cfg.access {
        Some(cfg) => Some(access::AccessState::new(cfg).await?),
        None => {
            println!("pkdump: Cloudflare Access is NOT configured — /api is unauthenticated");
            None
        }
    };
    let tenants = if cfg.multi_tenant {
        println!(
            "pkdump: MULTI-TENANT resolution is ON — every request must carry a valid \
             Cloudflare Access JWT and the verified email must be bound to a tenant."
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
        access,
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
    use access::test_support::TestAccessFixture;
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

    fn router_for(
        dir: &std::path::Path,
        shared: &std::path::Path,
        tenants: Tenants,
        access: Option<Arc<access::AccessState>>,
    ) -> Router {
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
            access,
        };
        app(state, dir.join("static"), data_dir)
    }

    /// A single-tenant test router — the production shape.
    async fn test_app() -> (tempfile::TempDir, Router, TestAccessFixture) {
        let fx = TestAccessFixture::new().await;
        let dir = tempfile::tempdir().unwrap();
        let shared = seed(dir.path());
        let tenants = Tenants::single(
            "collection",
            dir.path().join("tenants").join("collection.sqlite"),
            shared.clone(),
        )
        .unwrap();
        let router = router_for(dir.path(), &shared, tenants, Some(fx.access.clone()));
        (dir, router, fx)
    }

    /// A multi-tenant test router with `handles` provisioned, each with an
    /// email binding `<handle>@example.com`.
    async fn multi_tenant_app(
        handles: &[&str],
    ) -> (tempfile::TempDir, Router, PathBuf, TestAccessFixture) {
        let fx = TestAccessFixture::new().await;
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
            pkdump_db::registry::identity_add(
                &registry,
                &user.database_id,
                &format!("{handle}@example.com"),
                None,
                None,
            )
            .unwrap();
        }
        let router = router_for(
            dir.path(),
            &shared,
            Tenants::multi(tenants_dir.clone(), shared.clone(), &registry_db).unwrap(),
            Some(fx.access.clone()),
        );
        (dir, router, tenants_dir, fx)
    }

    /// Build a request, optionally injecting a JWT and a body.
    fn request(method: &str, uri: &str, token: Option<&str>, body: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            b = b.header(access::JWT_HEADER, t);
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
        let (_d, router, _fx) = test_app().await;
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
    async fn api_request_without_token_is_401() {
        let (_d, router, _fx) = test_app().await;
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/collection")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "a request with no JWT must be rejected"
        );
    }

    #[tokio::test]
    async fn backup_status_no_marker_is_not_stale() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request("GET", "/api/backup-status", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("\"last_ok_epoch\":null"), "body: {body}");
        assert!(body.contains("\"stale\":false"), "body: {body}");
    }

    #[tokio::test]
    async fn backup_status_old_marker_is_stale() {
        let (dir, router, fx) = test_app().await;
        let old = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 100 * 3600;
        std::fs::write(dir.path().join(".backup-last-ok"), old.to_string()).unwrap();
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request("GET", "/api/backup-status", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("\"stale\":true"), "body: {body}");
    }

    #[tokio::test]
    async fn serves_spa_index_with_fallback() {
        let (_d, router, _fx) = test_app().await;

        let root = router
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::OK);
        assert!(body_string(root).await.contains("PokeDumpster"));

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

    async fn body_bytes(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    async fn get_encoding(
        router: &Router,
        uri: &str,
        accept: Option<&str>,
        token: Option<&str>,
    ) -> (String, Vec<u8>) {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = accept {
            b = b.header(header::ACCEPT_ENCODING, a);
        }
        if let Some(t) = token {
            b = b.header(access::JWT_HEADER, t);
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

    const BULKY_JSON: &str = "/api/search/keywords";

    #[tokio::test]
    async fn json_gzips_and_decodes_to_the_uncompressed_bytes() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");

        let (plain_encoding, plain) = get_encoding(&router, BULKY_JSON, None, Some(&tok)).await;
        assert_eq!(plain_encoding, "");
        serde_json::from_slice::<serde_json::Value>(&plain).unwrap();
        assert!(
            plain.len() > COMPRESS_MIN_BYTES as usize,
            "fixture too small: {} bytes",
            plain.len()
        );

        let (encoding, compressed) =
            get_encoding(&router, BULKY_JSON, Some("gzip"), Some(&tok)).await;
        assert_eq!(encoding, "gzip");
        assert!(
            compressed.len() < plain.len(),
            "gzip grew the body: {} -> {}",
            plain.len(),
            compressed.len()
        );
        assert_eq!(gunzip(&compressed), plain);
    }

    #[tokio::test]
    async fn brotli_is_used_when_the_client_offers_it() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let (_, plain) = get_encoding(&router, BULKY_JSON, None, Some(&tok)).await;

        let (encoding, compressed) =
            get_encoding(&router, BULKY_JSON, Some("gzip, deflate, br"), Some(&tok)).await;
        assert_eq!(encoding, "br");
        assert_eq!(unbrotli(&compressed), plain);
    }

    #[tokio::test]
    async fn responses_below_the_threshold_are_not_compressed() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let (encoding, body) =
            get_encoding(&router, "/api/backup-status", Some("gzip, br"), Some(&tok)).await;
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
        let (dir, router, _fx) = test_app().await;
        let symbols = dir.path().join("symbols");
        std::fs::create_dir_all(&symbols).unwrap();
        let png = vec![b'P'; 8 * 1024];
        std::fs::write(symbols.join("sv3pt5.png"), &png).unwrap();

        // Images are not under /api so no token is needed.
        let (encoding, body) =
            get_encoding(&router, "/sym/sv3pt5.png", Some("gzip, br"), None).await;
        assert_eq!(encoding, "");
        assert_eq!(body, png);
    }

    #[tokio::test]
    async fn collection_endpoints_round_trip() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");

        let created = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&tok),
                Some(r#"{"printing_id":"sv3pt5-1-normal","source":"manual_id"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let listed = router
            .clone()
            .oneshot(request("GET", "/api/collection", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        assert!(body_string(listed).await.contains("sv3pt5-1-normal"));

        let bad = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&tok),
                Some(r#"{"printing_id":"sv3pt5-1-nope","source":"manual_id"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::NOT_FOUND);

        let deleted = router
            .oneshot(request("DELETE", "/api/collection/1", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn manual_price_on_a_catalog_printing_is_a_400_naming_the_seed_file() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");

        let refused = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/manual-prices",
                Some(&tok),
                Some(r#"{"printing_id":"sv3pt5-1-normal","price":29.0}"#),
            ))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert!(
            body_string(refused)
                .await
                .contains("data/overrides/catalog_prices.json")
        );

        let unknown = router
            .oneshot(request(
                "POST",
                "/api/manual-prices",
                Some(&tok),
                Some(r#"{"printing_id":"nope-0-normal","price":1.0}"#),
            ))
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn card_endpoint_returns_detail_and_404() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");

        let found = router
            .clone()
            .oneshot(request("GET", "/api/card/sv3pt5/1", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(found.status(), StatusCode::OK);
        assert!(body_string(found).await.contains("Bulbasaur"));

        let missing = router
            .oneshot(request("GET", "/api/card/sv3pt5/999", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_owned_and_missing() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");

        router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&tok),
                Some(r#"{"printing_id":"sv3pt5-1-normal","source":"manual_id"}"#),
            ))
            .await
            .unwrap();

        let owned = router
            .clone()
            .oneshot(request("GET", "/api/collection/search", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(owned.status(), StatusCode::OK);
        let body = body_string(owned).await;
        assert!(body.contains("sv3pt5-1-normal"), "owned search: {body}");
        assert!(body.contains("\"owned\":true"), "owned flag: {body}");

        let none = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/collection/search?q=t:fire",
                Some(&tok),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(none.status(), StatusCode::OK);
        let body = body_string(none).await;
        assert!(body.contains("\"rows\":[]"), "no matches: {body}");
        assert!(body.contains("\"total\":0"), "no matches: {body}");
    }

    #[tokio::test]
    async fn search_returns_a_page_envelope_with_a_bounded_default() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request(
                "GET",
                "/api/collection/search?include_unowned=1",
                Some(&tok),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(body["total"], 1);
        assert_eq!(body["limit"], pkdump_db::search::DEFAULT_LIMIT);
        assert_eq!(body["offset"], 0);
    }

    #[tokio::test]
    async fn search_honours_limit_and_offset() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let page = |uri: String| {
            let router = router.clone();
            let tok = tok.clone();
            async move {
                let resp = router
                    .oneshot(request("GET", &uri, Some(&tok), None))
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            }
        };

        let empty = page("/api/collection/search?include_unowned=1&limit=0".to_string()).await;
        assert!(empty["rows"].as_array().unwrap().is_empty());
        assert_eq!(empty["total"], 1, "limit=0 is a count-only request");

        let past_end =
            page("/api/collection/search?include_unowned=1&limit=10&offset=5".to_string()).await;
        assert!(past_end["rows"].as_array().unwrap().is_empty());
        assert_eq!(past_end["total"], 1);
        assert_eq!(past_end["limit"], 10);
        assert_eq!(past_end["offset"], 5);
    }

    #[tokio::test]
    async fn search_prices_the_whole_result_and_not_the_page() {
        let (dir, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&tok),
                Some(ADD_CARD),
            ))
            .await
            .unwrap();
        {
            let c = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
            c.execute(
                "INSERT INTO catalog_price_overrides (printing_id, price, observed_at) \
                 VALUES ('sv3pt5-1-normal', 12.5, '2025-01-01')",
                [],
            )
            .unwrap();
        }

        let page = |uri: String| {
            let router = router.clone();
            let tok = tok.clone();
            async move {
                let resp = router
                    .oneshot(request("GET", &uri, Some(&tok), None))
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            }
        };

        let whole = page("/api/collection/search?limit=all".to_string()).await;
        assert_eq!(whole["total_value"], 12.5);

        let none = page("/api/collection/search?limit=0".to_string()).await;
        assert!(none["rows"].as_array().unwrap().is_empty());
        assert_eq!(none["total_value"], 12.5);
    }

    #[tokio::test]
    async fn search_serves_the_whole_result_for_limit_all() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request(
                "GET",
                "/api/collection/search?include_unowned=1&limit=all",
                Some(&tok),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let rows = body["rows"].as_array().unwrap().len();
        assert_eq!(body["total"], rows);
        assert_eq!(body["limit"], rows);
        assert_eq!(body["offset"], 0);
    }

    #[tokio::test]
    async fn search_refuses_an_offset_alongside_limit_all() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request(
                "GET",
                "/api/collection/search?limit=all&offset=10",
                Some(&tok),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(body["error"].as_str().unwrap().contains("limit=all"));
        assert!(body["position"].is_null());
    }

    #[tokio::test]
    async fn search_refuses_bad_paging_bounds() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
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
                .oneshot(request("GET", &uri, Some(&tok), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
            assert!(body["error"].is_string(), "{uri}: {body}");
            assert!(body["position"].is_null(), "not a query error: {uri}");
        }
    }

    #[tokio::test]
    async fn search_refuses_a_sort_it_cannot_satisfy() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        for key in ["value", "adj", "nonsense"] {
            let uri = format!("/api/collection/search?sort={key}");
            let resp = router
                .clone()
                .oneshot(request("GET", &uri, Some(&tok), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
            let message = body["error"].as_str().unwrap_or_default();
            for named in pkdump_db::search::SORT_KEYS {
                assert!(message.contains(named), "{uri}: {message} omits {named}");
            }
            assert!(body["position"].is_null(), "not a query error: {uri}");
        }
    }

    #[tokio::test]
    async fn search_serves_every_advertised_sort_key() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        for key in pkdump_db::search::SORT_KEYS {
            let uri = format!("/api/collection/search?sort={key}&include_unowned=1");
            let resp = router
                .clone()
                .oneshot(request("GET", &uri, Some(&tok), None))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn search_rejects_unknown_keyword_with_position() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request(
                "GET",
                "/api/collection/search?q=xyz:1",
                Some(&tok),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_string(resp).await;
        assert!(body.contains("position"), "expected position in {body}");
        assert!(body.contains("xyz"), "expected keyword in {body}");
    }

    #[tokio::test]
    async fn search_keywords_endpoint_serves_registry() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request("GET", "/api/search/keywords", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("energy_type"), "keywords: {body}");
        assert!(body.contains("holo"), "flags: {body}");
    }

    #[tokio::test]
    async fn export_json_serves_a_downloadable_envelope() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");
        let resp = router
            .oneshot(request("GET", "/api/export/json", Some(&tok), None))
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
        assert!(envelope.get("collection").is_some());
        assert!(envelope.get("printings").is_none());
    }

    // ---- tenant resolution and isolation ------------------------------------

    /// Each tenant's JWT email resolves to their own database only.
    #[tokio::test]
    async fn one_tenant_cannot_reach_another_tenants_collection() {
        let (_d, router, _dir, fx) = multi_tenant_app(&["alice", "bob"]).await;
        let alice_tok = fx.valid_token("alice@example.com");
        let bob_tok = fx.valid_token("bob@example.com");

        let created = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&alice_tok),
                Some(ADD_CARD),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let bobs = router
            .clone()
            .oneshot(request("GET", "/api/collection", Some(&bob_tok), None))
            .await
            .unwrap();
        assert_eq!(bobs.status(), StatusCode::OK);
        let body = body_string(bobs).await;
        assert_eq!(
            body.trim(),
            "[]",
            "bob's collection must not contain alice's card: {body}"
        );

        let stolen = router
            .clone()
            .oneshot(request("DELETE", "/api/collection/1", Some(&bob_tok), None))
            .await
            .unwrap();
        assert_eq!(
            stolen.status(),
            StatusCode::NOT_FOUND,
            "bob deleted a row out of alice's collection"
        );

        let alices = router
            .oneshot(request("GET", "/api/collection", Some(&alice_tok), None))
            .await
            .unwrap();
        assert!(body_string(alices).await.contains("sv3pt5-1-normal"));
    }

    /// An unbound email is a 403 and does not create a database.
    #[tokio::test]
    async fn an_unbound_email_is_a_403_and_creates_nothing() {
        let (_d, router, tenants_dir, fx) = multi_tenant_app(&["alice"]).await;
        let tok = fx.valid_token("mallory@example.com");
        let before = std::fs::read_dir(&tenants_dir).unwrap().count();
        let resp = router
            .oneshot(request("GET", "/api/collection", Some(&tok), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(std::fs::read_dir(&tenants_dir).unwrap().count(), before);
    }

    /// In single-tenant mode, any authenticated caller reaches the one
    /// collection — the JWT is validated but its email is not looked up.
    #[tokio::test]
    async fn with_the_flag_off_any_identity_reaches_the_single_tenant() {
        let (_d, router, fx) = test_app().await;
        let tok = fx.valid_token("u@example.com");

        let created = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&tok),
                Some(ADD_CARD),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        // A different email — still resolves to the single collection.
        let other_tok = fx.valid_token("someone-else@example.com");
        let claimed = router
            .oneshot(request("GET", "/api/collection", Some(&other_tok), None))
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK);
        assert!(body_string(claimed).await.contains("sv3pt5-1-normal"));
    }

    /// A leftover `x-pkdump-tenant` header cannot be used to reach another
    /// tenant's collection. The header is not read; resolution is from the JWT
    /// email only. Sending bob's handle alongside alice's JWT still gives alice.
    #[tokio::test]
    async fn leftover_tenant_header_cannot_escalate_to_another_tenant() {
        let (_d, router, _dir, fx) = multi_tenant_app(&["alice", "bob"]).await;
        let alice_tok = fx.valid_token("alice@example.com");

        // Alice adds a card.
        let created = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/collection",
                Some(&alice_tok),
                Some(ADD_CARD),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        // Now alice sends a request with her own JWT but a header claiming to
        // be bob. Resolution ignores the header; she still reaches alice's DB.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/collection")
                    .header(access::JWT_HEADER, &alice_tok)
                    .header("x-pkdump-tenant", "bob")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("sv3pt5-1-normal"),
            "alice's header claim resolved to the wrong collection: {body}"
        );
    }

    // ---- check_multitenant_access (sync, no HTTP) ----------------------------------------

    #[test]
    fn multi_tenant_without_access_refuses() {
        let err = check_multitenant_access(true, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(access::TEAM_DOMAIN_ENV), "{msg}");
        assert!(msg.contains(access::AUD_ENV), "{msg}");
    }

    #[test]
    fn multi_tenant_with_access_configured_starts() {
        check_multitenant_access(true, true).unwrap();
    }

    #[test]
    fn single_tenant_is_unaffected_by_access_config() {
        check_multitenant_access(false, false).unwrap();
        check_multitenant_access(false, true).unwrap();
    }

    /// Single-tenant mode without Access configured is the production shape.
    /// The access layer must not block /api/ requests when `state.access` is
    /// `None` — it installs a synthetic placeholder identity so `tenant::layer`
    /// can call `access::current()` without error; `Tenants::resolve` ignores
    /// it in single-tenant mode.
    #[tokio::test]
    async fn single_tenant_without_access_serves_api() {
        let dir = tempfile::tempdir().unwrap();
        let shared = seed(dir.path());
        let user_db = dir.path().join("tenants").join("collection.sqlite");
        pkdump_db::open_user(&user_db).unwrap();
        let tenants = Tenants::single("collection", user_db, shared.clone()).unwrap();
        // access: None — the production single-tenant shape.
        let router = router_for(dir.path(), &shared, tenants, None);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/collection")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "single-tenant mode with no Access config must serve /api/ routes"
        );
    }

    #[tokio::test]
    async fn there_is_no_database_outside_a_resolved_request() {
        let fx = TestAccessFixture::new().await;
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
            access: Some(fx.access.clone()),
        };

        let unscoped = blocking(&state, |c| {
            Ok(c.query_row("SELECT count(*) FROM collection", [], |r| {
                r.get::<_, i64>(0)
            })?)
        })
        .await;
        assert!(unscoped.is_err(), "a connection without a resolved tenant");

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
