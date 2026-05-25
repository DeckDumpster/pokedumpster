-- sets.symbol_source_url — the upstream pokemontcg.io URL the normalized
-- /sym/<set_code>.png on disk was trimmed from. The symbol-normalization
-- phase of `pkdump setup` / `pkdump data refresh` rewrites symbol_url to
-- the local path and records the source URL here, so a subsequent refresh
-- can detect when pokemontcg.io rotates the upstream image and re-trim.
--
-- NULL means the row's symbol_url has not been processed yet (either it's
-- still pointing at an http upstream, or it's an override like
-- /sets/mep-symbol.svg that the pipeline leaves alone).

ALTER TABLE sets ADD COLUMN symbol_source_url TEXT;
