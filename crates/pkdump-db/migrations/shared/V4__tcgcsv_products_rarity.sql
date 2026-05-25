-- tcgcsv_products.rarity — pulled from each product's extendedData
-- "Rarity" entry at import time. Synth-cards reads this to populate
-- the `rarity` column on synthesized card rows, so MEP/SVP synth
-- entries surface the "Promo" glyph in the binder/collection views
-- like every other set does.

ALTER TABLE tcgcsv_products ADD COLUMN rarity TEXT;
