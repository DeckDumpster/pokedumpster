//! Shared scaffolding for this crate's unit tests. Compiled out of any real
//! build.
//!
//! The same shape as `pkdump-keys`' own, and for the same reason: the master
//! key's location is an environment variable, so every test that needs a key
//! is mutating process-wide state and they cannot be allowed to overlap.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Points `PKDUMP_MASTER_KEY_FILE` at `path` for the life of the guard, then
/// puts the previous value back.
pub struct EnvGuard {
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Take the lock and set the variable.
    pub fn set(path: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(pkdump_keys::master::KEY_ENV_FILE).ok();
        unsafe { std::env::set_var(pkdump_keys::master::KEY_ENV_FILE, path) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(pkdump_keys::master::KEY_ENV_FILE, v) },
            None => unsafe { std::env::remove_var(pkdump_keys::master::KEY_ENV_FILE) },
        }
    }
}

/// A registry holding the real schema, with `ids` registered as live tenants.
pub fn registry(ids: &[&str]) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../../pkdump-db/src/schema_registry.sql"))
        .unwrap();
    for id in ids {
        pkdump_keys::state::register(&conn, id).unwrap();
    }
    conn
}
