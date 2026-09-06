# Intents UI Test Fixture

Deterministic Pokémon fixture for PokeDumpster's intents UI-testing harness.
The harness snapshots/restores these two files before each test (see
`conftest.ts`); the running server opens them as its shared catalog and
user collection.

## Regenerating

```bash
cargo run -p pkdump-cli -- seed-fixture
# or, into an explicit directory:
cargo run -p pkdump-cli -- seed-fixture --out tests/ui/fixtures
```

The command does a clean rebuild: it deletes any existing `shared.sqlite` /
`collection.sqlite`, recreates them through `pkdump_db::open_shared` /
`connect_user` (so the schema and the shipped seeds are applied), then seeds
deterministic rows. User data is inserted through the `pkdump-db` repository
functions so app-layer validation runs.

**Both files are byte-stable.** Two regenerations of an unchanged seeder
produce two identical pairs, so `git status` after a regeneration is silent
unless something really moved — and a regeneration that does move something
is a diff worth reading rather than noise to scroll past.
`fixture::tests::two_seed_runs_are_byte_identical` is the gate, on the bytes
rather than on the columns anybody thought to list, because the failure mode
is a table nobody has added yet.

Every catalog row, price and observation date in `shared.sqlite` is a fixed
constant. `collection.sqlite` goes in through the repository functions, which
stamp their timestamp columns from the clock, so `seed-fixture` **pins the
clock** for the length of the build: a timeline starting at 2024-01-15T09:00Z
that advances one minute per read. Advancing rather than frozen, because
`/recent` and `/batches` `ORDER BY` those stamps — one constant would leave
their order a tie broken by rowid, which is stable pixels and a route that has
stopped demonstrating the ordering it exists to show. A minute per step,
because those two routes render `created_at.slice(0, 16)`: the five batches
are five distinct times on screen, not five copies of one.

The dates are chosen. 2024-01-15 is the fixture's own `OBSERVED_AT`, the day
its prices were quoted; it falls after the last order date the fixture invents
(2024-01-14) and after the acquisition constant on its manually-entered copies
(2024-01-10), so the collection reads as one that was assembled and then
priced. It also falls before the 2026-01-15 that `tests/visual/stabilize.ts`
freezes the browser at, which is asserted:
`fixture::tests::no_fixture_row_is_dated_after_the_frozen_browser_clock` reads
every text cell of every table — outbox payloads included — and fails on a
date in the suite's future. A build timestamp is always in the recent past,
and the recent past IS the future to a clock pinned at 2026-01-15.

Two things the pinned clock cannot reach, both handled at the end of the seed:

* **The ownership outbox** is written by the triggers in `schema_user.sql`,
  which read SQLite's clock rather than the process's. That is the property
  that makes the outbox correct — a trigger fires where no call site can
  forget it — so the events are dated afterwards from the holding each one
  describes: `payload` is the whole row as JSON, so the row's own
  `acquired_at` / `added_at` is in there even for a delete, and `seq` breaks
  the ties in milliseconds and stays the ordering authority.
* **A freed page keeps its old contents** until something reuses it, so two
  runs whose every row was identical still differed in the bytes of the
  pre-dating outbox rows left lying in the file. The seed ends with `VACUUM`.

So a regeneration moves a visual baseline only when the fixture's own DATA
moved. When it does, re-record with `bash tests/visual/run.sh --update` —
every viewport, never one (`tests/visual/README.md`) — and commit the PNGs on
the branch alongside the fixture, having read the diffs first.

Regenerate whenever `schema_shared.sql` gains a table, view or column. The
catalog is ATTACHed **read-only** at request time, so it is the one database in
this project that is never repaired on open — a stale one stays stale forever
and the first query naming the new object dies with `no such table`.
`PRAGMA user_version` will not warn you (additive change does not bump it);
`fixture::tests::the_committed_fixture_carries_every_catalog_object_the_schema_declares`
will, in `cargo test`.

## Files

| File | Contents |
| --- | --- |
| `shared.sqlite` | Immutable catalog: 4 sets, 37 cards, 57 printings, 57 prices, 4 sealed products, 2 sealed prices. |
| `collection.sqlite` | User data: 3 binders, 3 decks, 5 batches, 2 orders, 26 collection copies, 2 sealed entries, 3 wishlist entries, 1 saved view. |

## Catalog (`shared.sqlite`)

### Sets

| `set_code` | `ptcgo_code` | Name | Series | Release | Total / Printed |
| --- | --- | --- | --- | --- | --- |
| `base1` | `BS` | Base Set | Base | 1999/01/09 | 102 / 102 |
| `sv3pt5` | `MEW` | 151 | Scarlet & Violet | 2023/09/22 | 165 / 207 |
| `sv8` | `SSP` | Surging Sparks | Scarlet & Violet | 2024/11/08 | 191 / 252 |
| `jp-23723` | — | Mystery of the Fossils | Pokémon JP — Original Era | 1997/06/21 | 6 / — |

Sets list newest-first (`release_date DESC`), so the Japanese set is last on
/browse — and, being the only one with no owned cards, the only series whose
accordion section starts collapsed.

### Cards (37)

Card ids are `<set_code>-<number>`. Printing ids are
`<card_id>-<variant>` (e.g. `sv3pt5-6-holo`). Every linked printing has a
single `market` price row observed `2024-01-15`.

**Base Set (`base1`)** — 9 cards:
Charizard (#4, Rare Holo), Blastoise (#2, Rare Holo), Venusaur (#15,
Rare Holo), Hitmonchan (#7, Rare Holo), Raichu (#24, Rare),
Pikachu (#58, Common), Bulbasaur (#46, Common),
Energy Removal (#88, Common, Trainer), Fire Energy (#98, Common, Energy).
Holo rares carry one `holo` printing; commons carry `normal` +
`reverse_holo`.

**151 (`sv3pt5`)** — 13 cards, including a secret rare above the printed
total of 165:
Bulbasaur (#1, Common), Charmander (#4, Common), Charizard ex (#6,
Double Rare), Squirtle (#7, Common), Pikachu (#25, Common), Alakazam ex
(#65, Double Rare), Ditto (#105, Uncommon), Mew (#131, Rare),
Snorlax (#151, Uncommon), Professor's Research (#165, Illustration Rare,
Trainer), Bulbasaur (#166, Illustration Rare), Charizard ex (#199,
Special Illustration Rare), **Mew ex (#201, Hyper Rare — secret rare)**.

**Surging Sparks (`sv8`)** — 9 cards, including a secret rare above the
printed total of 191:
Exeggcute (#3, Common), Magmar (#32, Common), Pikachu ex (#57, Double
Rare), Milotic ex (#89, Double Rare), Boss's Orders (#120, Uncommon,
Trainer), Latias ex (#160, Illustration Rare), Alolan Exeggutor ex (#191,
Illustration Rare), Pikachu ex (#238, Special Illustration Rare),
**Iono (#252, Special Illustration Rare — secret rare, Trainer)**.

**Mystery of the Fossils (`jp-23723`)** — 6 cards. The Japanese catalog is
TCGCSV-native and its rows are shaped differently from anything
pokemontcg.io publishes, so this set is the fixture's only cover for a
handful of paths (pd-zonm):

| Card | Number | Rarity | Printings |
| --- | --- | --- | --- |
| Aerodactyl | `p575661` | Rare Holo | 1st Edition Holofoil, Unlimited Holofoil |
| Articuno | `p575662` | Rare Holo | 1st Edition Holofoil, Unlimited Holofoil |
| Ekans | `p575663` | Common | 1st Edition Normal, Unlimited Normal |
| Energy Search | `p575664` | Common (Trainer) | 1st Edition Normal, Unlimited Normal |
| Omanyte | `p575665` | Common | 1st Edition Normal, Unlimited Normal |
| Lapras | `p575666` | Rare | 1st Edition Normal, Unlimited Normal |

- **`printed_total` is NULL.** TCGCSV publishes no `Number` for this group,
  so there is no denominator to read one off. The /browse tile drops its
  Base meter; the binder page files the whole set as one `base` section, so
  both of its meters carry the same figure.
- **Every collector number is the synthetic `p<product_id>` form** —
  `japan::collector_number`'s fallback for a product with no printed number.
- **`ptcgio_covered = 0` beside a NULL `ptcgio_fetched_at`**, so the tile
  must NOT carry the "TCGCSV — provisional" badge (pd-mt57). Every English
  set here is covered, so nothing else could catch that badge leaking back.
- **No `ptcgo_code` and no `symbol_url`**, so the tile stamps `JP-23723`.
- **One product id per card**, its print runs told apart by `sub_type_name`.
  The English sets give every variant its own product id, so
  `first_ed_*` / `unlimited_*` reach the variant display layer only here.
- **No `artist`, no `national_pokedex_numbers`** — `synthesize_cards` writes
  neither — and Lapras has no image at all (TCGCSV's `imageCount` is 0),
  which is the binder's `.noart` slot.

`fixture::tests::the_committed_fixture_carries_a_japanese_set_in_the_japanese_shape`
holds all of that; `tests/ui/intents/browse_japanese_set_*.yaml` walk it.

### Sealed products

| `product_id` | Name | Category | Set | Quoted |
| --- | --- | --- | --- | --- |
| 900001 | Base Set Booster Box | `booster_box` | `base1` | — |
| 900002 | 151 Elite Trainer Box | `etb` | `sv3pt5` | market $59.42 |
| 900003 | 151 Booster Bundle | `bundle` | `sv3pt5` | — |
| 900004 | Surging Sparks Booster Pack | `booster_pack` | `sv8` | mid $5.24, no market |

The two the collection HOLDS are quoted one way each, so a screenshot goes
through both halves of `sealed_market_price_expr_from!` —
`COALESCE(market_price, mid_price)` — rather than one (sp-ysb). 900001 and
900003 stay unquoted: neither is held, so neither moves a number, and a sealed
product TCGCSV prices nowhere is a shape the catalog really carries.

`fixture::tests::the_committed_fixture_values_its_sealed_holdings` holds it.
Before it, `sealed_prices` was EMPTY, so both lots fell to pd-bbv7's unquoted
arm — skipped by the sum, counted in the units, correctly — and that was the
only arm any baseline could take: `/` read `sealed $0.00`, `/sealed` showed an
em dash wherever a market value goes, and the `dimension='sealed'` series had
no non-zero point on it.

## User data (`collection.sqlite`)

### Binders (3)

| Name | Pocket size | Type | Location |
| --- | --- | --- | --- |
| Trade Binder | 9 | trade | Shelf A |
| Master Set: 151 | 12 | set | Shelf A |
| Vintage Vault | 9 | showcase | Safe |

### Decks (3) — one per lifecycle state

| Name | State | Owner | Format |
| --- | --- | --- | --- |
| Charizard ex Control | `built` | Ryan | standard |
| Pikachu ex Aggro | `ready` | Ryan | standard |
| Vintage Base Brawl | `idea` | Alice | casual |

### Batches (5)

| `batch_type` | Name |
| --- | --- |
| `order_tcgplayer` | TCG-100001 *(auto-created by the received order)* |
| `order_ebay` | EBAY-55012 *(auto-created by the open order)* |
| `manual_id` | Vintage holo entry |
| `binder_click` | 151 binder page-through |
| `csv_manabox` | ManaBox export 2024-01 |

### Orders (2)

| `order_number` | Source | Status | Notes |
| --- | --- | --- | --- |
| TCG-100001 | tcgplayer | received → copies `owned` | 3 copies (1× Charizard ex holo, 2× Charmander) |
| EBAY-55012 | ebay | open → copy still `ordered` | 1× Base Set Charizard holo, in transit |

### Collection (26 copies)

By status: 23 `owned`, 1 `ordered` (the open eBay order), 1 `listed`
(`sv8-160-holo`, Latias ex — listed for trade), 1 `sold`
(`sv3pt5-65-holo`, Alakazam ex).

Notable copies:
- `base1-24-holo` (Raichu) — graded **PSA 8**, cert `12345678`.
- 5 vintage holos in **Vintage Vault**, varied condition (NM through
  Moderately Played).
- 8 `sv3pt5` copies registered to **Master Set: 151** via `binder_click`,
  including the Mew ex hyper rare.
- 4 `sv8` copies in **Trade Binder** from the ManaBox CSV import.
- 3 copies built into the **Charizard ex Control** deck.
- 2 loose, unassigned owned copies.

### Sealed collection (2)

| Product | Qty | Paid (each) | Market (each) | Source |
| --- | --- | --- | --- | --- |
| 151 Elite Trainer Box (900002) | 1 | $49.99 | $59.42 (market) | pokemoncenter |
| Surging Sparks Booster Pack (900004) | 6 | $4.49 | $5.24 (mid) | lgs |

7 units, $76.93 paid, $90.86 market — the `dimension='sealed'` point on the
value chart and the `sealed $90.86` half of the home page's headline. `/`
renders the two halves separately and sums them at read time; there is no
stored combined total (pd-bbv7).

### Wishlist (3)

| Card | Priority | Max price |
| --- | --- | --- |
| Charizard ex SIR (`sv3pt5-199`) | 3 | $250 |
| Pikachu ex SIR (`sv8-238`) | 2 | $120 |
| Base Set Charizard (`base1-4`) | 1 | $300 |

### Saved collection views (1)

- **Vintage Holos** — filters `{"set":"base1","rarity":"Rare Holo"}`.

## Deliberately unowned

Nothing in `collection.sqlite` references a `jp-23723` printing. That is the
point: the Japanese set is what the "add my first copy of this" walk starts
from, and a series with no owned cards is the only way /browse's default
collapse rule gets exercised at all.
