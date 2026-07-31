# Known upstream-data issues

Tracks bugs and gaps in PokeDumpster's upstream data sources and the
hand-curated overrides under `data/overrides/` that work around them. Check
here before investigating a suspected data bug.

## Variant coverage

No upstream source enumerates every printing of a card. PokeDumpster's
three-layer variant expansion (PLAN.md §4) handles this:

1. **Data-driven** — variants implied by pokemontcg.io's TCGplayer price keys.
2. **Rarity bootstrap** — fallback for brand-new sets pokemontcg.io has not
   priced yet.
3. **JSON overlay** — `data/overrides/variant_augmentations.json`.

Known gaps the overlay exists to fix:

- **Poké Ball / Master Ball pattern reverse holos** — exist for `151` and
  `Prismatic Evolutions` Common/Uncommon/Rare cards but are not modelled as
  TCGplayer price keys on the base card. Added via overlay rules.
- **Stamped promos** (Pokémon Center, prerelease, Build & Battle) share the
  base card's artwork and are not separate price keys. Added via overlay
  per-card rules as needed.

The system self-refines: when TCGplayer prices a new variant, layer 1 picks
it up automatically. Overlay rules are added lazily, only when a real gap is
noticed.

## MCAP (TCGCSV group 2374) residual

Group 2374, "Miscellaneous Cards and Products", is TCGplayer's catch-all for
promos whose base card lives in some other group. Cross-group resolution
(`pkdump-ingest/src/overrides.rs`) bridges them to the base card by either a
set-name keyword parsed out of the parenthetical, or — when the parenthetical
names no set, as with `(Prerelease)` — the card name plus the `/total` half of
the collector number. Two residual classes survived that work:

- **`extendedData.Number` truncated to the bare number.** Products 221176
  (`"Buck's Training - 130/146 (Prerelease)"`) and 532631 (its `[Staff]`
  sibling) ship `Number = "130"`; the `/146` survives only in the product
  *name*. With no `/total` the card-name gate had nothing to match against,
  and `"130"` is pure digits so it could not fall through to the
  promo-namespace escape hatch either — both products stayed unmodeled.
  Fixed by `tcgcsv::restore_truncated_set_total`, which recovers the total
  from the name at ingest time when the name's numerator agrees with the
  number upstream gave us. These two are the only products with this shape
  across every group we ingest (2374, 1840, 2289, 3179, 22872, 23266, 23561),
  so the repair is deliberately narrow.

- **Japanese-namespace promos — still open.** 16 single-card products in
  group 2374 sit in Japanese numbering namespaces (`S-P`, `ADV-P`, `SM-P`,
  and the 11th Movie Commemoration Set's `NNN/009`). They have no English
  base card to bridge to and cannot resolve until the Pokémon Japan ingest
  (TCGCSV category 85) carries those base sets. The two
  `"…Movie Commemoration Set"` entries without a `Number` are sealed sets and
  are correctly skipped as non-cards.

The rest of the unmodeled group-2374 products are foil-only collapse
siblings — the card is already represented by another product, by design.

## Upstream card corrections

`data/overrides/upstream_card_corrections.json` is the registry of cards
pokemontcg.io ships with a field that contradicts its own `id`/image — today
just `number` (e.g. `zsv10pt5-80` Antique Cover Fossil ships `number="60"`,
colliding with Escavalier at binder slot 60). `raw_json` is preserved
verbatim; only the materialized column is corrected.

Corrections are applied in two places:

- `upsert_card` — every card as it is ingested (`pkdump setup`, and the
  newest-sets tail in `pkdump data refresh`).
- `pkdump data apply-corrections` — already-ingested rows. `refresh`'s tail
  fetch skips sets the catalog already has, so a correction added or edited
  after a card landed never reaches its row through ingest. This subcommand
  UPDATEs `number` + recomputes `number_sortable` for every registered
  `card_id` present in the catalog; it is idempotent and takes `--dry-run`
  to report the rows it would change. `pkdump data refresh` runs it as a
  phase, so the nightly heals anything added since the last run — a manual
  invocation is only needed to heal immediately.

## Set-code bridging

`tcgplayer_groups.abbreviation` is matched against `sets.ptcgo_code` to link
TCGCSV groups to catalog sets. Mismatches leave `tcgplayer_groups.set_code`
NULL; corrections will go in `data/overrides/set_aliases.json` when that
consumer is built.
