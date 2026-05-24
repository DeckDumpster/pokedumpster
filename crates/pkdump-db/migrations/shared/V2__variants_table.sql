-- variants — display metadata for every printing.variant code, so labels,
-- short table tags, sort ranks, and chip pip colors are data, not
-- scattered TS/Svelte heuristics. `data/variants.json` is the canonical
-- authoring source and `pkdump setup` re-seeds from it; this migration
-- only needs to leave the table in a state where the FK rebuild below
-- can complete.

CREATE TABLE variants (
    code   TEXT PRIMARY KEY,    -- 'pokeball_rh'
    label  TEXT NOT NULL,       -- 'Poké Ball Reverse Holo'
    short  TEXT NOT NULL,       -- 'BALL'   (collection table tag)
    rank   INTEGER NOT NULL,    -- 3        (UI sort order, low first)
    color  TEXT NOT NULL        -- '#e94560' (browse-slot chip pip)
);

INSERT INTO variants (code, label, short, rank, color) VALUES
  ('normal',              'Normal',                     'Normal', 0, '#bbbbbb'),
  ('first_ed_normal',     '1st Edition Normal',         '1ED',    0, '#d4af37'),
  ('holo',                'Holofoil',                   'H',      1, '#f0c878'),
  ('first_ed_holo',       '1st Edition Holofoil',       '1ED',    1, '#d4af37'),
  ('unlimited_holo',      'Unlimited Holofoil',         'U',      1, '#aa7733'),
  ('cosmos_holo',         'Cosmos Holo',                'COSMOS', 1, '#c0d0f0'),
  ('reverse_holo',        'Reverse Holo',               'R',      2, '#a0c4f0'),
  ('pokeball_rh',         'Poké Ball Reverse Holo',     'BALL',   3, '#e94560'),
  ('masterball_rh',       'Master Ball Reverse Holo',   'BALL',   3, '#9c5fb5'),
  ('quickball_rh',        'Quick Ball Reverse Holo',    'BALL',   3, '#4a8df0'),
  ('duskball_rh',         'Dusk Ball Reverse Holo',     'BALL',   3, '#3a3a52'),
  ('loveball_rh',         'Love Ball Reverse Holo',     'BALL',   3, '#f478a0'),
  ('friendball_rh',       'Friend Ball Reverse Holo',   'BALL',   3, '#5cb85c'),
  ('energy_symbol_rh',    'Energy Symbol Reverse Holo', 'ENERGY', 3, '#ffd24a'),
  ('team_rocket_rh',      'Team Rocket Reverse Holo',   'ROCKET', 3, '#2f1b1b'),
  ('stamp_prerelease',        'Prerelease Stamp',       'STAMP',  4, '#b88cc0'),
  ('stamp_prerelease_staff',  'Prerelease Staff Stamp', 'STAMP',  4, '#b88cc0'),
  ('stamp_buildbattle',       'Build & Battle Stamp',   'STAMP',  4, '#b88cc0'),
  ('stamp_pokemoncenter',     'Pokémon Center Stamp',   'STAMP',  4, '#b88cc0'),
  ('stamp_staff',             'Staff Stamp',            'STAMP',  4, '#b88cc0');

-- Catch any variant code already present in `printings` but not in our
-- seed (set-specific stamps written by prior ingest runs). Use a raw
-- code label so the FK rebuild below succeeds; `pkdump setup`'s
-- variants::reconcile pass replaces these with synthesized labels on
-- the next run.
INSERT INTO variants (code, label, short, rank, color)
SELECT DISTINCT p.variant, p.variant, 'STAMP', 4, '#b88cc0'
  FROM printings p
 WHERE p.variant NOT IN (SELECT code FROM variants);

-- Rebuild printings to add the FK to variants(code). SQLite has no
-- direct ALTER ADD FOREIGN KEY, so the canonical recipe is
-- CREATE NEW + COPY + DROP + RENAME (PRAGMA foreign_keys is off
-- inside refinery's migration transaction, so the copy is safe).
CREATE TABLE printings_new (
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
INSERT INTO printings_new (printing_id, card_id, variant, language,
                           tcgplayer_product_id, sub_type_name,
                           image_override, badge_overlay, deprecated_at)
SELECT printing_id, card_id, variant, language,
       tcgplayer_product_id, sub_type_name,
       image_override, badge_overlay, deprecated_at
  FROM printings;
DROP TABLE printings;
ALTER TABLE printings_new RENAME TO printings;
CREATE INDEX idx_printings_card ON printings(card_id);
CREATE INDEX idx_printings_tcg  ON printings(tcgplayer_product_id);
