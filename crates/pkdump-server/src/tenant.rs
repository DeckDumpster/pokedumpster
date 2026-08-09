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
//! With the flag off — production, and every existing UI test — this module
//! is inert: [`Tenants::resolve`] ignores the request entirely and hands back
//! the single tenant the process was started with. A request cannot switch
//! tenants by sending a header, because in single-tenant mode the header is
//! never read.
//!
//! # The header is a lookup key, not a filename
//!
//! What a request names is a **handle**. What it is served from is
//! `tenants/<database_id>.sqlite`, and the two are joined by a row in the
//! user registry ([`pkdump_db::registry`]) — never by string equality.
//! Resolution is therefore a `SELECT` with the header as a bound parameter,
//! and the only string that reaches a path constructor is the `database_id`
//! the registry hands back, which only the registry mints. An unknown handle
//! is not in the table; a handle full of `../` is not in the table either.
//! There is nothing for it to escape, because nothing concatenates it.
//!
//! That is the whole of `pd-rqgv`. Before it, the validated header value
//! *was* the filename, and the only thing standing between an unauthenticated
//! caller and a path was a charset regex.
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

use pkdump_db::registry::{self, UserState};
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
/// The inner value is private and there is no public constructor: the only
/// way to obtain one is [`Tenants::resolve`], which is reachable only from
/// the middleware. So a route cannot name a tenant of its own choosing —
/// including its own, including the default one.
///
/// What it holds is *storage*, not identity: in multi-tenant mode the
/// `database_id` the registry issued, and in single-tenant mode the one
/// tenant name the process was started with (whose database it was given
/// outright). It is never the handle a request sent — that string stops at
/// the registry lookup.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// The identifier of the database this request is served from.
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
    /// Resolution on. Every request names a handle; the registry says which
    /// database that handle is served from, and that database must already
    /// exist under `dir`.
    ///
    /// The registry connection is opened once and held: it is consulted on
    /// every request, and a per-request open would put the data root's
    /// directory entries in the hot path for no gain. One mutex, because a
    /// `rusqlite::Connection` is not `Sync` — the lookup is a single indexed
    /// `SELECT` on a table with one row per user.
    Multi {
        dir: PathBuf,
        registry: Mutex<Connection>,
    },
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

    /// Multi-tenant: serve whichever user each request names, resolving the
    /// handle through the registry at `registry_db` to a database under
    /// `tenants_dir`. Collection connections open lazily, on first request
    /// per tenant.
    ///
    /// No *collection* is created here or by [`Tenants::resolve`]: a user
    /// exists because `pkdump tenant create` registered them and made their
    /// database, and a request naming anyone else gets a 404. The registry
    /// itself is opened — and its schema applied — up front, so a data root
    /// that has never had a user registered comes up and answers 404 rather
    /// than failing the first request with a missing file.
    pub fn multi(
        tenants_dir: PathBuf,
        shared_db: PathBuf,
        registry_db: &Path,
    ) -> Result<Self, DbError> {
        Ok(Tenants {
            mode: Mode::Multi {
                dir: tenants_dir,
                registry: Mutex::new(pkdump_db::open_registry(registry_db)?),
            },
            shared_db,
            open: Mutex::new(HashMap::new()),
        })
    }

    /// The tenant this request is served as.
    ///
    /// In single-tenant mode the headers are not read at all — the header is
    /// not a way to switch tenants on an instance that did not opt in.
    ///
    /// In multi-tenant mode the header is a **lookup key**: it is compared
    /// against `user.handle` as a bound parameter and is then done with. A
    /// handle that is not registered, and one whose registration was
    /// detached, are the same 404 — neither is an active user, and neither
    /// becomes a path.
    pub(crate) fn resolve(&self, headers: &HeaderMap) -> Result<TenantId, AppError> {
        let (dir, registry) = match &self.mode {
            Mode::Single { id, .. } => return Ok(id.clone()),
            Mode::Multi { dir, registry } => (dir, registry),
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
        let handle = raw.to_str().map_err(|_| {
            AppError(
                StatusCode::BAD_REQUEST,
                format!("`{TENANT_HEADER}` is not valid ASCII"),
            )
        })?;
        // The handle is not validated here, and that is the point: there is
        // no longer anything to protect it from. It is matched against a
        // column; a value the registry would never have issued a row for
        // simply misses. Reintroducing a charset check would suggest the
        // safety came from the charset, when it comes from the lookup.
        let found = registry
            .lock()
            .map_err(|_| AppError::internal("registry mutex poisoned"))
            .and_then(|conn| {
                registry::lookup(&conn, handle)
                    .map_err(|e| AppError::internal(format!("registry lookup failed: {e}")))
            })?;
        // Unregistered and detached are one answer: not an active user.
        // Distinguishing them would tell an unauthenticated caller which
        // handles have ever existed, and the handle is echoed back to
        // nobody — it is untrusted bytes and does not belong in a response.
        let database_id = match found {
            Some(user) if user.state == UserState::Active => user.database_id,
            _ => {
                return Err(AppError(
                    StatusCode::NOT_FOUND,
                    "no such tenant on this instance".to_string(),
                ));
            }
        };
        // Registered, so their database must be on disk. It is not created
        // here: a resolver that opened whatever it was handed would let a
        // caller provision by guessing, and a registry that disagrees with
        // the disk is a fault to surface, not to paper over with an empty
        // collection.
        let db = pkdump_db::tenant_db_file(dir, &database_id)?;
        if !db.exists() {
            return Err(AppError::internal(format!(
                "registry names database {database_id} for this tenant, but {} \
                 does not exist",
                db.display()
            )));
        }
        Ok(TenantId(database_id))
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
            // [`pkdump_db::tenant_db_file`] and not an interpolation here:
            // it refuses anything that is not a ULID the registry minted, so
            // the last step before a path exists is a check that the string
            // came from us. Nothing off the wire can satisfy it.
            Mode::Multi { dir, .. } => pkdump_db::tenant_db_file(dir, id.as_str()),
        }
    }
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

    /// A multi-tenant instance with `handles` registered, each with the
    /// collection database its `database_id` names — what
    /// `pkdump tenant create` leaves behind. Returns the instance, the
    /// `tenants/` directory, and the registry connection.
    fn provisioned(dir: &Path, handles: &[&str]) -> (Tenants, PathBuf, Connection) {
        let shared = catalog(dir);
        let tenants_dir = dir.join("tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let registry_db = dir.join("registry.sqlite");
        let reg = pkdump_db::open_registry(&registry_db).unwrap();
        for handle in handles {
            let user = registry::insert(&reg, handle).unwrap();
            pkdump_db::open_user(
                &pkdump_db::tenant_db_file(&tenants_dir, &user.database_id).unwrap(),
            )
            .unwrap();
        }
        let tenants = Tenants::multi(tenants_dir.clone(), shared, &registry_db).unwrap();
        (tenants, tenants_dir, reg)
    }

    fn status(result: Result<TenantId, AppError>) -> StatusCode {
        result.expect_err("resolved when it should not have").0
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
        let (tenants, _, _reg) = provisioned(dir.path(), &[]);
        let AppError(status, body) = tenants.resolve(&HeaderMap::new()).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(TENANT_HEADER), "unhelpful error: {body}");
    }

    /// Naming a handle nobody registered is a 404 — and, crucially, does
    /// not bring a database into existence. Otherwise any caller could
    /// provision tenants by guessing names.
    #[test]
    fn an_unknown_handle_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, _reg) = provisioned(dir.path(), &[]);

        assert_eq!(
            status(tenants.resolve(&headers("mallory"))),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            std::fs::read_dir(&tenants_dir).unwrap().count(),
            0,
            "resolution created something"
        );
    }

    /// **The load-bearing test (`pd-rqgv`).** A handle is a lookup key and
    /// nothing else: a database that the registry does not name cannot be
    /// reached by naming its *filename* in the header.
    ///
    /// `ghost.sqlite` is a real, openable collection sitting in `tenants/`.
    /// Under the old resolver — `dir.join(format!("{name}.sqlite"))` behind
    /// a charset check — the header `ghost` passed validation, the file
    /// existed, and the request was served from it. Put that interpolation
    /// back and this test fails on the first assertion.
    #[test]
    fn a_database_the_registry_does_not_name_cannot_be_reached() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &["alice"]);
        // A perfectly valid handle, a perfectly real file, no registry row.
        pkdump_db::open_user(&tenants_dir.join("ghost.sqlite")).unwrap();
        assert_eq!(
            status(tenants.resolve(&headers("ghost"))),
            StatusCode::NOT_FOUND
        );

        // Nor by naming the file alice IS served from: a database_id is not
        // a handle. Only the handle column is a way in.
        let alice = registry::lookup(&reg, "alice").unwrap().unwrap();
        assert_eq!(
            status(tenants.resolve(&headers(&alice.database_id))),
            StatusCode::NOT_FOUND
        );

        // And the registered handle resolves to the id, not to its own name.
        assert_eq!(
            tenants.resolve(&headers("alice")).unwrap().as_str(),
            alice.database_id
        );
    }

    /// The negative the epic exists for: a handle carrying traversal or a
    /// separator is rejected at the lookup, and no path is built from it.
    ///
    /// It is not rejected for its *characters* — nothing validates the
    /// header any more. It misses the table, like any other string that was
    /// never registered. The `collection.sqlite` beside the catalog is the
    /// prize `../collection` was reaching for; it is still there afterwards,
    /// unopened.
    #[test]
    fn a_traversing_handle_never_reaches_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, _reg) = provisioned(dir.path(), &["alice"]);
        // A database one level up, which a traversing handle would reach.
        let outside = dir.path().join("collection.sqlite");
        pkdump_db::open_user(&outside).unwrap();
        let before = std::fs::read_dir(&tenants_dir).unwrap().count();

        for hostile in [
            "../collection",
            "../../etc/passwd",
            "tenants/../collection",
            "alice/../alice",
            "..",
            ".",
            "/etc/shadow",
            "Alice",
            "alice ",
            "has space",
            "",
        ] {
            let mut h = HeaderMap::new();
            h.insert(
                TENANT_HEADER,
                HeaderValue::from_bytes(hostile.as_bytes()).unwrap(),
            );
            assert_eq!(
                status(tenants.resolve(&h)),
                StatusCode::NOT_FOUND,
                "{hostile:?}"
            );
        }

        // Nothing was created, and nothing outside `tenants/` was touched
        // into existence either.
        assert_eq!(std::fs::read_dir(&tenants_dir).unwrap().count(), before);
        assert!(!dir.path().join("collection.sqlite-wal").exists());
    }

    /// A detached user is not an active user. Their handle is free for
    /// someone else, and their database — which is still on disk, that
    /// being the point of detaching rather than deleting — is not served to
    /// whoever asks for the old name.
    #[test]
    fn a_detached_handle_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &["alice"]);
        let alice = registry::lookup(&reg, "alice").unwrap().unwrap();
        let detached = registry::detach(&reg, "alice").unwrap();

        // Her database is still there...
        assert!(
            pkdump_db::tenant_db_file(&tenants_dir, &alice.database_id)
                .unwrap()
                .exists()
        );
        // ...and her handle does not reach it. The row that keeps those bytes
        // attributable still spells her name — it is `state`, not a rewritten
        // handle, that takes her out of circulation, so the resolver has to
        // be asking the right question and not merely failing to match.
        assert_eq!(detached.handle, "alice");
        assert_eq!(
            status(tenants.resolve(&headers("alice"))),
            StatusCode::NOT_FOUND
        );
    }

    /// `pd-pm7b`, at the resolver: re-registering a released handle serves
    /// the new user their own empty database, never their predecessor's.
    #[test]
    fn a_recycled_handle_does_not_inherit_its_predecessors_database() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &["alice"]);
        let first = registry::lookup(&reg, "alice").unwrap().unwrap();
        registry::detach(&reg, "alice").unwrap();

        let second = registry::insert(&reg, "alice").unwrap();
        assert_ne!(second.database_id, first.database_id);
        pkdump_db::open_user(
            &pkdump_db::tenant_db_file(&tenants_dir, &second.database_id).unwrap(),
        )
        .unwrap();

        let resolved = tenants.resolve(&headers("alice")).unwrap();
        assert_eq!(resolved.as_str(), second.database_id);
        assert_ne!(resolved.as_str(), first.database_id);
    }

    /// Registered, but their database is missing: a fault, surfaced. Not a
    /// 404 that reads as "no such user", and above all not a fresh empty
    /// collection created on the spot.
    #[test]
    fn a_registered_user_with_no_database_is_an_error_not_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &[]);
        let alice = registry::insert(&reg, "alice").unwrap();

        assert_eq!(
            status(tenants.resolve(&headers("alice"))),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(
            !pkdump_db::tenant_db_file(&tenants_dir, &alice.database_id)
                .unwrap()
                .exists()
        );
    }

    /// Two tenants get two connections, each seeing only its own file.
    #[test]
    fn each_tenant_gets_its_own_database() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, _tenants_dir, _reg) = provisioned(dir.path(), &["alice", "bob"]);

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
