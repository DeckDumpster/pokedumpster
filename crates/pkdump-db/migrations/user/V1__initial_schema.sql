-- <user>.sqlite — a single user's mutable PokeDumpster collection.
-- The only data worth backing up; the catalog is reproducible.
-- See PLAN.md §3.3. Cross-database references (printing_id -> shared.printings,
-- card_id -> shared.cards, product_id -> shared.sealed_products) are NOT
-- declared as SQL foreign keys — SQLite cannot enforce them across an
-- ATTACHed database, so they are validated in the repository layer (§3.5).

CREATE TABLE binders (
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

CREATE TABLE decks (
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

CREATE TABLE orders (
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

CREATE TABLE batches (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_type  TEXT NOT NULL,                 -- 'manual_id'|'binder_click'|'csv_manabox'|'order_tcg'|...
    name        TEXT,
    notes       TEXT,
    order_id    INTEGER REFERENCES orders(id),
    binder_id   INTEGER REFERENCES binders(id),
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_batches_type ON batches(batch_type);

-- One row per physical card owned (strict — no quantity aggregation).
CREATE TABLE collection (
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
CREATE INDEX idx_collection_printing ON collection(printing_id);
CREATE INDEX idx_collection_binder   ON collection(binder_id);
CREATE INDEX idx_collection_deck     ON collection(deck_id);
CREATE INDEX idx_collection_status   ON collection(status);

-- Append-only audit of status transitions.
CREATE TABLE status_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    from_status   TEXT,
    to_status     TEXT NOT NULL,
    changed_at    TEXT NOT NULL,
    note          TEXT
);
CREATE INDEX idx_status_log_collection ON status_log(collection_id);

-- Append-only audit of binder/deck reassignments.
CREATE TABLE movement_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id   INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    from_binder_id  INTEGER REFERENCES binders(id),
    to_binder_id    INTEGER REFERENCES binders(id),
    from_deck_id    INTEGER REFERENCES decks(id),
    to_deck_id      INTEGER REFERENCES decks(id),
    changed_at      TEXT NOT NULL,
    note            TEXT
);
CREATE INDEX idx_movement_log_collection ON movement_log(collection_id);

CREATE TABLE wishlist (
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
CREATE INDEX idx_wishlist_card ON wishlist(card_id);

CREATE TABLE sealed_collection (
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
CREATE INDEX idx_sealed_collection_product ON sealed_collection(product_id);
CREATE INDEX idx_sealed_collection_status  ON sealed_collection(status);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
