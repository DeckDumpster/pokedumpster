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
