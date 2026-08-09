//! Tenant resolution — mapping one HTTP request to exactly one tenant's
//! collection database.
//!
//! # This is resolution, not authentication
//!
//! There is no login, no session, no token. A request in multi-tenant mode
//! says which tenant it is and is believed. That is the whole mechanism, and
//! it is why the mode is **off by default** behind an explicit opt-in
//! (`pkdump serve --multi-tenant` / `PKDUMP_MULTITENANT=1`) and why nothing
//! running it may face the internet. Identity is a separate epic.
//!
//! "May not" is enforced, not asked for: with the flag on and a bind address
//! that is not loopback, the process refuses to start
//! (`check_bind` in the crate root) unless a second, explicit opt-in says to expose it
//! anyway.
//!
//! With the flag off — production, and every existing UI test — this module
//! is inert: [`Tenants::resolve`] ignores the request entirely and hands back
//! the single tenant the process was started with. A request cannot switch
//! tenants by sending a header, because in single-tenant mode the header is
//! never read.
//!
//! # How isolation is enforced
//!
//! Structurally, not by filtering. A tenant's connection is opened against
//! that tenant's own database file, so there is no row belonging to another
//! tenant anywhere in its scope — no `WHERE tenant_id = ?` to forget. Three
//! things keep that true:
//!
//! 1. [`AppState`](crate::AppState) holds no connection of its own. The only
//!    way to reach a database is [`Tenants::connection`], and the only way to
//!    name one is a [`TenantId`], which only [`Tenants::resolve`] mints.
//! 2. The resolved id lives in a task-local for the duration of the request
//!    ([`scope`]), and [`crate::blocking`] reads it there. A handler that
//!    forgets to thread the tenant through cannot exist, because there is
//!    nothing to thread.
//! 3. [`assert_isolated`] fails the open unless the connection is wired to
//!    exactly this tenant's file plus the one read-only shared catalog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rusqlite::Connection;

use pkdump_db::{DbError, validate_tenant_name};

use crate::{AppError, AppState};

/// The request header naming the tenant, when resolution is on.
///
/// A header rather than a hostname or a path prefix, deliberately: a browser
/// does not send it on its own, so an unauthenticated multi-tenant instance
/// cannot be driven by simply pointing a browser at it. Making the dangerous
/// mode awkward to reach by accident is the point.
pub const TENANT_HEADER: &str = "x-pkdump-tenant";

/// A tenant that has been resolved for the request in hand.
///
/// The inner name is private and there is no public constructor: the only
/// way to obtain one is [`Tenants::resolve`], which is reachable only from
/// the middleware. So a route cannot name a tenant of its own choosing —
/// including its own, including the default one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// The tenant's name, as it appears in `tenants/<name>.sqlite`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which tenant a request is served as.
enum Mode {
    /// Resolution off. One tenant for the life of the process — the one
    /// `$PKDUMP_USER` named — and the request is not consulted.
    Single { id: TenantId, db: PathBuf },
    /// Resolution on. Every request names its tenant, and that tenant's
    /// database must already exist under `dir`.
    Multi { dir: PathBuf },
}

/// The set of collection databases this process may serve, and the open
/// connections to them.
///
/// One connection per tenant, opened on first use and kept for the life of
/// the process, each behind its own mutex — so two tenants' requests do not
/// serialise against each other the way two requests for the same tenant do.
pub struct Tenants {
    mode: Mode,
    shared_db: PathBuf,
    open: Mutex<HashMap<TenantId, Arc<Mutex<Connection>>>>,
}

impl Tenants {
    /// Single-tenant: serve exactly `tenant`, from `user_db`, whatever any
    /// request says. This is the default and the only thing production runs.
    ///
    /// The connection is opened here rather than lazily so that a missing
    /// catalog (`pkdump setup` not run) is a startup failure, as it was
    /// before tenants existed, and not a 500 on the first request.
    pub fn single(tenant: &str, user_db: PathBuf, shared_db: PathBuf) -> Result<Self, DbError> {
        validate_tenant_name(tenant)?;
        let id = TenantId(tenant.to_string());
        let tenants = Tenants {
            mode: Mode::Single {
                id: id.clone(),
                db: user_db,
            },
            shared_db,
            open: Mutex::new(HashMap::new()),
        };
        tenants.connection(&id)?;
        Ok(tenants)
    }

    /// Multi-tenant: serve whichever tenant each request names, from
    /// `tenants_dir`. Connections open lazily, on first request per tenant.
    ///
    /// Nothing is created here. A tenant exists because
    /// `pkdump tenant create` made its database; a request naming a tenant
    /// that has none gets a 404 (see [`Tenants::resolve`]).
    pub fn multi(tenants_dir: PathBuf, shared_db: PathBuf) -> Self {
        Tenants {
            mode: Mode::Multi { dir: tenants_dir },
            shared_db,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// The tenant this request is served as.
    ///
    /// In single-tenant mode the headers are not read at all — the header is
    /// not a way to switch tenants on an instance that did not opt in.
    pub(crate) fn resolve(&self, headers: &HeaderMap) -> Result<TenantId, AppError> {
        let dir = match &self.mode {
            Mode::Single { id, .. } => return Ok(id.clone()),
            Mode::Multi { dir } => dir,
        };
        let raw = headers.get(TENANT_HEADER).ok_or_else(|| {
            AppError(
                StatusCode::BAD_REQUEST,
                format!(
                    "multi-tenant resolution is on: every request must name its tenant \
                     in the `{TENANT_HEADER}` header"
                ),
            )
        })?;
        let name = raw.to_str().map_err(|_| {
            AppError(
                StatusCode::BAD_REQUEST,
                format!("`{TENANT_HEADER}` is not valid ASCII"),
            )
        })?;
        // The same validation provisioning uses, so a name that could never
        // have been created is rejected before it reaches the filesystem.
        validate_tenant_name(name)
            .map_err(|e| AppError(StatusCode::BAD_REQUEST, format!("{TENANT_HEADER}: {e}")))?;
        if !tenant_db(dir, name).exists() {
            return Err(AppError(
                StatusCode::NOT_FOUND,
                format!("no tenant {name:?} on this instance"),
            ));
        }
        Ok(TenantId(name.to_string()))
    }

    /// The open connection for `id`, opening it if this is its first request.
    pub(crate) fn connection(&self, id: &TenantId) -> Result<Arc<Mutex<Connection>>, DbError> {
        let mut open = self.open.lock().expect("tenant registry mutex poisoned");
        if let Some(conn) = open.get(id) {
            return Ok(conn.clone());
        }
        let path = self.db_path(id)?;
        let conn = pkdump_db::connect_user(&path, &self.shared_db)?;
        assert_isolated(&conn, &path, &self.shared_db)?;
        let handle = Arc::new(Mutex::new(conn));
        open.insert(id.clone(), handle.clone());
        Ok(handle)
    }

    /// The database file `id` is served from.
    ///
    /// In single-tenant mode any id but the configured one is an error
    /// rather than a path: resolution can only mint the one id, so a second
    /// one reaching here means the wiring is wrong, and a wrong path here is
    /// the one mistake that would serve a tenant someone else's collection.
    fn db_path(&self, id: &TenantId) -> Result<PathBuf, DbError> {
        match &self.mode {
            Mode::Single { id: only, db } => {
                if id != only {
                    return Err(DbError::Env(format!(
                        "tenant {id:?} requested from a single-tenant process serving {only:?}"
                    )));
                }
                Ok(db.clone())
            }
            Mode::Multi { dir } => Ok(tenant_db(dir, id.as_str())),
        }
    }
}

/// `tenants/` + a validated tenant name → that tenant's database file.
fn tenant_db(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.sqlite"))
}

/// Refuse a connection wired to anything but this tenant's own database and
/// the one shared catalog.
///
/// Isolation is a property of what a connection can see, and what it can see
/// is `pragma_database_list`. `main` must be this tenant's file and `shared`
/// must be the catalog; a third entry would be another database in query
/// scope, which is precisely the thing that must not exist. (`temp` is
/// SQLite's own scratch database, holding the catalog TEMP VIEWs, and is
/// skipped.)
fn assert_isolated(conn: &Connection, user_db: &Path, shared_db: &Path) -> Result<(), DbError> {
    let mut stmt = conn.prepare(
        "SELECT name, file FROM pragma_database_list WHERE name <> 'temp' ORDER BY name",
    )?;
    let attached: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let expected = [("main", user_db), ("shared", shared_db)];
    let wrong = attached.len() != expected.len()
        || attached
            .iter()
            .zip(expected)
            .any(|((name, file), (want, path))| name != want || !same_file(file, path));
    if wrong {
        let seen: Vec<String> = attached
            .iter()
            .map(|(n, f)| format!("{n}={f}"))
            .collect::<Vec<_>>();
        return Err(DbError::Env(format!(
            "refusing a tenant connection that is not isolated: expected main={} \
             and shared={}, got [{}]",
            user_db.display(),
            shared_db.display(),
            seen.join(", ")
        )));
    }
    Ok(())
}

/// Whether the path SQLite resolved an attachment to is the file we meant.
/// Both sides are canonicalised — the deployment mounts the data directory
/// through a symlink on macOS, where a textual comparison would not hold.
fn same_file(sqlite_path: &str, expected: &Path) -> bool {
    match (
        std::fs::canonicalize(sqlite_path),
        std::fs::canonicalize(expected),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

tokio::task_local! {
    /// The tenant resolved for the request being served on this task.
    static CURRENT: TenantId;
}

/// The tenant this request resolved to.
///
/// Absent means the request never went through [`layer`], which for anything
/// touching a database is a wiring bug, not a case to paper over with a
/// default — a default here would silently serve one tenant's collection to
/// another.
pub(crate) fn current() -> Result<TenantId, AppError> {
    CURRENT.try_with(TenantId::clone).map_err(|_| {
        AppError::internal(
            "no tenant resolved for this request — the route is mounted outside the \
             tenant-resolution layer",
        )
    })
}

/// Middleware: resolve the tenant, then run the rest of the request with it
/// in scope. Applied to `/api` as a `route_layer`, so it runs for matched API
/// routes and not for the SPA fallback.
pub(crate) async fn layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let id = state.tenants.resolve(request.headers())?;
    Ok(CURRENT.scope(id, next.run(request)).await)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Run `f` as if the request had resolved to `tenant`. For tests that
    /// exercise [`crate::blocking`] without going through the router.
    pub(crate) async fn as_tenant<F: std::future::Future>(tenant: &str, f: F) -> F::Output {
        CURRENT.scope(TenantId(tenant.to_string()), f).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(tenant: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(TENANT_HEADER, HeaderValue::from_str(tenant).unwrap());
        h
    }

    fn catalog(dir: &Path) -> PathBuf {
        let path = dir.join("shared.sqlite");
        pkdump_db::open_shared(&path).unwrap();
        path
    }

    /// Flag off: the header is not read, so it cannot move a request off the
    /// tenant the process was started with.
    #[test]
    fn single_tenant_mode_never_reads_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let shared = catalog(dir.path());
        let tenants =
            Tenants::single("collection", dir.path().join("collection.sqlite"), shared).unwrap();

        assert_eq!(
            tenants.resolve(&headers("alice")).unwrap().as_str(),
            "collection"
        );
        assert_eq!(
            tenants.resolve(&HeaderMap::new()).unwrap().as_str(),
            "collection"
        );
    }

    /// Flag on and no tenant named: an error, never the first or default
    /// tenant. There is no ambient tenant to fall back to.
    #[test]
    fn multi_tenant_mode_requires_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let shared = catalog(dir.path());
        let tenants = Tenants::multi(dir.path().join("tenants"), shared);
        let AppError(status, body) = tenants.resolve(&HeaderMap::new()).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(TENANT_HEADER), "unhelpful error: {body}");
    }

    /// Naming a tenant that has no database is a 404 — and, crucially, does
    /// not bring one into existence. Otherwise any caller could provision
    /// tenants by guessing names.
    #[test]
    fn an_unknown_tenant_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let shared = catalog(dir.path());
        let tenants_dir = dir.path().join("tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenants = Tenants::multi(tenants_dir.clone(), shared);

        let AppError(status, _) = tenants.resolve(&headers("mallory")).unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!tenants_dir.join("mallory.sqlite").exists());
    }

    /// A tenant name is a filename. Resolution runs it through the same
    /// validation provisioning does, before it touches the filesystem.
    #[test]
    fn a_traversing_tenant_name_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let shared = catalog(dir.path());
        // A database one level up, which a traversing name would reach.
        pkdump_db::open_user(&dir.path().join("collection.sqlite")).unwrap();
        let tenants = Tenants::multi(dir.path().join("tenants"), shared);

        for bad in ["../collection", "Alice", "a/b", "..", "has space", ""] {
            let mut h = HeaderMap::new();
            h.insert(
                TENANT_HEADER,
                HeaderValue::from_bytes(bad.as_bytes()).unwrap(),
            );
            let AppError(status, _) = tenants.resolve(&h).expect_err("{bad:?} resolved");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    /// Two tenants get two connections, each seeing only its own file.
    #[test]
    fn each_tenant_gets_its_own_database() {
        let dir = tempfile::tempdir().unwrap();
        let shared = catalog(dir.path());
        let tenants_dir = dir.path().join("tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        pkdump_db::open_user(&tenants_dir.join("alice.sqlite")).unwrap();
        pkdump_db::open_user(&tenants_dir.join("bob.sqlite")).unwrap();
        let tenants = Tenants::multi(tenants_dir.clone(), shared);

        let alice = tenants.resolve(&headers("alice")).unwrap();
        let bob = tenants.resolve(&headers("bob")).unwrap();
        assert_ne!(alice, bob);

        let a = tenants.connection(&alice).unwrap();
        let b = tenants.connection(&bob).unwrap();
        a.lock()
            .unwrap()
            .execute(
                "INSERT INTO binders (name, created_at, updated_at) \
                 VALUES ('alice', '2026-08-07', '2026-08-07')",
                [],
            )
            .unwrap();
        let seen: i64 = b
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seen, 0, "bob's connection saw alice's binder");

        // The same tenant twice is the same connection, not a second one.
        assert!(Arc::ptr_eq(&a, &tenants.connection(&alice).unwrap()));
    }

    /// A single-tenant process asked for some other tenant must error, not
    /// hand back the one collection it holds under another name.
    #[test]
    fn single_tenant_mode_refuses_a_foreign_id() {
        let dir = tempfile::tempdir().unwrap();
        let shared = catalog(dir.path());
        let tenants =
            Tenants::single("collection", dir.path().join("collection.sqlite"), shared).unwrap();
        assert!(tenants.connection(&TenantId("alice".into())).is_err());
    }
}
