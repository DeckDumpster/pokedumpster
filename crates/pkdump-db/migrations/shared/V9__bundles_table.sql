-- bundles — registry of TTBB-style logical-set views. The Rust BUNDLES
-- constant that previously hard-coded these is gone; data/bundles.json
-- is the canonical source and `pkdump setup` reseeds via
-- bundles::reconcile (data-model-is-the-product, pokedumpster-80q).

CREATE TABLE bundles (
    slug              TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    year              INTEGER NOT NULL,
    tcgcsv_group_id   INTEGER NOT NULL,
    series            TEXT NOT NULL DEFAULT 'Trick or Trade Bundle'
);
