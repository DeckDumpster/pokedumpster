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
    -- a constraint so the database is the last word on it:
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
