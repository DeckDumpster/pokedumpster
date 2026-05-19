-- shared.sqlite — immutable Pokémon TCG catalog.
-- Fully reproducible from upstream (pokemontcg.io / pokemon-tcg-data / TCGCSV).
-- See PLAN.md §3.2. Migration versioning is owned by refinery's
-- refinery_schema_history table; there is no hand-rolled schema_version table.

CREATE TABLE sets (
    set_code          TEXT PRIMARY KEY,        -- "sv3pt5" (pokemontcg.io id)
    ptcgo_code        TEXT,                    -- "MEW" (collector-facing 3-letter code)
    name              TEXT NOT NULL,           -- "151"
    series            TEXT NOT NULL,           -- "Scarlet & Violet"
    series_sort_order INTEGER,
    set_sort_order    INTEGER,
    total             INTEGER,                 -- printed total ("the 165")
    printed_total     INTEGER,                 -- including secret rares ("184")
    release_date      TEXT,
    logo_url          TEXT,
    symbol_url        TEXT,
    ptcgio_fetched_at TEXT,                    -- cache freshness marker
    tcgcsv_group_id   INTEGER UNIQUE,          -- bridges to TCGCSV
    is_subset         INTEGER NOT NULL DEFAULT 0,
    parent_set_code   TEXT REFERENCES sets(set_code)
);

CREATE TABLE cards (
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
CREATE INDEX idx_cards_set    ON cards(set_code, number_sortable);
CREATE INDEX idx_cards_name   ON cards(name);
CREATE INDEX idx_cards_rarity ON cards(rarity);

CREATE TABLE printings (
    printing_id            TEXT PRIMARY KEY,    -- "sv3pt5-6-normal"
    card_id                TEXT NOT NULL REFERENCES cards(card_id),
    variant                TEXT NOT NULL,        -- flat enum, see RESEARCH.md §4.2
    language               TEXT NOT NULL DEFAULT 'en',
    tcgplayer_product_id   INTEGER,              -- bridges to TCGCSV pricing
    image_override         TEXT,                 -- usually NULL
    badge_overlay          TEXT,                 -- 'STAMP'|'PRERELEASE'|...
    deprecated_at          TEXT,                 -- soft-delete for overlay removals
    UNIQUE(card_id, variant, language)
);
CREATE INDEX idx_printings_card ON printings(card_id);
CREATE INDEX idx_printings_tcg  ON printings(tcgplayer_product_id);

CREATE TABLE prices (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    tcgplayer_product_id  INTEGER NOT NULL,
    sub_type_name         TEXT NOT NULL,        -- 'Normal'|'Holofoil'|'Reverse Holofoil'|...
    source                TEXT NOT NULL DEFAULT 'tcgplayer',
    price_type            TEXT NOT NULL,        -- 'low'|'mid'|'high'|'market'|'directLow'
    price                 REAL NOT NULL,
    observed_at           TEXT NOT NULL,
    UNIQUE(tcgplayer_product_id, sub_type_name, source, price_type, observed_at)
);
CREATE INDEX idx_prices_product ON prices(tcgplayer_product_id);
CREATE INDEX idx_prices_date    ON prices(observed_at);

CREATE TABLE prices_cardmarket (
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

CREATE VIEW latest_prices AS
SELECT p.* FROM prices p
JOIN (SELECT tcgplayer_product_id, sub_type_name, source, price_type,
             MAX(observed_at) AS observed_at
      FROM prices GROUP BY 1, 2, 3, 4) m
  ON p.tcgplayer_product_id = m.tcgplayer_product_id
 AND p.sub_type_name = m.sub_type_name
 AND p.source = m.source
 AND p.price_type = m.price_type
 AND p.observed_at = m.observed_at;

CREATE TABLE price_fetch_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source        TEXT NOT NULL,               -- 'tcgcsv'|'pokemontcg.io'
    set_code      TEXT,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    status        TEXT NOT NULL,               -- 'success'|'partial'|'failed'
    rows_inserted INTEGER,
    error         TEXT
);

CREATE TABLE tcgplayer_groups (
    group_id     INTEGER PRIMARY KEY,
    set_code     TEXT,
    name         TEXT NOT NULL,
    abbreviation TEXT,
    published_on TEXT,
    fetched_at   TEXT NOT NULL
);

CREATE TABLE sealed_products (
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
CREATE INDEX idx_sealed_products_set      ON sealed_products(set_code);
CREATE INDEX idx_sealed_products_category ON sealed_products(category);

-- Optional pull-recipe table for the "open product" flow. Hand-curated.
CREATE TABLE sealed_product_contents (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id      INTEGER NOT NULL REFERENCES sealed_products(product_id),
    slot_index      INTEGER NOT NULL,
    slot_label      TEXT,
    candidate_kind  TEXT NOT NULL,             -- 'card_id'|'rarity_pool'|'printing_id'
    candidate_value TEXT NOT NULL,
    weight          REAL DEFAULT 1.0
);
CREATE INDEX idx_spc_product ON sealed_product_contents(product_id);

CREATE TABLE sealed_prices (
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
CREATE INDEX idx_sealed_prices_product ON sealed_prices(tcgplayer_product_id);
CREATE INDEX idx_sealed_prices_date    ON sealed_prices(observed_at);

CREATE VIEW latest_sealed_prices AS
SELECT sp.* FROM sealed_prices sp
JOIN (SELECT tcgplayer_product_id, MAX(observed_at) AS observed_at
      FROM sealed_prices GROUP BY 1) m
  ON sp.tcgplayer_product_id = m.tcgplayer_product_id
 AND sp.observed_at = m.observed_at;
