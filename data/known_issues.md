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

## Set-code bridging

`tcgplayer_groups.abbreviation` is matched against `sets.ptcgo_code` to link
TCGCSV groups to catalog sets. Mismatches leave `tcgplayer_groups.set_code`
NULL; corrections will go in `data/overrides/set_aliases.json` when that
consumer is built.
