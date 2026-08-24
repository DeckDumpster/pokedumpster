-- Registry database schema — the logical→physical map (pd-fci1).
--
-- One row per user: the handle the outside world names them by, and the
-- opaque database id that names their file on disk. Two separate facts,
-- joined by this table rather than by string equality.
--
-- Its own file at the data root: NOT a table in shared.sqlite (which every
-- tenant connection ATTACHes readable, so a roster there would be visible to
-- every tenant), and NOT under tenants/ (which would break "every *.sqlite
-- under tenants/ is a tenant", the statement the Litestream glob rests on).
--
-- Re-applied idempotently on every open, like the other two schemas.
--
-- Three rules live HERE rather than in the accessor, because the data model
-- for the application belongs in the schema:
--
--   1. `database_id` is the PRIMARY KEY. It is the stable identity; a handle
--      is a mutable label on it. That is this epic's whole thesis, and the
--      table says so rather than a comment in Rust.
--   2. The handle format is a CHECK, so every write is held to it no matter
--      which code path inserts — accessor, migration, or an operator with
--      sqlite3 open on the file.
--   3. Handle reuse is a PARTIAL UNIQUE INDEX. A handle is unique among
--      ACTIVE users only, so `detach` is an ordinary state change and the
--      retired row keeps the person's real handle.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS user (
    -- Opaque ULID, and the filename stem of tenants/<database_id>.sqlite.
    -- The PRIMARY KEY: what a person's collection, their replica prefix and
    -- their history are all keyed on, and the one thing that never changes.
    database_id TEXT PRIMARY KEY,
    -- What a request names; renameable without touching anything on disk.
    -- The same charset crate::paths::validate_tenant_name admits, spelled as
    -- a constraint so the database is the last word on it. The two cannot
    -- share an implementation — one is a Rust function, the other is checked
    -- for writers that never enter the crate — so they share a CORPUS instead:
    -- crate::paths::HANDLE_CASES is run through the validator by
    -- paths::tests::tenant_names_are_validated and through this CHECK by
    -- registry::tests::the_check_and_the_validator_agree. Relax one side only
    -- and one of those two fails. It matters because the validator is also
    -- what refuses a malformed tenant header with a 400 (pd-4g7c): a handle
    -- one side admits and the other does not is a request answered wrongly.
    --
    --   * 1..32 characters — long enough to name a person, short enough to
    --     stay a filename-safe label rather than a payload.
    --   * starts with a lowercase letter or digit — a leading `-` reads as a
    --     flag to every CLI it is passed to.
    --   * nothing outside a-z 0-9 - _ — no `.`, `/` or `\`, and no uppercase
    --     (on a case-insensitive filesystem `Alice` and `alice` are one file
    --     but two S3 prefixes).
    handle      TEXT NOT NULL CHECK (
        length(handle) BETWEEN 1 AND 32
        AND handle GLOB '[a-z0-9]*'            -- starts alphanumeric
        AND handle NOT GLOB '*[^a-z0-9_-]*'    -- and contains nothing else
    ),
    created_at  TEXT NOT NULL,
    -- active   — a live user; resolvable, and their handle is taken.
    -- detached — the handle was released but the database and its replica
    --            were kept. The row survives so the file stays attributable.
    state       TEXT NOT NULL CHECK (state IN ('active', 'detached')),
    -- When the handle was released. NULL while active.
    retired_at  TEXT
);

-- A handle is unique among ACTIVE users, and only among them.
--
-- This is what makes `detach` an ordinary state change instead of a primary
-- key rewrite: the retired row keeps the person's REAL handle, so an orphaned
-- database stays attributable without parsing a composite string, and the
-- handle is free for reuse the instant it is released. Two live users can
-- never share one, and the database is what says so.
CREATE UNIQUE INDEX IF NOT EXISTS user_one_active_handle
    ON user(handle) WHERE state = 'active';

-- ── KEY CUSTODY (pd-ulds) ──────────────────────────────────────────────────
--
-- Per-tenant encryption keys for the tenant zone are DERIVED, never stored:
-- HKDF(master key, database_id). So there is no key column here and never will
-- be — the only thing worth recording is whether this database's key may still
-- be derived at all. See crates/pkdump-keys.
--
-- Three properties of this table are decisions, not implementation:
--
--   1. NO FOREIGN KEY to user(database_id). A tombstone must OUTLIVE the row
--      it names. `pkdump tenant purge` deletes the user row; if that cascaded
--      to the tombstone, deletion would silently un-revoke the key it just
--      destroyed and the account would become readable again the moment the
--      partition was restored from anywhere. The tombstone is the durable
--      half of the deletion, so it deliberately does not depend on the
--      ephemeral half.
--   2. ABSENCE IS NOT PERMISSION. Derivation requires an explicit `active`
--      row; a database_id with no row here is refused (KeyError::NotRegistered)
--      rather than derived for. A registry restored empty is missing its
--      tombstones as well as its users, and the fail-closed direction is the
--      one where that is loud.
--   3. A TOMBSTONE IS TERMINAL. There is no path back to 'active' — not in
--      the accessor, and the CHECK below means an operator with sqlite3 open
--      on the file cannot half-do it either (clearing the state without
--      clearing tombstoned_at, or the reverse, fails the row constraint).
--      Revocation that can be undone by accident is not revocation.
--
-- What this table is NOT is the master key's business. The master key is a
-- file; this is a database. Backing the key up never touches a row here, and
-- tombstoning a tenant never touches that file — see crates/pkdump-keys, where
-- the two paths are kept apart on purpose.
CREATE TABLE IF NOT EXISTS tenant_key (
    -- The database whose key state this is. Opaque ULID, exactly as
    -- user.database_id — but joined by value, never by constraint (1, above).
    database_id   TEXT PRIMARY KEY,
    -- active     — the key may be derived.
    -- tombstoned — it may not, ever again. Deliberate revocation.
    state         TEXT NOT NULL CHECK (state IN ('active', 'tombstoned')),
    -- When the id was first registered here (RFC 3339).
    created_at    TEXT NOT NULL,
    -- When the key was revoked. NULL while active, and the two move together.
    tombstoned_at TEXT,
    -- Whatever the operator said at the time. Free text, for the audit trail.
    reason        TEXT,
    CHECK ((state = 'tombstoned') = (tombstoned_at IS NOT NULL))
);
