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

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS user (
    -- What a request names; renameable without touching anything on disk.
    handle      TEXT PRIMARY KEY,
    -- Opaque ULID, and the filename stem of tenants/<database_id>.sqlite.
    -- UNIQUE so two handles can never be pointed at one database file.
    database_id TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL,
    -- active   — a live user; resolvable.
    -- detached — the handle was released but the database and its replica
    --            were kept. The row survives so the file stays attributable.
    state       TEXT NOT NULL CHECK (state IN ('active', 'detached'))
);
