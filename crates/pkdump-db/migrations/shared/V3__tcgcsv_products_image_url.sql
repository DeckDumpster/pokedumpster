-- tcgcsv_products.image_url — populated at import time so the
-- synth-cards path (pokedumpster-itw) can surface MEP/etc. binder
-- art without a second TCGCSV round-trip. Nullable: older rows
-- pre-migration carry NULL until the next data refresh repopulates.

ALTER TABLE tcgcsv_products ADD COLUMN image_url TEXT;
