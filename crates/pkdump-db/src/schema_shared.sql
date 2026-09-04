-- The full shared-catalog schema, applied on every open of shared.sqlite.
-- Single-instance project (pokedumpster-luo): no migration history, no
-- refinery. New installs build to this exact shape; the existing prod
-- DB already matches it so every CREATE IF NOT EXISTS is a no-op.
-- Future schema changes: edit this file + manually apply the diff to
-- prod (it's one box, one user).

-- ---------------------------------------------------------------------
-- Not part of the shape (pd-yj40)
--
-- `refinery_schema_history` is the migration-history table the pre-luo
-- migration system left behind. Nothing has written to it since, and
-- every consumer that walks sqlite_master had to name it just to skip it.
-- Dropping it here removes both the table and the reason to know it
-- existed; a database that never had it is untouched, and re-opening one
-- that did writes nothing at all.
--
-- No `user_version` bump: an older binary reading a catalog without this
-- table is not wrong, merely missing a table it already ignored. Bumping
-- would refuse to open on rollback for a dead table.
-- ---------------------------------------------------------------------

DROP TABLE IF EXISTS refinery_schema_history;

-- ---------------------------------------------------------------------
-- Moved out of the catalog (pd-s4c2)
--
-- `conditions` — the card-condition value multipliers — now lives in the
-- per-tenant collection schema (`schema_user.sql`), seeded there with the
-- same five defaults. It was the one table in this file joined against
-- *collection* rows, so while it sat here a value computation crossed the
-- ATTACH boundary and a single multiplier edit would have changed the
-- meaning of every tenant's rendered values at once.
--
-- This one DOES bump `user_version` (Shared 1 -> 2), unlike the drop above:
-- an older binary attaching a catalog without `conditions` gets no TEMP
-- VIEW for it, and its value queries fail on a table that is not there —
-- wrong, not merely missing. The bump is what stops that build rather than
-- letting it serve a broken collection.
-- ---------------------------------------------------------------------

DROP TABLE IF EXISTS conditions;

-- ---------------------------------------------------------------------
-- Sets and cards
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS sets (
    set_code          TEXT PRIMARY KEY,
    ptcgo_code        TEXT,
    name              TEXT NOT NULL,
    series            TEXT NOT NULL,
    series_sort_order INTEGER,
    set_sort_order    INTEGER,
    total             INTEGER,
    printed_total     INTEGER,
    release_date      TEXT,
    logo_url          TEXT,
    symbol_url        TEXT,
    ptcgio_fetched_at TEXT,
    is_subset         INTEGER NOT NULL DEFAULT 0,
    parent_set_code   TEXT REFERENCES sets(set_code),
    symbol_source_url TEXT,
    -- Set of the TCGCSV group this row was auto-synthesized from, when
    -- pokemontcg.io had not published the set yet (pd-558b1e4f). NULL for
    -- every upstream-managed and bridge-declared set. Provenance only:
    -- once upstream publishes, `ptcgio_fetched_at` goes non-NULL and the
    -- row is upstream's, this column just records where it started.
    discovered_from_group_id INTEGER,
    -- Whether pokemontcg.io publishes this set's catalog at all (pd-mt57).
    -- Read it beside `ptcgio_fetched_at`, which is the pair that says what
    -- a locally-built set row MEANS:
    --   covered=1, fetched_at NULL -> upstream is BEHIND. The row is
    --     provisional and resolves itself the refresh after upstream
    --     publishes. Every English set is this case.
    --   covered=0, fetched_at NULL -> upstream does not carry this catalog
    --     and never will. The Japanese catalog (TCGCSV category 85) is the
    --     only one so far: pokemontcg.io has no Japanese data, so those 450
    --     sets are TCGCSV-native permanently, not provisionally.
    -- Without the distinction `ptcgio_fetched_at IS NULL` conflates the two,
    -- and every Japanese tile on /browse carries a "provisional" badge whose
    -- promise never comes due.
    ptcgio_covered INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS cards (
    card_id                   TEXT PRIMARY KEY,    -- "sv3pt5-6"
    set_code                  TEXT NOT NULL REFERENCES sets(set_code),
    number                    TEXT NOT NULL,       -- "6", "184", "GG01", "SWSH123"
    number_sortable           INTEGER NOT NULL,    -- see PLAN.md §3.4
    name                      TEXT NOT NULL,
    supertype                 TEXT,                -- 'Pokémon'/'Trainer'/'Energy'
    subtypes                  TEXT,                -- JSON array
    hp                        INTEGER,
    types                     TEXT,                -- JSON array
    rarity                    TEXT,
    artist                    TEXT,
    flavor_text               TEXT,
    attacks                   TEXT,                -- JSON
    abilities                 TEXT,                -- JSON
    weaknesses                TEXT,                -- JSON
    resistances               TEXT,                -- JSON
    retreat_cost              TEXT,                -- JSON array
    regulation_mark           TEXT,                -- 'F','G','H',...
    national_pokedex_numbers  TEXT,                -- JSON array
    legalities                TEXT,                -- JSON
    image_small               TEXT,
    image_large               TEXT,
    raw_json                  TEXT                 -- full API response
    -- No UNIQUE(set_code, number): real Pokémon data shares a collector
    -- number across distinct cards (e.g. Celebrations Classic Collection
    -- #15 has four artwork variants). card_id is the only key.
);
CREATE INDEX IF NOT EXISTS idx_cards_set    ON cards(set_code, number_sortable);
CREATE INDEX IF NOT EXISTS idx_cards_name   ON cards(name);
CREATE INDEX IF NOT EXISTS idx_cards_rarity ON cards(rarity);

-- External set-name aliases: an import platform's set label (Collectr's
-- "Scarlet & Violet Promo") mapped to our catalog set_code ('svp'). The
-- data model owns import synonyms — the resolver consults this table as a
-- fallback rather than hard-coding names. Seeded from data/set_aliases.json
-- at `pkdump setup` (see crate::set_aliases). NOCASE so a case-insensitive
-- label still matches its single canonical row.
CREATE TABLE IF NOT EXISTS set_aliases (
    alias    TEXT PRIMARY KEY COLLATE NOCASE,     -- 'Scarlet & Violet Promo'
    set_code TEXT NOT NULL REFERENCES sets(set_code)
);

-- ---------------------------------------------------------------------
-- Variants and printings
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS variants (
    code        TEXT PRIMARY KEY,           -- 'pokeball_rh'
    label       TEXT NOT NULL,              -- 'Poké Ball Reverse Holo'
    short       TEXT NOT NULL,              -- 'BALL'   (collection table tag)
    rank        INTEGER NOT NULL,           -- 3        (UI sort order, low first)
    color       TEXT NOT NULL,              -- '#e94560' (browse-slot chip pip)
    provenance  TEXT,                       -- human-readable origin
    tiebreak    INTEGER NOT NULL DEFAULT 0  -- intra-rank sort key
);

CREATE TABLE IF NOT EXISTS printings (
    printing_id            TEXT PRIMARY KEY,
    card_id                TEXT NOT NULL REFERENCES cards(card_id),
    variant                TEXT NOT NULL REFERENCES variants(code),
    language               TEXT NOT NULL DEFAULT 'en',
    tcgplayer_product_id   INTEGER,
    sub_type_name          TEXT,
    image_override         TEXT,
    badge_overlay          TEXT,
    deprecated_at          TEXT,
    UNIQUE(card_id, variant, language)
);
CREATE INDEX IF NOT EXISTS idx_printings_card ON printings(card_id);
CREATE INDEX IF NOT EXISTS idx_printings_tcg  ON printings(tcgplayer_product_id);

-- ---------------------------------------------------------------------
-- Search query language metadata (data-model-is-the-product; decision D1/D2).
-- Seeded from data/search_keywords.json, data/rarities.json,
-- data/search_flags.json by pkdump_db::search_meta::reconcile at
-- `pkdump setup` / `pkdump data refresh`. No FK references these — they are
-- the registry the parser/compiler and the autocomplete/help endpoints read.
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS search_keywords (
    canonical   TEXT PRIMARY KEY,   -- 'energy_type'
    aliases     TEXT NOT NULL,      -- JSON array, e.g. ["t","type"]
    operators   TEXT NOT NULL,      -- JSON array, e.g. [":","=","!="]
    kind        TEXT NOT NULL,      -- value class the compiler dispatches on
    target      TEXT,               -- column or JSON path, e.g. 'cards.types'
    value_enum  TEXT,               -- optional value-set reference
    semantics   TEXT,               -- compiler semantics tag ('exists','rank',…)
    help        TEXT                -- one-line help / autocomplete description
);

CREATE TABLE IF NOT EXISTS rarities (
    name   TEXT PRIMARY KEY,        -- 'Illustration Rare'
    rank   INTEGER NOT NULL,        -- curated ordinal for r>= / r<
    grp    TEXT                     -- group alias: 'secret','ultra','common'…
);

CREATE TABLE IF NOT EXISTS search_flags (
    flag       TEXT PRIMARY KEY,    -- 'holo'
    kind       TEXT NOT NULL,       -- 'variant_match' | 'computed'
    match_str  TEXT,                -- substring for variant_match flags
    predicate  TEXT,                -- predicate id for computed flags
    help       TEXT
);

-- ---------------------------------------------------------------------
-- TCGCSV catalog
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS tcgplayer_groups (
    group_id     INTEGER PRIMARY KEY,
    set_code     TEXT,
    name         TEXT NOT NULL,
    abbreviation TEXT,
    published_on TEXT,
    fetched_at   TEXT NOT NULL,
    role         TEXT NOT NULL DEFAULT 'primary'
);

-- Variant expansion resolves a card's products by walking
-- `tcgcsv_products` → `tcgplayer_groups` → `set_code`, once per card.
-- Without this index that's a full group scan per card, and the Japanese
-- catalog (categoryId 85) nearly doubles both sides of the join.
CREATE INDEX IF NOT EXISTS idx_tcgplayer_groups_set ON tcgplayer_groups(set_code);

CREATE TABLE IF NOT EXISTS tcgcsv_products (
    product_id        INTEGER PRIMARY KEY,
    group_id          INTEGER NOT NULL,
    name              TEXT NOT NULL,
    collector_number  TEXT,
    derived_variant   TEXT,
    fetched_at        TEXT NOT NULL,
    image_url         TEXT,
    rarity            TEXT
);
CREATE INDEX IF NOT EXISTS idx_tcgcsv_products_group  ON tcgcsv_products(group_id);
CREATE INDEX IF NOT EXISTS idx_tcgcsv_products_number ON tcgcsv_products(group_id, collector_number);

CREATE TABLE IF NOT EXISTS tcgcsv_sub_type_variant_map (
    tcgcsv_group_id INTEGER NOT NULL,                          -- 0 = global default
    sub_type_name   TEXT    NOT NULL,
    variant_code    TEXT    NOT NULL REFERENCES variants(code),
    PRIMARY KEY (tcgcsv_group_id, sub_type_name)
);

-- ---------------------------------------------------------------------
-- Bundles (logical-set containers — TTBB, etc.)
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS bundles (
    slug              TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    year              INTEGER NOT NULL,
    tcgcsv_group_id   INTEGER NOT NULL,
    series            TEXT NOT NULL DEFAULT 'Trick or Trade Bundle'
);

-- ---------------------------------------------------------------------
-- Prices
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS prices (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    tcgplayer_product_id  INTEGER NOT NULL,
    sub_type_name         TEXT NOT NULL,        -- 'Normal'|'Holofoil'|'Reverse Holofoil'|...
    source                TEXT NOT NULL DEFAULT 'tcgplayer',
    price_type            TEXT NOT NULL,        -- 'low'|'mid'|'high'|'market'|'directLow'
    price                 REAL NOT NULL,
    observed_at           TEXT NOT NULL,
    UNIQUE(tcgplayer_product_id, sub_type_name, source, price_type, observed_at)
);
CREATE INDEX IF NOT EXISTS idx_prices_product ON prices(tcgplayer_product_id);
CREATE INDEX IF NOT EXISTS idx_prices_date    ON prices(observed_at);

CREATE TABLE IF NOT EXISTS prices_cardmarket (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id         TEXT NOT NULL REFERENCES cards(card_id),
    variant         TEXT NOT NULL,             -- 'normal' or 'reverse_holo' (CM only splits these)
    avg_sell_price  REAL,
    low_price       REAL,
    trend_price     REAL,
    avg30           REAL,
    avg7            REAL,
    avg1            REAL,
    observed_at     TEXT NOT NULL,
    UNIQUE(card_id, variant, observed_at)
);

-- latest_prices is MATERIALIZED (a table, not a view — pokedumpster-vi37).
-- It was a view that GROUP BY'd the whole (multi-million-row) prices table;
-- every collection/search/binder row does a per-row market-price lookup, so
-- that view turned a page load into ~1.2s. As an indexed table the lookup is
-- a point read (~60ms for the whole collection). Rebuilt at ingest by
-- `pkdump_ingest::latest_prices::refresh_latest_prices` (pkdump setup /
-- data refresh), right after prices are appended.
--
-- An existing DB that still has the old VIEW is migrated by
-- `refresh_latest_prices`, which drops the view (only when it really is a
-- view — `DROP VIEW` on a table errors) before rebuilding. This CREATE is a
-- no-op while the old view still exists, so schema re-application stays
-- idempotent. Applied only by open_shared (ingest, read-write); the request
-- path attaches shared read-only and never runs it.
CREATE TABLE IF NOT EXISTS latest_prices (
    tcgplayer_product_id  INTEGER NOT NULL,
    sub_type_name         TEXT NOT NULL,
    source                TEXT NOT NULL,
    price_type            TEXT NOT NULL,
    price                 REAL NOT NULL,
    observed_at           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_latest_prices_lookup
    ON latest_prices(tcgplayer_product_id, sub_type_name, price_type);

-- Hand-curated prices for CATALOG printings TCGplayer does not price.
--
-- A printing the feed cannot price is a catalog defect, not a per-user
-- problem: the gap is identical for every tenant, so the patch belongs where
-- every tenant benefits from it (pd-m4gw). This is the same curated-patch
-- layer as data/overrides/variant_augmentations.json — the canonical source
-- is data/overrides/catalog_prices.json in git, reconciled here by
-- `crate::catalog_prices::reconcile` at open_shared. It is deliberately NOT
-- writable at request time: shared.sqlite is reproducible-from-upstream and
-- is not replicated off-box, so a price only entered here would be destroyed
-- by a catalog rebuild without anyone noticing.
--
-- `latest_prices` always wins — see `crate::prices::MARKET_PRICE_EXPR` — so
-- an override left in place after upstream starts pricing the printing is
-- inert rather than wrong.
CREATE TABLE IF NOT EXISTS catalog_price_overrides (
    printing_id  TEXT PRIMARY KEY REFERENCES printings(printing_id),
    price        REAL NOT NULL,
    observed_at  TEXT NOT NULL,
    note         TEXT
);

CREATE TABLE IF NOT EXISTS price_fetch_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source        TEXT NOT NULL,               -- 'tcgcsv'|'pokemontcg.io'
    set_code      TEXT,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    status        TEXT NOT NULL,               -- 'success'|'partial'|'failed'
    rows_inserted INTEGER,
    error         TEXT
);

-- ---------------------------------------------------------------------
-- Sealed products
-- ---------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS sealed_products (
    product_id      INTEGER PRIMARY KEY,       -- TCGplayer productId
    set_code        TEXT REFERENCES sets(set_code),
    name            TEXT NOT NULL,
    category        TEXT NOT NULL,             -- 'booster_pack'|'booster_box'|'etb'|'bundle'|'tin'|...
    subtype         TEXT,
    card_count      INTEGER,                   -- cards per pack
    product_size    INTEGER,                   -- packs per box/bundle
    release_date    TEXT,
    image_url       TEXT,
    tcgplayer_url   TEXT,
    fetched_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sealed_products_set      ON sealed_products(set_code);
CREATE INDEX IF NOT EXISTS idx_sealed_products_category ON sealed_products(category);

CREATE TABLE IF NOT EXISTS sealed_product_contents (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id      INTEGER NOT NULL REFERENCES sealed_products(product_id),
    slot_index      INTEGER NOT NULL,
    slot_label      TEXT,
    candidate_kind  TEXT NOT NULL,             -- 'card_id'|'rarity_pool'|'printing_id'
    candidate_value TEXT NOT NULL,
    weight          REAL DEFAULT 1.0
);
CREATE INDEX IF NOT EXISTS idx_spc_product ON sealed_product_contents(product_id);

CREATE TABLE IF NOT EXISTS sealed_prices (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    tcgplayer_product_id  INTEGER NOT NULL,
    low_price             REAL,
    mid_price             REAL,
    high_price            REAL,
    market_price          REAL,
    direct_low_price      REAL,
    observed_at           TEXT NOT NULL,
    UNIQUE(tcgplayer_product_id, observed_at)
);
CREATE INDEX IF NOT EXISTS idx_sealed_prices_product ON sealed_prices(tcgplayer_product_id);
CREATE INDEX IF NOT EXISTS idx_sealed_prices_date    ON sealed_prices(observed_at);

CREATE VIEW IF NOT EXISTS latest_sealed_prices AS
SELECT sp.* FROM sealed_prices sp
JOIN (SELECT tcgplayer_product_id, MAX(observed_at) AS observed_at
      FROM sealed_prices GROUP BY 1) m
  ON sp.tcgplayer_product_id = m.tcgplayer_product_id
 AND sp.observed_at = m.observed_at;

-- ---------------------------------------------------------------------
-- Provenance: which raw runs produced this catalog (pd-1uem)
-- ---------------------------------------------------------------------
--
-- Written ONLY by the offline derive (`pkdump-lake-derive shared`), one row
-- per (ingest_date, source, dataset) it replayed. The online `pkdump data
-- refresh` writes nothing here — it fetched from upstream, so there is no raw
-- run to name, and a row claiming otherwise would be a lie about where the
-- bytes came from.
--
-- The point is that a RERUN IS IDENTIFIABLE, not merely tolerable. Two
-- derives of one date are indistinguishable in every other table by design
-- (that is what idempotence means); this is where the difference that does
-- exist is recorded: which run ULID was replayed, how many parts it held, and
-- when the derive happened.
--
-- `observed_at` is the run's clock day, and it is deliberately NOT the same
-- column as `ingest_date`. They are the same day for almost every run and
-- differ for exactly the run that crossed UTC midnight — the run where taking
-- the partition for the observation date would file yesterday's prices under
-- today. Keeping both is what makes that case legible after the fact.
--
-- Delete-then-insert per ingest_date, so re-deriving a date replaces that
-- date's provenance rather than accumulating a row per attempt.
--
-- No `user_version` bump: additive CREATE ... IF NOT EXISTS, and a build that
-- predates this table simply does not read it. Nothing on the serving path
-- does either — this is a record for an operator, not an input.
CREATE TABLE IF NOT EXISTS raw_derivation (
    ingest_date  TEXT    NOT NULL,           -- the raw/ partition derived from
    source       TEXT    NOT NULL,           -- 'tcgcsv' | 'pokemontcgio' | ...
    dataset      TEXT    NOT NULL,           -- 'groups' | 'products' | ...
    run_id       TEXT    NOT NULL,           -- the run= ULID that was replayed
    parts        INTEGER NOT NULL,           -- how many payloads that run held
    complete     INTEGER NOT NULL,           -- the manifest's complete flag
    observed_at  TEXT    NOT NULL,           -- the run's clock day (see above)
    derived_at   TEXT    NOT NULL,           -- when this derive ran, RFC 3339
    PRIMARY KEY (ingest_date, source, dataset)
);

-- ── The convergence fingerprint (pd-dzu5) ──────────────────────────────────
-- One row, holding a SHA-256 over every embedded input the build that last
-- opened this catalog read-write would write into it: schema_shared.sql, the
-- ALTER TABLEs in connection.rs::ADDED_COLUMNS, the shared schema version,
-- and all six shipped seed files. See crates/pkdump-db/src/convergence.rs.
--
-- It exists so `pkdump serve` can ask, READ-ONLY, whether there is anything
-- for it to converge. Before it, every server start took the catalog's write
-- lock unconditionally — search_meta::reconcile alone is a DELETE and a few
-- hundred INSERTs — and a restart landing inside the nightly
-- `pkdump-lake-derive shared` failed on "database is locked" after five
-- seconds, then retried every fifteen until the build was over.
--
-- Written LAST by open_shared, once the convergence has really happened. A
-- catalog with no such table, or no row, or a different hash, is simply not
-- converged, which is the direction being wrong is cheap in.
--
-- The hash and nothing else. A `converged_at` beside it was the obvious second
-- column and it is deliberately absent: two derives of one raw/ partition must
-- be row-identical, that is what `pkdump-lake-derive diff` exists to check, and
-- a clock read here would make every such comparison differ in a field nobody
-- reads. `raw_derivation` earns its `derived_at` by being the operator's record
-- of which run produced this catalog and is excluded by name; this table is an
-- input to a decision the next process makes, and *when* it was written is not
-- part of that decision. The journal has the time.
--
-- No `user_version` bump: additive CREATE ... IF NOT EXISTS, and a build that
-- predates this table reads nothing from it.
CREATE TABLE IF NOT EXISTS catalog_convergence (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    fingerprint  TEXT NOT NULL            -- hex SHA-256, convergence::fingerprint
);
