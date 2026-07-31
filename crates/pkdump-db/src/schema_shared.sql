-- The full shared-catalog schema, applied on every open of shared.sqlite.
-- Single-instance project (pokedumpster-luo): no migration history, no
-- refinery. New installs build to this exact shape; the existing prod
-- DB already matches it so every CREATE IF NOT EXISTS is a no-op.
-- Future schema changes: edit this file + manually apply the diff to
-- prod (it's one box, one user).

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
    discovered_from_group_id INTEGER
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

-- Card-condition value multipliers (data-model-is-the-product; pokedumpster-e1vo).
-- The standard TCGplayer raw-card multipliers applied to a copy's Near-Mint
-- market price to estimate its value at its recorded condition. Seeded from
-- data/conditions.json by pkdump_db::conditions::reconcile. Read by the
-- frontend via /api/conditions (backs $lib/conditions.svelte) AND by the
-- Rust value-history snapshot/backfill — one source instead of a hardcoded
-- multiplier map duplicated in TypeScript.
CREATE TABLE IF NOT EXISTS conditions (
    name        TEXT PRIMARY KEY,   -- 'Near Mint' (matches collection.condition)
    multiplier  REAL NOT NULL,      -- 1.00, 0.85, 0.65, 0.45, 0.25
    rank        INTEGER NOT NULL    -- display/sort order, best first
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
