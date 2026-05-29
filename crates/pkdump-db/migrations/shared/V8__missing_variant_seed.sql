-- Seed the canonical 'missing_variant' escape-hatch variant so user-DB
-- user_printings rows (decision pokedumpster-x7k) can FK to it without
-- requiring a full `pkdump setup` re-run. data/variants.json carries the
-- same entry for fresh installs.

INSERT OR IGNORE INTO variants (code, label, short, rank, tiebreak, color, provenance)
VALUES (
    'missing_variant',
    'Missing Variant',
    'MISS',
    999,
    0,
    '#777777',
    'User-curated catch-all for copies whose variant is not yet modelled in the catalog (misprints, undocumented promos, etc.). Each user_printings row carries its own free-text description.'
);
