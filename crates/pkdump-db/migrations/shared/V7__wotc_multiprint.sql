-- WotC-era multi-print coverage (pokedumpster-5is).
--
-- Three schema moves:
--   1. tcgplayer_groups.role discriminates the bridge from a TCGCSV group
--      to a set ('primary' for the regular run, 'shadowless' for Base
--      Set's group 1663, future codes as needed). The (set_code, role)
--      pair is the new way ingest finds which groups feed a set.
--   2. sets.tcgcsv_group_id is dropped — tcgplayer_groups.set_code is the
--      bridge (1:N from set → groups). Keeping the column would be a
--      data-model smell and force every multi-group set to lie about
--      which group is "the" group.
--   3. variants.tiebreak orders within a rank — used to place
--      first_ed_normal before shadowless_normal before normal inside a
--      single binder slot.
--
-- Plus a new lookup table:
--   tcgcsv_sub_type_variant_map — group-aware (tcgcsv_group_id,
--   sub_type_name) → variant_code. tcgcsv_group_id = 0 is the global
--   fallback (covers modern sets). Replaces the flat Rust match in
--   pkdump-core::variant::sub_type_to_variant. data/tcgcsv_sub_type_variants.json
--   is the authoring source; sub_type_map::reconcile re-applies it on
--   every `pkdump setup` like the variants seed.

ALTER TABLE tcgplayer_groups ADD COLUMN role TEXT NOT NULL DEFAULT 'primary';

ALTER TABLE variants ADD COLUMN tiebreak INTEGER NOT NULL DEFAULT 0;

CREATE TABLE tcgcsv_sub_type_variant_map (
    tcgcsv_group_id INTEGER NOT NULL,                          -- 0 = global default
    sub_type_name   TEXT    NOT NULL,
    variant_code    TEXT    NOT NULL REFERENCES variants(code),
    PRIMARY KEY (tcgcsv_group_id, sub_type_name)
);

-- Drop sets.tcgcsv_group_id. SQLite can't drop a column with a UNIQUE
-- index via plain DROP COLUMN, so do the canonical CREATE NEW + COPY +
-- DROP + RENAME. open_shared toggles `PRAGMA foreign_keys = OFF` around
-- the migration runner so the intermediate DROP TABLE (against which
-- cards.set_code etc. hold FKs) doesn't trip the checker. After the
-- rename, every FK target exists again.

CREATE TABLE sets_new (
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
    symbol_source_url TEXT
);
INSERT INTO sets_new (set_code, ptcgo_code, name, series, series_sort_order,
                      set_sort_order, total, printed_total, release_date,
                      logo_url, symbol_url, ptcgio_fetched_at, is_subset,
                      parent_set_code, symbol_source_url)
SELECT set_code, ptcgo_code, name, series, series_sort_order,
       set_sort_order, total, printed_total, release_date,
       logo_url, symbol_url, ptcgio_fetched_at, is_subset,
       parent_set_code, symbol_source_url
  FROM sets;
DROP TABLE sets;
ALTER TABLE sets_new RENAME TO sets;
