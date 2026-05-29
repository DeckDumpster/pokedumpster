-- Manual price entries — user-curated time series for printings the
-- upstream TCGplayer feed doesn't cover (basep promos are the motivating
-- case; their printings carry no tcgplayer_product_id, so the prices
-- table never produces a row for them). One entry per (printing, point
-- in time); the user can add as many as they want — typical sources are
-- recent sale comps or backfilled time series from third-party trackers.
--
-- printing_id is an app-layer FK to shared.printings (SQLite cannot
-- enforce FKs across the ATTACHed catalog DB; the repository layer
-- validates on insert).
--
-- Effective-price rule (consumed by collection-value queries): TCGplayer
-- market price wins when present; manual_prices is consulted only when
-- no TCGplayer row exists for the printing. See manual_prices.rs.

CREATE TABLE manual_prices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    printing_id  TEXT NOT NULL,
    price        REAL NOT NULL,
    observed_at  TEXT NOT NULL,
    note         TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_manual_prices_printing  ON manual_prices(printing_id, observed_at DESC);
