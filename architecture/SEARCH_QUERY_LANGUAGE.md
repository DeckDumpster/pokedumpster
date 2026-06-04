# Search Query Language

The design record for the Scryfall-style collection search query language
(beads epic `pokedumpster-idf`). Ported in spirit from DeckDumpster's MTG
search (vendored at `_deckdumpster-src/mtg_collector/search/`), re-homed to
this Rust/SQLite/Svelte workspace and adapted for Pokémon card data.

Status: design ratified (decisions D1–D4 closed). Implementation tracked by
`pokedumpster-idf.5`–`.12`.

---

## 1. Goal

A single query bar on the collection page that accepts a composable,
Scryfall-like language: boolean conditions over every card and collection
attribute, attribute-specific keywords (`s:pfl`, `t:fire`, `hp>=200`,
`rarity:"illustration rare"`), price/date/quantity filters, and — the
headline — clean operation over **owned and unowned cards alike** via
`is:missing`.

Example queries the language must support:

```
charizard is:holo                      # holo Charizards I own
s:pfl t:fire rarity>=rare              # Phantasmal Flames fire cards, rare+
t:water hp>=200 -is:graded            # big Water cards, ungraded copies
is:missing s:sv3pt5 sub:ex            # 151 'ex' cards I'm still missing
price>20 added>=2026-01-01 order:price direction:desc
pikachu qty>=2                         # printings I own 2+ copies of
```

---

## 2. Architecture

The same four-stage pipeline as DeckDumpster, but split along this
workspace's crate boundaries. The guiding constraint: **`pkdump-core` does
no IO**, so the parser is pure and the keyword registry is injected.

```
query string
   │  pkdump-core/src/query/lexer.rs    ordered-regex tokenizer
   ▼
tokens
   │  pkdump-core/src/query/parser.rs   recursive descent (needs &KeywordRegistry)
   ▼
AST  (pkdump-core/src/query/ast.rs)
   │  pkdump-db/src/search.rs           compile(): AST + registry → SQL
   ▼
CompiledQuery { where_sql, params, flags, order_by, mode }
   │  pkdump-db/src/search.rs           execute(): pick template, run
   ▼
Vec<SearchRow>
   │  pkdump-server/src/routes/search.rs
   ▼
JSON → frontend/src/routes/collection/+page.svelte
```

| Stage | Home | Ported from |
|---|---|---|
| Lexer | `pkdump-core/src/query/lexer.rs` | `grammar.py` |
| AST | `pkdump-core/src/query/ast.rs` | `ast_nodes.py` |
| Parser | `pkdump-core/src/query/parser.rs` | `transformer.py` |
| Registry | `pkdump-core/src/query/registry.rs` | `keywords.py` (now data) |
| Compiler | `pkdump-db/src/search.rs` | `compiler.py` |
| Route | `pkdump-server/src/routes/search.rs` | `crack_pack_server._api_collection` |
| UI | `frontend/.../collection/+page.svelte` | `static/collection.html` |

### Why hand-rolled, not a parser generator

The grammar is context-sensitive in ways a clean CFG fights (DeckDumpster's
`grammar.py` docstring documents this and it applies unchanged):

- keyword detection depends on adjacency to the operator (`c:r` is a keyword,
  `c : r` is not),
- `or`/`and` are keywords *between* clauses but bare words elsewhere,
- `-` is negation as a prefix but a literal hyphen inside names
  (`-is:holo` vs `ho-oh`).

An ordered-regex tokenizer plus a small recursive-descent parser handles all
three directly and keeps dependencies out of `pkdump-core`.

---

## 3. Grammar

Precedence, lowest to highest: **OR < AND (implicit) < NOT < atom**.

```
query   := or_expr
or_expr := and_expr ( "or" and_expr )*
and_expr:= atom ( ["and"] atom )*           # adjacency = AND; "and" is a no-op connector
atom    := "-" atom                          # negation
         | "(" or_expr ")"                    # group
         | "!" STRING                         # exact name
         | KEYWORD OP VALUE                    # comparison
         | WORD                                # bare-word name search
OP      := ":" | "=" | "!=" | "<" | ">" | "<=" | ">="
VALUE   := QUOTED | UNQUOTED                   # quotes preserve spaces
```

### AST nodes (`ast.rs`)

```rust
enum Ast {
    And(Vec<Ast>),
    Or(Vec<Ast>),
    Not(Box<Ast>),
    Comparison { keyword: String, op: Op, value: String },  // keyword = canonical
    NameSearch(String),                                       // bare word(s)
    ExactName(String),                                        // !"Charizard ex"
}
```

### Errors

`QueryError { message: String, position: usize }` — the byte offset is
carried from the lexer through the parser so the frontend can place a caret
under the offending token. Unknown keywords are a **parse-time** error
(alias resolution happens in the parser against the registry), so "is this a
real field?" fails fast with a position.

Per project policy: **no fallback logic**. A malformed query is a hard error
surfaced to the user, never a silent empty result or a best-effort guess.

---

## 4. The keyword registry is data (decision D1)

DeckDumpster's `keywords.py` holds the alias map, operator sets, and value
enums as in-code dicts — exactly the "data living in code" smell this project
flags (memory `data-model-is-the-product-...`). Here the registry is **data**.

### Source of truth

- `data/search_keywords.json` → `shared.sqlite` table `search_keywords`,
  seeded at `pkdump setup` the same way `data/variants.json` seeds `variants`.
- `data/rarities.json` → `shared.sqlite` table `rarities` (decision D2).

```sql
CREATE TABLE IF NOT EXISTS search_keywords (
    canonical   TEXT PRIMARY KEY,   -- 'energy_type'
    aliases     TEXT NOT NULL,      -- JSON array: ["t","type"]
    operators   TEXT NOT NULL,      -- JSON array: [":","=","!="]
    kind        TEXT NOT NULL,      -- 'text'|'numeric'|'enum'|'date'|'json_contains'|'flag'|'modifier'|'collection'
    target      TEXT,               -- column or JSON path, e.g. 'cards.types'
    value_enum  TEXT,               -- optional ref to an enum set (energy types, conditions, …)
    semantics   TEXT,               -- 'superset'|'subset'|'exists'|… free-form tag the compiler reads
    help        TEXT                -- one-line description for the help page + autocomplete
);

CREATE TABLE IF NOT EXISTS rarities (
    name   TEXT PRIMARY KEY,        -- 'Illustration Rare'
    rank   INTEGER NOT NULL,        -- curated ordinal for r>= / r<
    grp    TEXT                     -- group alias: 'secret','ultra','common',…
);
```

### Injection (keeps the parser pure)

`pkdump-core` defines a `KeywordRegistry` type and *consumes* it; it never
loads it. `pkdump-db` provides `load_registry(conn) -> KeywordRegistry`
(reads the tables) and a `KeywordRegistry::from_json(&str)` path for unit
tests that have no DB. The server builds the registry once at startup and
holds it in `AppState`.

### Payoff

The autocomplete dropdown and the `/search-help` page render **from the same
table** — there is no hardcoded keyword list anywhere (DeckDumpster's
frontend has one; we don't). Adding a keyword is a seed-file edit plus a
compiler arm.

---

## 5. Pokémon keyword map

The canonical set for v1. All string matches are `COLLATE NOCASE`. JSON
columns (`types`, `subtypes`, `attacks`, …) are matched with `LIKE` over the
serialized array, mirroring the existing binder/collection queries.

### Card metadata (shared catalog)

| Keyword (aliases) | Target | Ops | Notes |
|---|---|---|---|
| *(bare words)* | `cards.name` | substring | `LIKE %term%` |
| `!"…"` | card identity by exact name | `=` | every printing of every card with that exact name |
| `s:` `set:` `e:` | `sets.ptcgo_code` / `set_code` / `name` | `:` `=` `!=` | **`s:pfl`** → Phantasmal Flames |
| `r:` `rarity:` | `cards.rarity` | `:` `=` `!=` `<` `>` `<=` `>=` | ordinals via `rarities.rank` (D2); `:`/`=` exact/substring |
| `t:` `type:` | `cards.types` (energy) | `:` `=` `!=` | the "color" analog: Fire/Water/Grass/Lightning/Psychic/Fighting/Darkness/Metal/Fairy/Dragon/Colorless |
| `super:` `supertype:` | `cards.supertype` | `:` `=` | Pokémon / Trainer / Energy |
| `sub:` `subtype:` | `cards.subtypes` | `:` `=` `!=` | Basic, Stage 1/2, ex, V, VMAX, VSTAR, GX, Supporter, Item, Stadium… |
| `hp:` | `cards.hp` | numeric | |
| `a:` `artist:` | `cards.artist` | substring | |
| `ft:` `flavor:` | `cards.flavor_text` | substring | |
| `o:` `text:` | `abilities` + `attacks` text | substring | the "rules text" analog |
| `ability:` | `cards.abilities` (name+text) | substring | |
| `atk:` `attack:` | `cards.attacks` (name+text) | substring | |
| `dmg:` `damage:` | max attack damage | numeric | parsed from `attacks[].damage` |
| `retreat:` `rc:` | `len(retreat_cost)` | numeric | |
| `weak:` | `cards.weaknesses` type | `:` | |
| `resist:` | `cards.resistances` type | `:` | |
| `dex:` `pokedex:` | `national_pokedex_numbers` | numeric + range | any-element match |
| `reg:` `regulation:` | `regulation_mark` | `:` `=` | F/G/H |
| `year:` | `sets.release_date` (year) | numeric | |
| `legal:` `f:` `format:` / `banned:` | `cards.legalities` JSON | `:` | standard/expanded/unlimited |
| `variant:` `v:` | `printings.variant` → `variants` code/label/short | substring | data-driven from the `variants` table |

### `is:` flags

Defined in the seed (so they are data, not a `match` arm). Two families:

- **Variant-derived**: `is:holo`, `is:reverse`, `is:firstedition`,
  `is:pokeball`, `is:masterball`, `is:promo` — each expands to a set of
  `variants.code` values (the mapping lives in the seed).
- **Computed**: `is:graded`, `is:dupe` (owned_count ≥ 2), and the ownership
  trio **`is:missing` / `is:unowned` / `is:owned`** (see §7).

### Collection (user DB)

| Keyword (aliases) | Target | Ops | Notes |
|---|---|---|---|
| `status:` | `collection.status` | `:` `=` `!=` | default `owned`+`ordered` unless explicit (see §7) |
| `condition:` `cond:` | `collection.condition` | `:` `=` | aliases nm/lp/mp/hp/dmg |
| `grade:` | `collection.grade_value` | numeric | |
| `grader:` | `collection.grade_company` | `:` `=` | psa/bgs/cgc/sgc/… |
| `price:` | market price (the COALESCE expr) | numeric | |
| `paid:` | `collection.purchase_price` | numeric | |
| `sold:` | `collection.sale_price` | numeric | |
| `added:` | `collection.acquired_at` | date compare | |
| `deck:` | deck name / `*` / negation | `:` `=` `!=` | lazy LEFT JOIN |
| `binder:` | binder name / `*` / negation | `:` `=` `!=` | lazy LEFT JOIN |
| `lang:` `language:` | `collection.language` | `:` `=` | |
| `source:` | `collection.source` | `:` `=` | |
| `tag:` | `collection.tags` (JSON) | substring | array contains |
| `note:` | `collection.notes` | substring | |
| `qty:` `count:` | copies of the printing | numeric | per-printing (D4) |

### Modifiers (extracted, not compiled to WHERE)

`order:` (name, number, set, rarity, hp, price, added, dex, qty) and
`direction:`/`dir:` (asc/desc) are pulled out of the AST into
`CompiledQuery.order_by`/`order_dir`, exactly as DeckDumpster's
`_extract_modifiers` does.

---

## 6. Rarity ordering (decision D2)

Pokémon rarity is dozens of unordered, era-dependent strings with no natural
total order, so `r>=rare` is meaningless out of the box. The `rarities` table
supplies a **curated `rank`** (enabling `<`/`>`/`<=`/`>=` via a join) and a
**group alias** so `rarity:secret` or `rarity:ultra` matches a family. Until a
rank exists for a value, only `:`/`=` (exact/substring) apply to it. The rank
ordering and group taxonomy are authored in `data/rarities.json` as part of
`pokedumpster-idf.6`.

---

## 7. The unified per-printing result model (decision D3)

This is the most consequential design choice and the biggest departure from
today's collection page.

### The problem

Today the collection page renders `CollectionRow` — each row is a **physical
copy you own** (`collection.id` non-optional, with per-copy condition /
status / binder). An **unowned** card surfaced by `is:missing` has no
`collection` row at all, so `CollectionRow` cannot represent it.

### The decision

**One row per printing, in every mode.** Every printing in the catalog is a
candidate row; `owned_count` says how many copies you have (0 = missing,
rendered dimmed like deprecated variants). Per-copy detail moves into an
expand/drill-down rather than being top-level rows. This unifies owned and
catalog-wide views under a single, consistent row unit and matches the binder
"slot" mental model.

```rust
#[derive(Serialize, ts_rs::TS)]
struct SearchRow {
    // identity / display — always present
    printing_id, card_id, set_code, set_name, set_ptcgo_code, set_symbol_url,
    number, name, rarity, artist, supertype, subtypes, types, attacks,
    market_price, image_small, variant, variant_description,
    // ownership
    owned: bool,
    owned_count: i64,            // 0 = missing
    // per-copy detail for the expand (empty when unowned)
    copies: Vec<CopySummary>,    // id, condition, language, status, graded,
                                 // purchase_price, acquired_at, binder_id, deck_id
}
```

`SearchRow` is a **new type**, not a retrofit of `CollectionRow` — the latter
stays exactly as-is for `list_by_binder` / `list_by_deck` / `list_by_order` /
`get_row`, where the per-copy fields are genuinely always present. Making them
`Option` there would scatter false optionality across the app.

### Two SQL templates (both per-printing)

The compiler chooses based on whether an ownership flag
(`is:missing`/`is:unowned`/`is:owned`) appears:

```sql
-- OWNED MODE (default): only printings with ≥1 owned copy
FROM (printings ⋃ user_printings) p
  JOIN cards cd ON p.card_id = cd.card_id
  JOIN sets s   ON cd.set_code = s.set_code
WHERE EXISTS (SELECT 1 FROM collection c WHERE c.printing_id = p.printing_id
              AND <status default owned+ordered>)
  AND (<compiled WHERE>)

-- CATALOG-WIDE MODE (is:missing etc.): every printing, ownership via LEFT JOIN
FROM (printings ⋃ user_printings) p
  JOIN cards cd ... JOIN sets s ...
  LEFT JOIN (SELECT printing_id, COUNT(*) n FROM collection GROUP BY 1) oc
         ON oc.printing_id = p.printing_id
WHERE (<compiled WHERE>)        -- is:missing → owned_count = 0; is:owned → > 0
```

`owned_count` is a correlated/aggregated count; `owned = owned_count > 0`.
`copies` is attached via a grouped second query keyed on the page's
printing_ids (preferred, avoids N+1); lazy fetch-on-expand is the fallback.

### Collection-predicate semantics

In a per-printing model, collection-row predicates
(`status`/`condition`/`binder`/`deck`/`added`/`price`/`paid`/`sold`/
`source`/`tag`/`note`/`grade`/`grader`) compile to **EXISTS over the
printing's copies** — the printing matches if at least one copy satisfies.
`owned_count` reflects **total** copies of the printing, not only matching
ones. (Deferred nuance: "count only matching copies" — revisit if it proves
necessary in real use.)

### Quantity (decision D4)

`qty:` / `count:` and `is:dupe` count copies of the **same printing**
(`card_id` + `variant` + `language`). Worked example: 2× Base Set Pikachu
(one printing) + 1× Base Set 2 Pikachu (a different printing) satisfies
`pikachu qty:2` only for the Base Set printing. Implemented as the
`owned_count` aggregate compared to the operand; `is:dupe` ⇔ `owned_count ≥ 2`.

---

## 8. Server + frontend integration (decision: augment the collection page)

- `GET /api/collection/search?q=&sort=&dir=` → `Vec<SearchRow>`. The handler
  parses (`SearchError` → HTTP 400 with `position`), compiles, picks the
  template, executes.
- `GET /api/search/keywords` → the registry, JSON, for data-driven
  autocomplete and the help page.
- The collection page (`frontend/src/routes/collection/+page.svelte`) moves
  from "load the whole collection, filter client-side"
  (`collection::list_rows`, today's documented pattern) to the server query:
  debounced input, position-aware error display, an `is:missing` toggle, and
  **printing-centric rows that expand to per-copy detail**. Unowned printings
  render dimmed. Filters round-trip through the URL (`?q=&sort=&dir=`).
- `/search-help` renders from the registry.

`ts-rs` exports `SearchRow` / `CopySummary` into
`frontend/src/lib/types/` via `cargo test`, as with every other type.

---

## 9. Test strategy — four tiers

Layered cheap→expensive, deterministic→probabilistic (DeckDumpster's exact
shape, re-implemented in Rust).

1. **Curated corpus** (`crates/pkdump-db/tests/fixtures/search_corpus.json`,
   ~150–200 Pokémon-flavored entries `{query, should_parse,
   compiler_supported, category}`). Parametrized: should-parse parses;
   should-error raises `QueryError`; supported compiles **without the `1=0`
   sentinel** and executes against the seed-fixture DB.

2. **Unit tests**. Parser tests assert AST *shape* (`pkdump-core`). Compiler
   tests assert SQL substrings + bound params and execute representative
   queries against an in-memory schema (`pkdump-db`).

3. **Generative / probabilistic** (always-on, offline). A Rust
   `QueryGenerator` **inverts the grammar productions** with a seeded
   `StdRng` and per-keyword operator/value pools, so every output parses by
   construction. Three invariants over N seeds: parses, compiles without
   `1=0`, executes without a SQL error (against the seed fixture).
   `PKDUMP_SEEDS` / `PKDUMP_SEED` mirror DeckDumpster's `--seeds`/`--seed`
   for volume and single-seed reproduction.

4. **pokémontcg.io differential oracle** (opt-in, cached). A
   `grammar → Lucene` translator for the **card-metadata subset**
   (collection-only keywords are skipped, exactly as DeckDumpster skips its
   extensions against Scryfall). A rate-limited client with a SQLite response
   cache keyed by query hash; a comparator that dedupes both sides to card
   identity, intersects the fixture's known universe, and classifies
   disagreements (real-bug / coverage-gap / truncation) into a JSON report.
   Report mode, `#[ignore]` by default. Plus a parse-agreement check: if
   pokémontcg.io accepts a translatable query, we must not reject it.

### The `1=0` sentinel

A keyword that parses but isn't wired in the compiler emits
`1=0 /* unsupported */`. Tiers 1 and 3 both assert `"1=0"` never appears in
the compiled SQL of anything claimed supported — so "added a keyword to the
parser, forgot the compiler arm" is a mechanical test failure.

### The core insight worth keeping

One reverse-parser yields unlimited valid fuzz inputs; three trivial
invariants catch the overwhelming majority of regressions; the `1=0` sentinel
catches the rest. The differential oracle is the high-fidelity backstop for
*semantic* correctness, run on demand.

Per project rule: a test that demonstrates a bug must **fail** until the bug
is fixed, and ship in the same commit as the fix.

---

## 10. Implementation order (beads `pokedumpster-idf`)

```
idf.5  Core parser (pkdump-core/src/query/)        — no DB; cleanest port
idf.6  Keyword registry data + tables + loader
idf.7  SQL compiler (pkdump-db/src/search.rs)      — needs 5, 6
idf.8  Corpus + unit tests (tiers 1–2)             — needs 5, 7
idf.9  Generative fuzzer (tier 3)                  — needs 5, 7
idf.10 Route + frontend (printing-centric page)    — needs 7
idf.11 pokémontcg.io oracle (tier 4)               — needs 7, 8
idf.12 QA finish (/qa-finish) + close-out
```

Decisions D1–D4 are closed; this document is their consolidated record.
