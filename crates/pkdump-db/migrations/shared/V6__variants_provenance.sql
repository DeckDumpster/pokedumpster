-- variants.provenance — human-readable description of where this
-- variant's printings come from (e.g. "From a Build & Battle Box",
-- "Trick or Trade BOOster Bundle 2023"). Nullable: variants that lack
-- a single canonical source (plain holos, etc.) leave it NULL and the
-- UI shows no provenance line. Reconciled on every `pkdump setup` from
-- data/variants.json, which is the authoring source.

ALTER TABLE variants ADD COLUMN provenance TEXT;
