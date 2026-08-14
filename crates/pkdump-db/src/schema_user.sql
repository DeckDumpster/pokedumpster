-- The full per-user collection schema, applied on every open of the
-- user database (default collection.sqlite). Single-instance project
-- (pokedumpster-luo): no migration history, no refinery. New installs
-- build to this exact shape; the existing prod DB already matches it so
-- every CREATE IF NOT EXISTS is a no-op.

-- ---------------------------------------------------------------------
-- Not part of the shape (pd-yj40)
--
-- `refinery_schema_history` is the migration-history table the pre-luo
-- migration system left behind. Nothing has written to it since, and
-- every consumer that walks sqlite_master had to name it just to skip it
-- — including the JSON export, which would otherwise carry a dead
-- migration log into a fresh collection. Dropping it here removes both
-- the table and the reason to know it existed; a database that never had
-- it is untouched, and re-opening one that did writes nothing at all.
--
-- No `user_version` bump: an older binary reading a collection without
-- this table is not wrong, merely missing a table it already ignored.
-- Bumping would refuse to open on rollback for a dead table.
-- ---------------------------------------------------------------------

DROP TABLE IF EXISTS refinery_schema_history;

-- ---------------------------------------------------------------------
-- Settings (key/value)
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- ---------------------------------------------------------------------
-- Card-condition value multipliers (data-model-is-the-product;
-- pokedumpster-e1vo, moved here from the catalog by pd-s4c2)
--
-- The standard TCGplayer raw-card multipliers applied to a copy's Near-Mint
-- market price to estimate its value at its recorded condition. Read by the
-- frontend via /api/conditions (backs $lib/conditions.svelte), by the
-- collection search's `order:value`, and by the Rust value-history
-- snapshot/backfill — one source instead of a hardcoded multiplier map
-- duplicated in TypeScript.
--
-- It lives beside `collection` rather than in the catalog because that is
-- what it is joined to: `conditions.name` matches `collection.condition`,
-- so while it sat in the catalog every value computation crossed the ATTACH
-- boundary and every tenant shared one row set. Seeded with the five
-- defaults from data/conditions.json by pkdump_db::conditions::seed_defaults
-- on every open — insert-if-absent, never an overwrite, so the rows are the
-- tenant's own.
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS conditions (
    name        TEXT PRIMARY KEY,   -- 'Near Mint' (matches collection.condition)
    multiplier  REAL NOT NULL,      -- 1.00, 0.85, 0.65, 0.45, 0.25
    rank        INTEGER NOT NULL    -- display/sort order, best first
);

-- ---------------------------------------------------------------------
-- Containers: binders, decks
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS binders (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT NOT NULL,
    description       TEXT,
    color             TEXT,
    binder_type       TEXT,
    pocket_size       INTEGER NOT NULL DEFAULT 9,
    storage_location  TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS decks (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT NOT NULL,
    description       TEXT,
    format            TEXT,                    -- free-text tag: 'standard'|'expanded'|'casual'
    owner             TEXT,                    -- free-text: "Ryan"|"Alice"
    state             TEXT NOT NULL DEFAULT 'idea'
        CHECK (state IN ('idea', 'ready', 'built')),
    sleeve_color      TEXT,
    storage_location  TEXT,
    notes             TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

-- ---------------------------------------------------------------------
-- Orders and batches
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS orders (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    order_number       TEXT,
    source             TEXT NOT NULL,          -- 'tcgplayer'|'ebay'|'pokemoncenter'|'lgs'|'other'
    seller_name        TEXT,
    order_date         TEXT,
    subtotal           REAL,
    shipping           REAL,
    tax                REAL,
    total              REAL,
    shipping_status    TEXT,
    estimated_delivery TEXT,
    notes              TEXT,
    created_at         TEXT NOT NULL,
    UNIQUE(source, order_number)
);

CREATE TABLE IF NOT EXISTS batches (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_type  TEXT NOT NULL,                 -- 'manual_id'|'binder_click'|'csv_manabox'|'order_tcg'|...
    name        TEXT,
    notes       TEXT,
    order_id    INTEGER REFERENCES orders(id),
    binder_id   INTEGER REFERENCES binders(id),
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_batches_type ON batches(batch_type);

-- ---------------------------------------------------------------------
-- The collection: one row per physical card. App-layer FKs into the
-- shared catalog (`printing_id` -> shared.printings) since SQLite has no
-- cross-database FK enforcement.
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS collection (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    printing_id     TEXT NOT NULL,             -- -> shared.printings (app-layer FK)
    condition       TEXT NOT NULL DEFAULT 'Near Mint'
        CHECK (condition IN ('Near Mint', 'Lightly Played', 'Moderately Played',
                             'Heavily Played', 'Damaged')),
    language        TEXT NOT NULL DEFAULT 'English',
    purchase_price  REAL,
    sale_price      REAL,
    acquired_at     TEXT NOT NULL,
    source          TEXT NOT NULL,             -- 'manual_id'|'binder_click'|'csv_manabox'|'order_import'|...
    notes           TEXT,
    tags            TEXT,                      -- JSON array
    graded          INTEGER NOT NULL DEFAULT 0,
    grade_company   TEXT,                      -- 'PSA'|'BGS'|'CGC'|'SGC'|'TAG'|'ACE'|...
    grade_value     REAL,
    grade_cert      TEXT,
    status          TEXT NOT NULL DEFAULT 'owned'
        CHECK (status IN ('owned', 'ordered', 'listed', 'sold', 'removed',
                          'traded', 'gifted', 'lost')),
    order_id        INTEGER REFERENCES orders(id),
    binder_id       INTEGER REFERENCES binders(id) ON DELETE SET NULL,
    deck_id         INTEGER REFERENCES decks(id) ON DELETE SET NULL,
    batch_id        INTEGER REFERENCES batches(id),
    -- A physical card is in at most one container.
    CHECK (binder_id IS NULL OR deck_id IS NULL)
);
CREATE INDEX IF NOT EXISTS idx_collection_printing ON collection(printing_id);
CREATE INDEX IF NOT EXISTS idx_collection_binder   ON collection(binder_id);
CREATE INDEX IF NOT EXISTS idx_collection_deck     ON collection(deck_id);
CREATE INDEX IF NOT EXISTS idx_collection_status   ON collection(status);

-- ---------------------------------------------------------------------
-- Lifecycle logs (audit trail for status + container moves)
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS status_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    from_status   TEXT,
    to_status     TEXT NOT NULL,
    changed_at    TEXT NOT NULL,
    note          TEXT
);
CREATE INDEX IF NOT EXISTS idx_status_log_collection ON status_log(collection_id);

CREATE TABLE IF NOT EXISTS movement_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id   INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    from_binder_id  INTEGER REFERENCES binders(id),
    to_binder_id    INTEGER REFERENCES binders(id),
    from_deck_id    INTEGER REFERENCES decks(id),
    to_deck_id      INTEGER REFERENCES decks(id),
    changed_at      TEXT NOT NULL,
    note            TEXT
);
CREATE INDEX IF NOT EXISTS idx_movement_log_collection ON movement_log(collection_id);

-- ---------------------------------------------------------------------
-- The ownership outbox (pd-5m54)
--
-- Every change to `collection` — the tenant's holdings — appended as an
-- event, IN THE SAME TRANSACTION as the change itself. The offline side
-- (the lakehouse tenant zone) is fed from this table, so that it is
-- eventually consistent BY CONSTRUCTION rather than by a second write
-- that a crash can lose: dual-writing SQLite and a bucket has no
-- atomicity, and the disagreement it leaves behind is undetectable.
--
-- The writer is the three triggers below, NOT the Rust call sites, and
-- that is the whole point. A trigger fires inside the statement's own
-- transaction, so there is no instant at which a holding has changed and
-- the event has not — no window to crash in, nothing to remember to
-- call. It also means the paths that write `collection` in raw SQL
-- (orders.rs, import.rs, json_backup.rs, the fixture seeder) are covered
-- without knowing this table exists, and a path added tomorrow is
-- covered before it is written.
--
-- `seq` is the ordering authority and the gap detector: AUTOINCREMENT, so
-- a value is never reused after the shipper trims a shipped prefix, and a
-- missing number is a LOST EVENT rather than a deleted one. `occurred_at`
-- is metadata — `datetime('now')` ties at millisecond resolution and
-- cannot order two events inside one transaction.
--
-- `payload` is the whole row as JSON: the post-image for insert/update,
-- the pre-image for delete. Whole, so that a consumer needing a column
-- nobody anticipated does not need a schema change here, and so that no
-- column can be silently omitted — `outbox.rs` asserts the payload keys
-- against `PRAGMA table_info(collection)`.
--
-- `source_table` is constant today (every row says 'collection'; sealed
-- holdings are deliberately out of scope, see pd-4gop) and carries no
-- CHECK. It exists because this file has no migration mechanism: a column
-- added later needs an ALTER nothing here can express, whereas another
-- source only needs three more triggers.
--
-- No tenant column. One collection is one database file, so the file IS
-- the tenant; a `database_id` here would be a second, staler copy of what
-- the registry already says, and handles are renameable.
--
-- CHANGING A TRIGGER BODY needs a deliberate `DROP TRIGGER` above the
-- CREATE, exactly like the refinery drop at the top of this file:
-- `IF NOT EXISTS` will not replace a trigger that an existing collection
-- already carries, and a stale trigger writes a stale payload forever.
-- ---------------------------------------------------------------------

-- `source` is PROVENANCE, and it is the one column consumers must not
-- branch on (pd-385w, rule 2 of the design's backfill section). A trigger
-- writes 'trigger'; `pkdump outbox emit` writes 'backfill' or 'redrive'.
-- The audit trail stays honest and the handling stays identical — the
-- moment the shipper reads this column, backfill stops being the same path
-- as the everyday one and the rare path is untested again.
--
-- DEFAULT 'trigger' rather than a fourth `json_object` argument in each of
-- the three trigger bodies, and that is deliberate: the note above about
-- `IF NOT EXISTS` not replacing a trigger applies here too. Every writer
-- that is not the emitter is a trigger, so the default IS the rule, and a
-- collection carrying the pre-pd-385w triggers keeps labelling its events
-- correctly without those triggers being replaced.
CREATE TABLE IF NOT EXISTS ownership_outbox (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at   TEXT NOT NULL,             -- UTC ISO-8601, millisecond
    source_table  TEXT NOT NULL,             -- 'collection'
    op            TEXT NOT NULL CHECK (op IN ('insert', 'update', 'delete')),
    row_id        INTEGER NOT NULL,          -- collection.id
    payload       TEXT NOT NULL,             -- JSON object: the whole row
    source        TEXT NOT NULL DEFAULT 'trigger'
        CHECK (source IN ('trigger', 'backfill', 'redrive'))
);

-- What `emit` reads to date each row it re-emits: the newest event this
-- outbox already holds for a (source_table, row_id). Without it a backfill
-- over a real collection is a full scan of the outbox per row.
CREATE INDEX IF NOT EXISTS idx_ownership_outbox_row
    ON ownership_outbox(source_table, row_id, occurred_at);

CREATE TRIGGER IF NOT EXISTS collection_outbox_insert
AFTER INSERT ON collection
BEGIN
    INSERT INTO ownership_outbox
        (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'collection', 'insert', NEW.id,
        json_object(
            'id',             NEW.id,
            'printing_id',    NEW.printing_id,
            'condition',      NEW.condition,
            'language',       NEW.language,
            'purchase_price', NEW.purchase_price,
            'sale_price',     NEW.sale_price,
            'acquired_at',    NEW.acquired_at,
            'source',         NEW.source,
            'notes',          NEW.notes,
            'tags',           NEW.tags,
            'graded',         NEW.graded,
            'grade_company',  NEW.grade_company,
            'grade_value',    NEW.grade_value,
            'grade_cert',     NEW.grade_cert,
            'status',         NEW.status,
            'order_id',       NEW.order_id,
            'binder_id',      NEW.binder_id,
            'deck_id',        NEW.deck_id,
            'batch_id',       NEW.batch_id
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS collection_outbox_update
AFTER UPDATE ON collection
BEGIN
    INSERT INTO ownership_outbox
        (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'collection', 'update', NEW.id,
        json_object(
            'id',             NEW.id,
            'printing_id',    NEW.printing_id,
            'condition',      NEW.condition,
            'language',       NEW.language,
            'purchase_price', NEW.purchase_price,
            'sale_price',     NEW.sale_price,
            'acquired_at',    NEW.acquired_at,
            'source',         NEW.source,
            'notes',          NEW.notes,
            'tags',           NEW.tags,
            'graded',         NEW.graded,
            'grade_company',  NEW.grade_company,
            'grade_value',    NEW.grade_value,
            'grade_cert',     NEW.grade_cert,
            'status',         NEW.status,
            'order_id',       NEW.order_id,
            'binder_id',      NEW.binder_id,
            'deck_id',        NEW.deck_id,
            'batch_id',       NEW.batch_id
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS collection_outbox_delete
AFTER DELETE ON collection
BEGIN
    INSERT INTO ownership_outbox
        (occurred_at, source_table, op, row_id, payload)
    VALUES (
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'collection', 'delete', OLD.id,
        json_object(
            'id',             OLD.id,
            'printing_id',    OLD.printing_id,
            'condition',      OLD.condition,
            'language',       OLD.language,
            'purchase_price', OLD.purchase_price,
            'sale_price',     OLD.sale_price,
            'acquired_at',    OLD.acquired_at,
            'source',         OLD.source,
            'notes',          OLD.notes,
            'tags',           OLD.tags,
            'graded',         OLD.graded,
            'grade_company',  OLD.grade_company,
            'grade_value',    OLD.grade_value,
            'grade_cert',     OLD.grade_cert,
            'status',         OLD.status,
            'order_id',       OLD.order_id,
            'binder_id',      OLD.binder_id,
            'deck_id',        OLD.deck_id,
            'batch_id',       OLD.batch_id
        )
    );
END;

-- ---------------------------------------------------------------------
-- The emit ledger (pd-385w)
--
-- One row per `pkdump outbox emit` run: what scope it covered, what
-- provenance it wrote, how many events it appended and which `seq` range
-- they landed on.
--
-- This is rule 4 of the design's backfill section — "re-running is safe but
-- not silent". Replay IS idempotent (the payload is a whole row, so
-- applying it twice is an upsert to the same value), so nothing here exists
-- to make a second run correct. It exists so a second full backfill is a
-- decision: without `--force` the emitter refuses and names the date the
-- first one completed. Idempotent does not mean invisible, and an operator
-- re-running a backfill at 3am because they have lost track of whether it
-- already ran is exactly the moment to say "yes, on the 14th".
--
-- A run and the events it wrote land in ONE transaction, so this table
-- cannot record a run whose events are not there, and there is no
-- in-flight state to represent. That is the same property the outbox
-- triggers have and it is here for the same reason: a ledger that can
-- disagree with the log it describes is worse than no ledger.
--
-- Transport state, not collection state — like the outbox itself, and for
-- the same reason: it is absent from the portable JSON envelope in both
-- directions (`crate::outbox::TRANSPORT_TABLES`).
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS ownership_emit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    scope         TEXT NOT NULL,             -- 'collection' | 'seq:A..B' | 'row:N'
    source        TEXT NOT NULL              -- the provenance written
        CHECK (source IN ('backfill', 'redrive')),
    completed_at  TEXT NOT NULL,             -- UTC ISO-8601
    rows_emitted  INTEGER NOT NULL DEFAULT 0,
    seq_first     INTEGER,                   -- the range this run appended,
    seq_last      INTEGER,                   --   NULL when it emitted nothing
    forced        INTEGER NOT NULL DEFAULT 0 -- --force was given
);

-- ---------------------------------------------------------------------
-- The shipper's end of the outbox (pd-dxn3)
-- ---------------------------------------------------------------------
--
-- Two tables the shipper owns, here rather than anywhere else for the
-- reason the outbox is here: they point INTO it, so a restore of this
-- file brings back the log and the position in it together, and the
-- deletion that drops a tenant drops their shipping state with them. Both
-- are transport state rather than collection state, so — like the outbox
-- and its emit ledger — neither is carried by the portable JSON envelope
-- (`crate::outbox::TRANSPORT_TABLES`, subtracted in json_backup.rs).
--
-- `ownership_outbox_cursor` is one row, ever: the highest seq known to be
-- in the tenant zone. It is written AFTER the object lands, never before,
-- which is what makes delivery at-least-once rather than at-most-once. A
-- crash in between re-ships a part, and that is harmless because a part's
-- object key is the sequence range it carries — the retry addresses the
-- object it is retrying rather than writing a second copy beside it.
--
-- `ownership_outbox_gap` is the durable half of gap detection. A missing
-- seq means an event was LOST, and it is recorded here BEFORE the cursor
-- advances past it — because once the cursor is past, nothing can notice
-- the hole again. A journal line would have answered "was anything lost"
-- only until the journal rotated; this answers it for as long as the
-- collection exists. Rows are never deleted by the shipper: an operator
-- who has reconciled a gap is the one who clears it.
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS ownership_outbox_cursor (
    id            INTEGER PRIMARY KEY CHECK (id = 1),  -- one row, ever
    shipped_thru  INTEGER NOT NULL,          -- highest seq known to be shipped
    shipped_at    TEXT NOT NULL              -- when that last became true
);

CREATE TABLE IF NOT EXISTS ownership_outbox_gap (
    from_seq      INTEGER NOT NULL,          -- first missing seq, inclusive
    to_seq        INTEGER NOT NULL,          -- last missing seq, inclusive
    detected_at   TEXT NOT NULL,
    PRIMARY KEY (from_seq, to_seq),
    CHECK (to_seq >= from_seq)
);

-- ---------------------------------------------------------------------
-- Wishlist
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS wishlist (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id       TEXT NOT NULL,               -- -> shared.cards (app-layer FK)
    printing_id   TEXT,                        -- NULL = any printing
    max_price     REAL,
    priority      INTEGER NOT NULL DEFAULT 0,
    notes         TEXT,
    added_at      TEXT NOT NULL,
    source        TEXT NOT NULL DEFAULT 'manual',
    fulfilled_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_wishlist_card ON wishlist(card_id);

-- ---------------------------------------------------------------------
-- Sealed collection
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS sealed_collection (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id     INTEGER NOT NULL,           -- -> shared.sealed_products (app-layer FK)
    quantity       INTEGER NOT NULL DEFAULT 1,
    condition      TEXT DEFAULT 'Near Mint',
    purchase_price REAL,
    sale_price     REAL,
    purchase_date  TEXT,
    source         TEXT,
    seller_name    TEXT,
    notes          TEXT,
    status         TEXT NOT NULL DEFAULT 'owned'
        CHECK (status IN ('owned', 'listed', 'sold', 'traded', 'gifted', 'opened')),
    added_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sealed_collection_product ON sealed_collection(product_id);
CREATE INDEX IF NOT EXISTS idx_sealed_collection_status  ON sealed_collection(status);

-- ---------------------------------------------------------------------
-- Manual prices (hand-entered fallback for printings TCGplayer doesn't price)
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS manual_prices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    printing_id  TEXT NOT NULL,
    price        REAL NOT NULL,
    observed_at  TEXT NOT NULL,
    note         TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_manual_prices_printing ON manual_prices(printing_id, observed_at DESC);

-- ---------------------------------------------------------------------
-- User-created printings (for upstream-missing variants — the "Missing
-- Variant" escape hatch on card detail).
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS user_printings (
    printing_id  TEXT PRIMARY KEY,
    card_id      TEXT NOT NULL,
    variant      TEXT NOT NULL DEFAULT 'missing_variant',
    description  TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_user_printings_card ON user_printings(card_id);

-- ---------------------------------------------------------------------
-- Import dead-letter queue (unresolved import rows). A persistent backlog
-- of import rows that didn't resolve to a catalog item: the user manually
-- matches each to a printing/product (replaying `raw`) or dismisses it.
-- One global queue, filterable by the import (`batch_id`). (pokedumpster-oq3i.5)
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS import_unresolved (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    kind          TEXT NOT NULL CHECK (kind IN ('single','sealed')),
    source        TEXT NOT NULL,          -- 'csv_collectr' | 'csv_manabox' | ...
    batch_id      INTEGER REFERENCES batches(id),  -- import that parked it (nullable)
    source_line   INTEGER,                -- original CSV line
    raw           TEXT NOT NULL,          -- JSON of ParsedRow/ParsedSealedRow (replay source)
    set_hint      TEXT,
    number        TEXT,                   -- singles only
    name          TEXT,                   -- display hint (card/product name)
    variant       TEXT,                   -- singles only
    quantity      INTEGER,                -- sealed only (default 1)
    reason        TEXT NOT NULL,          -- resolver's unmatched reason
    status        TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open','resolved','dismissed')),
    resolved_printing_id   TEXT,          -- picked printing (kind='single')
    resolved_product_id    INTEGER,       -- picked product (kind='sealed')
    resolved_collection_id INTEGER,       -- collection row created on resolve
    resolved_sealed_id     INTEGER,       -- sealed_collection row created on resolve
    parked_at     TEXT NOT NULL,
    resolved_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_import_unresolved_status ON import_unresolved(status);

-- ---------------------------------------------------------------------
-- Collection value history (pokedumpster-e1vo). One row per
-- (date, dimension, bucket): the total market value, cost basis, and card
-- count of the owned collection on that date, for the whole collection
-- ('all', bucket NULL), per set ('set', bucket = set_code), or per binder
-- ('binder', bucket = binder-id-as-text). Written idempotently — a full
-- delete-then-insert per (date, dimension) — by the nightly snapshot and
-- the one-time backfill. Read by GET /api/collection/value-history.
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS collection_value_snapshot (
    date          TEXT NOT NULL,             -- 'YYYY-MM-DD'
    dimension     TEXT NOT NULL,             -- 'all' | 'set' | 'binder'
    bucket        TEXT,                      -- NULL for 'all'; set_code / binder id
    market_value  REAL NOT NULL,
    cost_basis    REAL NOT NULL,
    card_count    INTEGER NOT NULL,
    PRIMARY KEY (date, dimension, bucket)
);

-- Where one day's snapshot rows came from (pd-ruwh). The lake transform
-- (lake/src/pkdump_lake/value_snapshots.py) computes the rows above from
-- `catalog.prices` at a PINNED Nessie commit, and a value derived from a
-- versioned catalog is only worth as much as the record of which version.
-- One row per (date, artefact), rewritten with the snapshot rows it explains,
-- so the pair cannot drift apart.
--
-- Deliberately NOT a column on collection_value_snapshot: those rows are the
-- frozen chart contract, and `tests/lake/value_snapshots.sh` requires the
-- transform to reproduce them byte-identically to `value_history::snapshot_
-- today`. A provenance column there would make that comparison impossible to
-- state. Nothing on the serving path reads this table — it is for the operator
-- asking "which catalog said my collection was worth that".
CREATE TABLE IF NOT EXISTS collection_value_snapshot_run (
    date        TEXT NOT NULL,               -- the snapshot date these rows are for
    artefact    TEXT NOT NULL,               -- the lake table read, e.g. 'catalog.prices'
    lake_ref    TEXT NOT NULL,               -- pinned Nessie ref, 'main@<commit-hash>'
    rows        INTEGER NOT NULL,            -- snapshot rows written for that date
    written_at  TEXT NOT NULL,               -- UTC ISO-8601
    PRIMARY KEY (date, artefact)
);
