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

`shared.sqlite` is byte-stable: every catalog row, price and observation date
in it is a fixed constant, so two regenerations produce identical files.
`collection.sqlite` is not, and cannot be while the rows go in through those
repository functions — they stamp `created_at` / `acquired_at` from the clock.
Two regenerations differ in exactly those columns and nothing else. Anything
that must be stable across a regeneration is pinned some other way: the visual
suite freezes the browser's clock (`tests/visual/stabilize.ts`) and pins the
ids it visits (`tests/visual/routes.json`).

So **a regeneration moves eight visual baselines** — `sealed`, `recent`,
`batches` and `batch-detail`, at both viewports — in the date digits and
nowhere else. Re-record them with `bash tests/visual/run.sh --update` and
commit the PNGs alongside the fixture, having read the diffs first. pd-nzlj is
the work that removes this: give the fixture a narrative of pinned dates so
those routes stop rendering the moment the file was built.

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
| `shared.sqlite` | Immutable catalog: 3 sets, 31 cards, 45 printings, 45 prices, 4 sealed products. |
| `collection.sqlite` | User data: 3 binders, 3 decks, 5 batches, 2 orders, 26 collection copies, 2 sealed entries, 3 wishlist entries, 1 saved view. |

## Catalog (`shared.sqlite`)

### Sets

| `set_code` | `ptcgo_code` | Name | Series | Release | Total / Printed |
| --- | --- | --- | --- | --- | --- |
| `base1` | `BS` | Base Set | Base | 1999/01/09 | 102 / 102 |
| `sv3pt5` | `MEW` | 151 | Scarlet & Violet | 2023/09/22 | 165 / 207 |
| `sv8` | `SSP` | Surging Sparks | Scarlet & Violet | 2024/11/08 | 191 / 252 |

### Cards (31)

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

### Sealed products

| `product_id` | Name | Category | Set |
| --- | --- | --- | --- |
| 900001 | Base Set Booster Box | `booster_box` | `base1` |
| 900002 | 151 Elite Trainer Box | `etb` | `sv3pt5` |
| 900003 | 151 Booster Bundle | `bundle` | `sv3pt5` |
| 900004 | Surging Sparks Booster Pack | `booster_pack` | `sv8` |

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

| Product | Qty | Source |
| --- | --- | --- |
| 151 Elite Trainer Box (900002) | 1 | pokemoncenter |
| Surging Sparks Booster Pack (900004) | 6 | lgs |

### Wishlist (3)

| Card | Priority | Max price |
| --- | --- | --- |
| Charizard ex SIR (`sv3pt5-199`) | 3 | $250 |
| Pikachu ex SIR (`sv8-238`) | 2 | $120 |
| Base Set Charizard (`base1-4`) | 1 | $300 |

### Saved collection views (1)

- **Vintage Holos** — filters `{"set":"base1","rarity":"Rare Holo"}`.
