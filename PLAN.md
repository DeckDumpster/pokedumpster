# PokeDumpster — Design & Implementation Plan

**Read first:** `RESEARCH.md`. This plan assumes the DeckDumpster architecture, the
upstream data sources, the pkmn.gg gaps, and the resolved binder-view UX from
RESEARCH are accepted as the foundation.

This is the revision after the interactive review. All previously-debated
decisions are now baked in; the `Decisions log` (§13) records what was
considered and why.

---

## 0. One-paragraph elevator

PokeDumpster is a single-user (forward-compatible for family multi-tenancy)
Pokémon TCG collection + sealed-collection tracker. It takes DeckDumpster's
feature shape — collection / sealed / orders / wishlist / binders / decks /
intents-tested UI — and rebuilds it on a modern Rust + SvelteKit stack
against pokemontcg.io and TCGCSV instead of Scryfall and MTGJSON. The
headline new feature is **binder-page browsing as the primary ingestion
surface**: a virtual 3×3 grid where each slot is one card number, owned
variants surface as filled pips plus inline checkboxes for the common Reg/RH
pair, and a click opens a pkmn.gg-style variant modal. Pokémon-specific
weirdness (Master Ball patterns, Pokémon Center stamps, Galarian Gallery
subsets) is handled by a three-layer variant-expansion pipeline: live API
inference → JSON overlay → rarity-based bootstrap.

---

## 1. Goals & non-goals

### In scope (v1)

- Local SQLite store split into shared (immutable catalog) + per-user (mutable collection) databases, ATTACHed at runtime — forward path to multi-tenancy without an auth layer.
- Collection management with per-copy condition, language, purchase price, status, source, notes (strict one-row-per-physical-card).
- **Binder-page browsing** — one slot per card number, modal variant picker, inline checkbox shortcuts, master-set completion stats.
- Sealed collection (booster boxes, ETBs, bundles, tins, premium collections).
- Decks (minimal: named, optionally format-tagged, deck/binder exclusivity enforced — no zones, no commander, no planning-mode).
- TCGplayer USD + Cardmarket EUR pricing, daily snapshots, price history graphs.
- Manual ID ingest (`set + collector_number + variant`).
- CSV import: **ManaBox primary, TCGplayer mass-entry CSV runner-up**.
- CSV export: ManaBox round-trippable.
- Order tracking (TCGplayer, eBay, Pokémon Center) with batch-receive.
- Wishlist (card-level or variant-specific, max-price).
- Multi-select bulk ops, saved views, card detail page with price chart.
- Recent batches view, `status_log` / `movement_log` audit trails.
- **Intents UI testing framework** ported wholesale into TypeScript + Playwright; all 175 relevant DeckDumpster intents catalogued in §15 carried forward.
- Rootless Podman Quadlet deploy; localhost + WireGuard for any remote access.

### Explicitly out of scope (v1)

- Image ingestion (corner OCR, full-card scan, batch-photo).
- Crack-a-pack / virtual booster simulator.
- Explore-sheets (MTG-specific Print Sheet concept).
- Jumpstart (MTG-specific).
- Commander format, mainboard/sideboard zones, deck planning-mode.
- App-level authentication. WireGuard handles access control at the network layer.
- Trading marketplace / cross-user matching.
- Native mobile app. Web-first; responsive at 480px/768px/1200px breakpoints.
- Japanese / OCG support. English-only v1; data model has `language` columns so TCGdex can be added later.
- pkmn.gg userscript migration. Deferred to post-v1 "gravy" once the test dataset has proven the bones.

### Held in reserve / explicit upgrade slots

| Slot | Trigger | Source |
|---|---|---|
| Scrydex paid | Need PSA-graded prices, true price history beyond what we self-snapshot | Scrydex API |
| TCGdex | Need Japanese / OCG cards | TCGdex GraphQL + GitHub repo |
| Pull-rate sealed open | Want "I opened this box, here's what I pulled" with bulk-add | Hand-curated `sealed_product_contents` |
| pkmn.gg userscript | Want to migrate existing pkmn.gg collection | Tampermonkey + JSON scrape |
| Real multi-tenant | A second persistent user wants their own collection visible separately | Add a thin user-selection wrapper above the per-user-DB picker |
| Phone-native UX | Daughter wants a dedicated mobile experience | Wrap web UI in a PWA or build a Tauri sidecar |

---

## 2. Stack

| Concern | Choice | Notes |
|---|---|---|
| Backend language | **Rust** | Same aesthetic as `pokedex`; static binary deploy |
| HTTP framework | **Axum** | Tokio-backed, Tower middleware, ergonomic extractors |
| SQL access | **rusqlite** | Matches `pokedex`; full control over ATTACH + pragmas. No compile-time query checking — covered by integration tests instead |
| Migrations | **refinery** | Versioned raw SQL files; same shape as pokedex's manual migration discipline |
| Serialization | **serde** + **serde_json** | |
| TS type generation | **ts-rs** | `#[derive(TS)]` on every API-exposed struct → `frontend/src/lib/types/api.ts` auto-regenerated at build |
| CLI parsing | **clap** | `pkdump` command tree, mirrors pokedex |
| HTTP client | **reqwest** | For pokemontcg.io / TCGCSV cache population only |
| Error types | **thiserror** + **anyhow** | thiserror in lib code, anyhow at handler boundaries |
| Logging | **tracing** + **tracing-subscriber** | Structured JSON logs in prod, pretty in dev |
| Frontend framework | **SvelteKit** | `@sveltejs/adapter-static` → Axum serves the `dist/` |
| Frontend types | **TypeScript** | strict mode; types from `lib/types/api.ts` (generated) |
| Frontend data layer | **TanStack Query for Svelte** | Cache, refetch, optimistic updates for binder writes |
| Frontend components | Hand-rolled + **shadcn-svelte** for primitives (modal, dropdown, toast) | No heavyweight design system |
| Frontend testing | **Playwright** + custom intents harness | Replaces DeckDumpster's Python harness |
| Backend testing | **cargo test** | Fresh temp DB per test via a test helper; integration tests cover query correctness |
| Container | **Podman** + **Quadlet** | Multi-stage Cargo build → distroless runtime |
| Runtime image size | **~20–40 MB** target | Static binary + ~5 MB SvelteKit `dist/` |
| Process model | Single binary, single port, single process | No nginx, no node sidecar |

### 2.1 Repo layout

```
pokedumpster/
├── README.md
├── CLAUDE.md
├── architecture/
│   └── CARD_DATA_ACCESS.md
├── plans/                          # one-shot design docs
├── data/
│   ├── known_issues.md
│   └── overrides/
│       ├── variant_augmentations.json
│       ├── subset_mappings.json
│       ├── promo_tie_ins.json
│       └── set_aliases.json
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── crates/
│   ├── pkdump-cli/                 # `pkdump` binary
│   │   └── src/main.rs             # clap dispatch only
│   ├── pkdump-core/                # domain logic, no IO
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types/              # API-exposed structs with #[derive(TS)]
│   │       ├── variant/            # three-layer expansion
│   │       └── search/             # query language compiler
│   ├── pkdump-db/                  # rusqlite + refinery
│   │   ├── migrations/
│   │   │   ├── shared/             # shared.sqlite migrations
│   │   │   └── user/               # per-user.sqlite migrations
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── shared.rs           # repositories that read shared catalog
│   │       └── user.rs             # repositories that read/write user data
│   ├── pkdump-ingest/              # pokemontcg.io + TCGCSV cache pipelines
│   │   └── src/
│   │       ├── pokemontcg.rs
│   │       ├── pokemon_tcg_data.rs # GitHub repo importer
│   │       ├── tcgcsv.rs
│   │       └── overrides.rs        # applies data/overrides/*.json
│   └── pkdump-server/              # Axum app
│       └── src/
│           ├── lib.rs
│           ├── routes/             # one module per resource
│           └── middleware.rs
├── frontend/
│   ├── package.json
│   ├── svelte.config.js
│   ├── vite.config.ts
│   ├── tsconfig.json
│   └── src/
│       ├── routes/                 # SvelteKit pages
│       │   ├── +layout.svelte
│       │   ├── +page.svelte        # homepage
│       │   ├── collection/+page.svelte
│       │   ├── browse/[set]/[[page]]/+page.svelte
│       │   ├── card/[set]/[number]/[[variant]]/+page.svelte
│       │   ├── binders/+page.svelte
│       │   ├── binders/[id]/+page.svelte
│       │   ├── decks/+page.svelte
│       │   ├── decks/[id]/+page.svelte
│       │   ├── sealed/+page.svelte
│       │   ├── sets/+page.svelte
│       │   ├── sets/[code]/+page.svelte
│       │   ├── orders/+page.svelte
│       │   ├── orders/[id]/+page.svelte
│       │   ├── recent/+page.svelte
│       │   ├── batches/+page.svelte
│       │   ├── batches/[id]/+page.svelte
│       │   ├── ingest/manual/+page.svelte
│       │   ├── ingest/csv/+page.svelte
│       │   ├── ingest/order/+page.svelte
│       │   └── search-help/+page.svelte
│       └── lib/
│           ├── api/                # typed client wrappers
│           ├── components/         # shared Svelte components
│           ├── stores/             # binder cache, undo toast queue, etc.
│           └── types/
│               └── api.ts          # generated by ts-rs
├── tests/
│   ├── ui/                         # intents framework — TypeScript
│   │   ├── package.json
│   │   ├── playwright.config.ts
│   │   ├── intents/                # *.yaml
│   │   ├── hints/                  # *.yaml
│   │   ├── implementations/        # *.ts
│   │   ├── ux-descriptions/        # *.md per page
│   │   ├── harness.ts              # Claude Vision agent loop
│   │   ├── replay.ts               # ReplayHarness
│   │   ├── generator.ts            # vision-mode → replay script
│   │   ├── resolver.ts             # diagnose failures
│   │   ├── fixtures/
│   │   │   └── test-data.sqlite    # pre-built test fixture
│   │   └── conftest.ts             # snapshot/restore equivalent
│   └── integration/                # cargo-side integration tests
├── deploy/
│   ├── seed.sh
│   ├── setup.sh
│   ├── deploy.sh
│   ├── teardown.sh
│   ├── pkdump.container            # Quadlet template
│   └── mac-*.sh                    # macOS variants
├── scripts/
│   ├── coverage.sh
│   └── pkmngg_export.user.js       # deferred to post-v1
├── Containerfile
└── Makefile
```

CLI entry point: `pkdump`. Default DB dir: `~/.pkdump/`, contains `shared.sqlite`
+ one or more `<user>.sqlite`. Env: `PKDUMP_HOME`, `PKDUMP_USER` (defaults to
`collection`).

### 2.2 Why not Python / TypeScript-fullstack / etc.

See §13 Decisions log. Short version: Rust matches `pokedex` and gives a
~20MB static binary deploy with compile-time SQL checks; the two-language
tax (Rust + SvelteKit) is mitigated by `ts-rs` codegen.

---

## 3. Data model

### 3.1 The shared/user split

Inspired by DeckDumpster's `SHARED_TABLES` pattern. At runtime, the user DB
`ATTACH`es the shared DB read-only and creates `TEMP VIEW`s for cross-DB
joins.

**`shared.sqlite`** — immutable catalog, fully reproducible from upstream:

- `cards`, `printings`, `sets`
- `prices`, `prices_cardmarket`, `price_fetch_log`
- `sealed_products`, `sealed_product_contents`, `sealed_prices`
- `tcgplayer_groups`
- `latest_prices` VIEW, `latest_sealed_prices` VIEW
- `schema_version` (shared-schema versioning)

**`<user>.sqlite`** — per-user mutable data; **the only thing worth backing up**:

- `collection`, `decks`, `binders`, `orders`, `wishlist`
- `batches`, `status_log`, `movement_log`
- `collection_views`, `settings`
- `ingest_cache`, `ingest_lineage`
- `schema_version` (user-schema versioning, independent)

Both schemas migrate independently via `refinery` with two migration
directories (`crates/pkdump-db/migrations/shared/` and `.../user/`). The
shared DB is recreated from scratch on `pkdump setup`; user DBs are
preserved across re-seeds.

### 3.2 Shared schema (catalog)

```sql
-- shared.sqlite -----------------------------------------------------------

CREATE TABLE sets (
    set_code          TEXT PRIMARY KEY,        -- "sv3pt5" (pokemontcg.io id)
    ptcgo_code        TEXT,                    -- "MEW" (collector-facing 3-letter code)
    name              TEXT NOT NULL,           -- "151"
    series            TEXT NOT NULL,           -- "Scarlet & Violet"
    series_sort_order INTEGER,
    set_sort_order    INTEGER,
    total             INTEGER,                 -- printed total ("the 165")
    printed_total     INTEGER,                 -- including secret rares ("184")
    release_date      TEXT,
    logo_url          TEXT,
    symbol_url        TEXT,
    ptcgio_fetched_at TEXT,                    -- cache freshness marker
    tcgcsv_group_id   INTEGER UNIQUE,          -- bridges to TCGCSV
    is_subset         INTEGER NOT NULL DEFAULT 0,
    parent_set_code   TEXT REFERENCES sets(set_code)
);

CREATE TABLE cards (
    card_id                   TEXT PRIMARY KEY,    -- "sv3pt5-6"
    set_code                  TEXT NOT NULL REFERENCES sets(set_code),
    number                    TEXT NOT NULL,       -- "6", "184", "GG01", "SWSH123"
    number_sortable           INTEGER NOT NULL,    -- see §3.4
    name                      TEXT NOT NULL,
    supertype                 TEXT,                -- 'Pokémon'/'Trainer'/'Energy'
    subtypes                  TEXT,                -- JSON array
    hp                        INTEGER,
    types                     TEXT,                -- JSON array
    rarity                    TEXT,
    artist                    TEXT,
    flavor_text               TEXT,
    attacks                   TEXT,                -- JSON
    abilities                 TEXT,                -- JSON
    weaknesses                TEXT,                -- JSON
    resistances               TEXT,                -- JSON
    retreat_cost              TEXT,                -- JSON array
    regulation_mark           TEXT,                -- 'F','G','H',...
    national_pokedex_numbers  TEXT,                -- JSON array
    legalities                TEXT,                -- JSON
    image_small               TEXT,
    image_large               TEXT,
    raw_json                  TEXT,                -- full API response
    UNIQUE(set_code, number)
);
CREATE INDEX idx_cards_set        ON cards(set_code, number_sortable);
CREATE INDEX idx_cards_name       ON cards(name);
CREATE INDEX idx_cards_rarity     ON cards(rarity);

CREATE TABLE printings (
    printing_id            TEXT PRIMARY KEY,    -- "sv3pt5-6-normal"
    card_id                TEXT NOT NULL REFERENCES cards(card_id),
    variant                TEXT NOT NULL,        -- flat enum, see §4.2 of RESEARCH
    language               TEXT NOT NULL DEFAULT 'en',
    tcgplayer_product_id   INTEGER,              -- bridges to TCGCSV pricing
    image_override         TEXT,                 -- usually NULL
    badge_overlay          TEXT,                 -- 'STAMP'|'PRERELEASE'|...
    deprecated_at          TEXT,                 -- soft-delete for overlay removals
    UNIQUE(card_id, variant, language)
);
CREATE INDEX idx_printings_card ON printings(card_id);
CREATE INDEX idx_printings_tcg  ON printings(tcgplayer_product_id);

CREATE TABLE prices (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    tcgplayer_product_id  INTEGER NOT NULL,
    sub_type_name         TEXT NOT NULL,        -- 'Normal'|'Holofoil'|'Reverse Holofoil'|...
    source                TEXT NOT NULL DEFAULT 'tcgplayer',
    price_type            TEXT NOT NULL,        -- 'low'|'mid'|'high'|'market'|'directLow'
    price                 REAL NOT NULL,
    observed_at           TEXT NOT NULL,
    UNIQUE(tcgplayer_product_id, sub_type_name, source, price_type, observed_at)
);
CREATE INDEX idx_prices_product ON prices(tcgplayer_product_id);
CREATE INDEX idx_prices_date    ON prices(observed_at);

CREATE TABLE prices_cardmarket (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id         TEXT NOT NULL REFERENCES cards(card_id),
    variant         TEXT NOT NULL,             -- 'normal' or 'reverse_holo' (CM only splits these)
    avg_sell_price  REAL,
    low_price       REAL,
    trend_price     REAL,
    avg30           REAL,
    avg7            REAL,
    avg1            REAL,
    observed_at     TEXT NOT NULL,
    UNIQUE(card_id, variant, observed_at)
);

CREATE VIEW latest_prices AS
SELECT p.* FROM prices p
JOIN (SELECT tcgplayer_product_id, sub_type_name, source, price_type,
             MAX(observed_at) AS observed_at
      FROM prices GROUP BY 1,2,3,4) m
  ON p.tcgplayer_product_id=m.tcgplayer_product_id
 AND p.sub_type_name=m.sub_type_name
 AND p.source=m.source
 AND p.price_type=m.price_type
 AND p.observed_at=m.observed_at;

CREATE TABLE price_fetch_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source      TEXT NOT NULL,                 -- 'tcgcsv'|'pokemontcg.io'
    set_code    TEXT,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    status      TEXT NOT NULL,                 -- 'success'|'partial'|'failed'
    rows_inserted INTEGER,
    error       TEXT
);

CREATE TABLE tcgplayer_groups (
    group_id     INTEGER PRIMARY KEY,
    set_code     TEXT,
    name         TEXT NOT NULL,
    abbreviation TEXT,
    published_on TEXT,
    fetched_at   TEXT NOT NULL
);

CREATE TABLE sealed_products (
    product_id      INTEGER PRIMARY KEY,       -- TCGplayer productId
    set_code        TEXT REFERENCES sets(set_code),
    name            TEXT NOT NULL,
    category        TEXT NOT NULL,             -- 'booster_pack'|'booster_box'|'etb'|'bundle'|'tin'|...
    subtype         TEXT,
    card_count      INTEGER,                   -- cards per pack
    product_size    INTEGER,                   -- packs per box/bundle
    release_date    TEXT,
    image_url       TEXT,
    tcgplayer_url   TEXT,
    fetched_at      TEXT NOT NULL
);
CREATE INDEX idx_sealed_products_set      ON sealed_products(set_code);
CREATE INDEX idx_sealed_products_category ON sealed_products(category);

-- Optional: pull-recipe table for the "open product" flow. Hand-curated.
CREATE TABLE sealed_product_contents (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id      INTEGER NOT NULL REFERENCES sealed_products(product_id),
    slot_index      INTEGER NOT NULL,
    slot_label      TEXT,
    candidate_kind  TEXT NOT NULL,             -- 'card_id'|'rarity_pool'|'printing_id'
    candidate_value TEXT NOT NULL,
    weight          REAL DEFAULT 1.0
);
CREATE INDEX idx_spc_product ON sealed_product_contents(product_id);

CREATE TABLE sealed_prices (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    tcgplayer_product_id  INTEGER NOT NULL,
    low_price             REAL,
    mid_price             REAL,
    high_price            REAL,
    market_price          REAL,
    direct_low_price      REAL,
    observed_at           TEXT NOT NULL,
    UNIQUE(tcgplayer_product_id, observed_at)
);
CREATE INDEX idx_sealed_prices_product ON sealed_prices(tcgplayer_product_id);
CREATE INDEX idx_sealed_prices_date    ON sealed_prices(observed_at);

CREATE VIEW latest_sealed_prices AS
SELECT sp.* FROM sealed_prices sp
JOIN (SELECT tcgplayer_product_id, MAX(observed_at) AS observed_at
      FROM sealed_prices GROUP BY 1) m
  ON sp.tcgplayer_product_id=m.tcgplayer_product_id
 AND sp.observed_at=m.observed_at;

CREATE TABLE schema_version (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);
```

### 3.3 Per-user schema (collection)

```sql
-- <user>.sqlite -----------------------------------------------------------

CREATE TABLE binders (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT NOT NULL,
    description       TEXT,
    color             TEXT,
    binder_type       TEXT,
    pocket_size       INTEGER NOT NULL DEFAULT 9,
    storage_location  TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE decks (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT NOT NULL,
    description       TEXT,
    format            TEXT,                    -- free-text tag: 'standard'|'expanded'|'casual'|NULL
    owner             TEXT,                    -- free-text: "Ryan"|"Alice"
    state             TEXT NOT NULL DEFAULT 'idea'
        CHECK (state IN ('idea','ready','built')),  -- lifecycle, see S7
    sleeve_color      TEXT,
    storage_location  TEXT,
    notes             TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE orders (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    order_number    TEXT,
    source          TEXT NOT NULL,             -- 'tcgplayer'|'ebay'|'pokemoncenter'|'lgs'|'other'
    seller_name     TEXT,
    order_date      TEXT,
    subtotal        REAL,
    shipping        REAL,
    tax             REAL,
    total           REAL,
    shipping_status TEXT,
    estimated_delivery TEXT,
    notes           TEXT,
    created_at      TEXT NOT NULL,
    UNIQUE(source, order_number)
);

CREATE TABLE batches (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_type  TEXT NOT NULL,                 -- 'manual_id'|'binder_click'|'csv_manabox'|'csv_tcgplayer'|'order_tcg'|...
    name        TEXT,
    notes       TEXT,
    order_id    INTEGER REFERENCES orders(id),
    binder_id   INTEGER REFERENCES binders(id),
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_batches_type ON batches(batch_type);

-- The big one. One row per physical card.
CREATE TABLE collection (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    printing_id     TEXT NOT NULL,             -- FK enforced at app layer (cross-DB)
    condition       TEXT NOT NULL DEFAULT 'Near Mint'
        CHECK (condition IN ('Near Mint','Lightly Played','Moderately Played','Heavily Played','Damaged')),
    language        TEXT NOT NULL DEFAULT 'English',
    purchase_price  REAL,
    sale_price      REAL,
    acquired_at     TEXT NOT NULL,
    source          TEXT NOT NULL,             -- 'manual_id'|'binder_click'|'csv_manabox'|'order_import'|...
    notes           TEXT,
    tags            TEXT,                      -- JSON array
    graded          INTEGER NOT NULL DEFAULT 0,
    grade_company   TEXT,                      -- 'PSA'|'BGS'|'CGC'|'SGC'|'TAG'|'ACE'|...
    grade_value     REAL,
    grade_cert      TEXT,
    status          TEXT NOT NULL DEFAULT 'owned'
        CHECK (status IN ('owned','ordered','listed','sold','removed','traded','gifted','lost')),
    order_id        INTEGER REFERENCES orders(id),
    binder_id       INTEGER REFERENCES binders(id) ON DELETE SET NULL,
    deck_id         INTEGER REFERENCES decks(id) ON DELETE SET NULL,
    batch_id        INTEGER REFERENCES batches(id),
    CHECK (binder_id IS NULL OR deck_id IS NULL)   -- mutual exclusivity
);
CREATE INDEX idx_collection_printing ON collection(printing_id);
CREATE INDEX idx_collection_binder   ON collection(binder_id);
CREATE INDEX idx_collection_deck     ON collection(deck_id);
CREATE INDEX idx_collection_status   ON collection(status);

CREATE TABLE status_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    from_status   TEXT,
    to_status     TEXT NOT NULL,
    changed_at    TEXT NOT NULL,
    note          TEXT
);

CREATE TABLE movement_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id   INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
    from_binder_id  INTEGER REFERENCES binders(id),
    to_binder_id    INTEGER REFERENCES binders(id),
    from_deck_id    INTEGER REFERENCES decks(id),
    to_deck_id      INTEGER REFERENCES decks(id),
    changed_at      TEXT NOT NULL,
    note            TEXT
);

CREATE TABLE wishlist (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id       TEXT NOT NULL,               -- FK to shared.cards, enforced at app layer
    printing_id   TEXT,                        -- NULL = any printing
    max_price     REAL,
    priority      INTEGER NOT NULL DEFAULT 0,
    notes         TEXT,
    added_at      TEXT NOT NULL,
    source        TEXT NOT NULL DEFAULT 'manual',
    fulfilled_at  TEXT
);

CREATE TABLE sealed_collection (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    product_id     INTEGER NOT NULL,            -- FK to shared.sealed_products, app-layer
    quantity       INTEGER NOT NULL DEFAULT 1,
    condition      TEXT DEFAULT 'Near Mint',
    purchase_price REAL,
    sale_price     REAL,
    purchase_date  TEXT,
    source         TEXT,
    seller_name    TEXT,
    notes          TEXT,
    status         TEXT NOT NULL DEFAULT 'owned'
        CHECK (status IN ('owned','listed','sold','traded','gifted','opened')),
    added_at       TEXT NOT NULL
);
CREATE INDEX idx_sealed_collection_product ON sealed_collection(product_id);
CREATE INDEX idx_sealed_collection_status  ON sealed_collection(status);

CREATE TABLE collection_views (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT NOT NULL,
    description  TEXT,
    filters_json TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE schema_version (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);
```

### 3.4 Sort key for binder pagination

`cards.number_sortable` solves the mixed-namespace pagination problem:

| Number form | Example | `number_sortable` |
|---|---|---|
| Plain numeric N | `19` | `19` |
| Secret rare (`N > printed_total`) | `184` | `184` |
| Galarian Gallery `GG##` | `GG01` | `1001` |
| Trainer Gallery `TG##` | `TG01` | `2001` |
| SWSH promo `SWSH###` | `SWSH123` | `9123` |
| SVP promo `SVP ###` | `SVP 042` | `10042` |

Computed once during ingest. Indexed on `(set_code, number_sortable)`.

### 3.5 Cross-DB referential integrity

SQLite's `REFERENCES` doesn't enforce across ATTACHed databases. Two strategies:

- **`printing_id`, `card_id`, `product_id`** are validated **at the repository
  layer** before insert/update. The shared catalog is read-only at runtime,
  so the keys can't go stale during a write.
- On `pkdump setup` rebuild of `shared.sqlite`, we run a **validation pass**
  that checks for orphaned `collection.printing_id` references and surfaces a
  warning. Orphans are auto-soft-deprecated; never deleted.

---

## 4. Variant expansion — the three layers

The pipeline (in `crates/pkdump-ingest/src/overrides.rs`):

```rust
pub fn expand_variants(card: &PokemonTcgCard) -> Vec<Variant> {
    let mut variants = expand_from_price_keys(card);           // Layer 1
    apply_overrides(&mut variants, card, &OVERRIDES);          // Layer 2
    if variants.is_empty() {
        variants = bootstrap_from_rarity(card);                // Layer 3
    }
    variants
}
```

### 4.1 Layer 1 — data-driven

Hardcoded map from pokemontcg.io's TCGplayer price keys to our variant codes:

```rust
const PRICE_KEY_TO_VARIANT: &[(&str, Variant)] = &[
    ("normal",             Variant::Normal),
    ("holofoil",           Variant::Holo),
    ("reverseHolofoil",    Variant::ReverseHolo),
    ("1stEditionHolofoil", Variant::FirstEdHolo),
    ("1stEditionNormal",   Variant::FirstEdNormal),
    ("unlimitedHolofoil",  Variant::UnlimitedHolo),
];
```

Authoritative when present. Self-maintaining as TCGplayer adds new keys.
Blind to curator-curated variants (Master Ball patterns, PC stamps, etc.).

### 4.2 Layer 2 — JSON overlay

Single file `data/overrides/variant_augmentations.json`, flat list of records.
Two record shapes share one file: `match`-based rules (cover many cards) and
explicit per-card overrides. Both can `add` or `remove` variants.

```json
[
  {
    "comment": "151: C/U/R get Poke Ball + Master Ball pattern reverse holos",
    "match": {"set": "sv3pt5", "rarity": ["Common", "Uncommon", "Rare"]},
    "add": ["pokeball_rh", "masterball_rh"]
  },
  {
    "comment": "Prismatic Evolutions: same gimmick as 151",
    "match": {"set": "sv8pt5", "rarity": ["Common", "Uncommon", "Rare"]},
    "add": ["pokeball_rh", "masterball_rh"]
  },
  {
    "comment": "Pokémon Center 151 ETB stamped Charizard ex",
    "match": {"set": "sv3pt5", "number": "199"},
    "add": ["stamp_pokemoncenter"]
  },
  {
    "comment": "Spurious RH entry from a bad price key",
    "match": {"set": "sv4", "number": "157"},
    "remove": ["reverse_holo"]
  }
]
```

Loaded once at startup (`include_str!`), parsed into a `Vec<OverrideRule>`
with the `match` predicate compiled to a closure. Processed in file order;
later records win on conflict.

Sibling files exist for **other dirty-data concerns** following the same
flat-record pattern:

- `subset_mappings.json` — `GG##`/`TG##` numbers → parent set + display section
- `promo_tie_ins.json` — "this SVP promo belongs to this set"
- `set_aliases.json` — `ptcgo_code` corrections / collector renames

`data/known_issues.md` documents the upstream-data bugs the overrides exist
to work around. Same discipline as `pokedex/data/known_issues.md`.

### 4.3 Layer 3 — rarity bootstrap

Only runs when Layer 1 returns nothing (brand-new set, pokemontcg.io has no
pricing yet). Pure function of `rarity`:

```rust
fn bootstrap_from_rarity(card: &PokemonTcgCard) -> Vec<Variant> {
    match card.rarity.as_deref() {
        Some("Common" | "Uncommon" | "Rare") => vec![Variant::Normal, Variant::ReverseHolo],
        Some("Rare Holo") => vec![Variant::Holo, Variant::ReverseHolo],
        Some("Double Rare" | "Ultra Rare" | "Illustration Rare"
            | "Special Illustration Rare" | "Hyper Rare"
            | "Trainer Gallery Rare Holo") => vec![Variant::UltraRare],
        _ => vec![Variant::Normal],
    }
}
```

### 4.4 Soft-deprecation policy

When an overlay `remove` deletes a printing the user owns:

1. Mark `printings.deprecated_at = NOW()`.
2. Keep the printing row (FK from `collection` still resolves).
3. Hide deprecated printings from binder grid and add-card flows.
4. Show a "deprecated" label on the card detail page so the user can rebadge
   their copy if needed.

No hard deletes. Ever.

### 4.5 Self-refinement

Per the resolved direction: **the system self-refines as upstream improves.**
No audit subcommand, no Bulbapedia scraper, no notification ceremony. When a
new variant gimmick ships, you have three options:

1. Wait until TCGplayer prices it (Layer 1 catches it automatically).
2. Add a one-line JSON record (Layer 2).
3. Ignore it.

---

## 5. URL / API surface

### 5.1 Pages (SvelteKit routes)

```
/                                  homepage / nav hub
/collection                        primary browser
/card/[set]/[number]/[[variant]]   card detail
/binders                           binder list
/binders/[id]                      binder detail
/decks                             deck list
/decks/[id]                        deck detail
/browse                            NEW: binder-page browsing entry (set picker)
/browse/[set]/[[page]]             NEW: specific set binder, optional page
/sealed                            sealed collection
/sets                              set list (released, value owned, completion %)
/sets/[code]                       per-set analytical view
/orders                            order list
/orders/[id]                       order detail
/recent                            recent batches
/batches                           all batches
/batches/[id]                      batch detail
/ingest/manual                     manual ID entry
/ingest/csv                        CSV import
/ingest/order                      paste order
/search-help                       query language reference
```

### 5.2 API (Axum routes under `/api`)

Verbs are HTTP-conventional. Every JSON payload corresponds to a `#[derive(TS)]`
struct so the frontend gets typed bindings.

```
GET    /api/collection                          list + filter
POST   /api/collection                          add a copy
POST   /api/collection/bulk                     bulk add (binder-click flush, CSV commit)
PUT    /api/collection/:id                      edit
DELETE /api/collection/:id                      delete
POST   /api/collection/bulk-delete              bulk delete

GET    /api/wishlist                            list
POST   /api/wishlist                            add
POST   /api/wishlist/bulk                       bulk add
DELETE /api/wishlist/:id

GET    /api/binders                             list
POST   /api/binders                             create
GET    /api/binders/:id                         detail (cards)
PUT    /api/binders/:id
DELETE /api/binders/:id
POST   /api/binders/:id/cards                   assign card(s) to binder

GET    /api/decks                               list
POST   /api/decks                               create
GET    /api/decks/:id
PUT    /api/decks/:id
DELETE /api/decks/:id
POST   /api/decks/:id/cards                     assign card(s) to deck

GET    /api/cards/by-set-cn?set=&cn=            card lookup
GET    /api/card/:set/:number                   full payload (printings, prices, copies)

GET    /api/sets                                set list w/ counts
GET    /api/sets/:code                          set detail
GET    /api/sets/:code/binder?page=&layout=&include=
                                                NEW: binder-page data

GET    /api/sealed/products                     sealed catalog search
GET    /api/sealed/collection                   user's sealed
POST   /api/sealed/collection                   add sealed
PUT    /api/sealed/collection/:id

GET    /api/orders                              list
GET    /api/orders/:id
POST   /api/orders/parse                        parse pasted order text/HTML
POST   /api/orders/commit                       commit parsed order
POST   /api/orders/:id/receive                  flip all 'ordered' → 'owned'

GET    /api/import/csv/preview                  preview without commit
POST   /api/import/csv/commit                   commit

GET    /api/recent                              recent batches
GET    /api/batches                             batch list
GET    /api/batches/:id                         batch detail

GET    /api/settings
PUT    /api/settings

GET    /api/views                               saved filter views
POST   /api/views
PUT    /api/views/:id
DELETE /api/views/:id

GET    /api/data/refresh                        SSE — refresh progress stream
POST   /api/data/refresh                        trigger refresh
```

---

## 6. The binder-browse feature (headline)

### 6.1 Layout

- One slot per card number (no RH-inline).
- 9-pocket default (`/browse/sv3pt5?layout=9`); also 4-pocket and 12-pocket.
- Set-number order via `number_sortable` ASC.
- Secret rares (`number_sortable > printed_total`) automatically render in a
  trailing "Secret Rares" section after the numbered set.
- Subsets (`is_subset = 1`) render as separate appended sections.
- Promos toggleable via the include chrome; off by default.

### 6.2 Slot rendering

Each slot displays:

- Card image (the canonical base printing — usually `normal` or `holo` per rarity)
- Card number + name in the footer
- **Inline quick-toggle checkboxes** for the two most common variants per rarity:
  - C/U/R → `Reg` + `RH`
  - Holo Rare → `Holo` + `RH`
  - Ultra Rare / IR / SIR / Hyper Rare / promo → no inline checkboxes (modal only)
- **Pip row** below the checkboxes — one dot per known variant, filled if owned ≥1 copy

### 6.3 Variant modal

Clicking the slot (not the checkboxes) opens a modal listing all variants:

- One row per variant: name, image thumb, market price, owned count, +/- buttons
- "Edit copies…" expands per-copy controls (condition, language, notes, status)
- "Open full card detail page →" link

### 6.4 Master Set semantics

Two completion stats shown side-by-side at top:

- **Base**: `card_numbers_with_at_least_one_variant / total_numbered_cards`
- **Master**: `variants_owned / total_known_variants` (includes secret rares + subsets + promos for this set)

Plus an **`Incomplete only`** filter pill that hides slots whose pip row is full.

### 6.5 Include chrome

Inline checkbox group controlling which sections render:

```
[ ◉ Base set  ◉ + Secret Rares  ◉ + Subset (GG/TG)  ☐ + Promos ]
```

The reverse-holo toggle is gone (stacked design makes it unnecessary).
Promos off by default — they're temporally orphaned and bloat the binder.

### 6.6 Click-to-add interaction

Quick path (inline checkbox):

1. Click `Reg` checkbox on slot
2. Optimistic UI: checkbox fills, first pip fills, slot border flashes green
3. `POST /api/collection` fires with `{printing_id, condition: 'Near Mint', source: 'binder_click', batch_id: <current session>}`
4. Toast: "Added Bulbasaur (Normal) — undo"

Full path (modal):

1. Click slot body
2. Modal opens
3. Click `+` next to a variant
4. Same optimistic UI + POST
5. Modal stays open; user can add more

### 6.7 Sessions & batches

Starting a binder-browse session creates a `batches` row with
`batch_type='binder_click'`, `name='Binder: 151 — 2026-05-18'`. All adds in
that session belong to it. Reviewable on `/recent`. From there, the user can
e.g. assign every card in the batch to a specific binder retroactively.

### 6.8 Performance

- 9 hi-res images × ~700KB worst case = ~6.3MB per page. `loading="lazy"`,
  preload N+1 on `requestIdleCallback`.
- API response is small (~30KB for 9 slots). TanStack Query caches by
  `(set, page, layout, include)`; invalidated on collection writes that touch
  the set.
- Pip counts: one indexed query per page —
  `SELECT printing_id, COUNT(*) FROM collection WHERE printing_id IN (...) GROUP BY 1`. <1ms.

### 6.9 Mobile

- 3 cols → 2 below 768px → 1 below 480px.
- Inline checkboxes always visible (touch-friendly tap targets).
- Variant modal → bottom sheet at ~70% height.

---

## 7. Decks (minimal)

- Named, with description, free-text `format` tag, free-text `owner` tag (`"Ryan"`, `"Alice"`), sleeve color, storage location, notes.
- **3-state lifecycle**: `idea` → `ready` → `built`. An `idea` deck is something you're planning; `ready` is a finalized list; `built` means it physically exists in a box. The lifecycle is the only deck-planning concept we keep — it's domain-generic, not MTG-specific. Once a deck is `built`, the variant-swap flow warns before letting you change a card (mirrors DeckDumpster's "constructed" guard).
- Cards added from collection via picker-search modal (same UX as binders).
- `deck_id` mutually exclusive with `binder_id` on a collection row — enforced via CHECK constraint AND repository layer; HTTP 409 on conflict.
- `/decks/:id` lists cards grouped by supertype (Pokémon / Trainer / Energy) — Pokémon's natural grouping rather than CMC.
- Card detail page surfaces "This card lives in: [Binder name] / [Deck name]" per copy.
- Multi-select on `/collection` includes "Move to deck" alongside "Move to binder".

**Not** included: format-legality validation, mainboard/sideboard/commander zones, planning-mode "expected cards" lists, completeness scoring, deck-share URLs. All cuttable; can come later.

---

## 8. Sealed

Direct port from DeckDumpster's sealed pipeline, with one constraint:
**no `contents_json` equivalent from upstream**. So:

- v1: sealed collection works fully (add, edit, dispose, sell, gift, mark opened). The "Open product → bulk-add resolved cards" flow is gated on `sealed_product_contents` being populated.
- v1 ships with `sealed_product_contents` empty. The "Open product" button shows a "No contents data — log pulls manually" message and links to the manual ID ingest with the product set pre-selected.
- v2 maybe: hand-curate the most-opened products (booster packs for SV-era sets) with pull-rate recipes.

---

## 9. CSV import

### 9.1 ManaBox (primary)

ManaBox is the de-facto interchange format. CSV columns:

```
Set code, Set name, Collector number, Foil, Rarity, Quantity,
ManaBox ID, Scryfall ID, Purchase price, Misprint, Altered,
Condition, Language, Purchase price currency
```

Column → field mapping:

| ManaBox | PokeDumpster | Notes |
|---|---|---|
| Set code | `cards.set_code` (via `ptcgo_code` lookup, then `name` fallback) | |
| Collector number | `cards.number` (string match) | |
| Foil | `printings.variant` | `'foil'`/`'reverseHolofoil'` → our variant codes |
| Quantity | one row per copy | strict 1:1, no aggregation |
| Condition | `collection.condition` | direct map |
| Language | `collection.language` | direct map |
| Purchase price | `collection.purchase_price` | convert via `currency` to USD-equiv |
| Misprint/Altered | `collection.tags` | merged into JSON array |

Resolve → preview unmatched rows → user reviews → commit. Idempotent if
`Purchase price` + `acquired_at` match an existing copy.

### 9.2 TCGplayer mass-entry CSV (runner-up)

TCGplayer order-history CSVs and mass-entry exports use a different shape but
the parser is straightforward. Mostly used to suck in order history after
the fact when the order itself wasn't ingested through `/ingest/order`.

### 9.3 Architecture

`crates/pkdump-core/src/import/` has one module per format. Each exposes:

```rust
pub trait CsvImporter {
    fn parse(&self, input: &str) -> Result<Vec<ParsedRow>>;
    fn resolve(&self, rows: Vec<ParsedRow>, db: &Db) -> Result<ResolutionReport>;
}
```

The resolver always produces a preview before commit. Same pattern
DeckDumpster uses; the only difference is which formats live in the registry.

---

## 10. The intents UI testing framework — TypeScript port

### 10.1 What changes from DeckDumpster

- Language: Python → TypeScript.
- Browser automation: Playwright Python → Playwright TS (same engine, same selectors).
- LLM SDK: `anthropic` Python → `@anthropic-ai/sdk`.
- Snapshot/restore fixture: SQLite `.backup()` → same idea, called via Bun's built-in SQLite or via `cp` shell calls.
- Test runner: pytest → Playwright's own runner (`@playwright/test`) which already does what pytest+conftest do here.

### 10.2 What stays the same

- The five-layer artifact stack: **UX descriptions → test plans → approved intents → intent YAML → hint YAML → implementation TS**.
- The three execution modes: **replay**, **vision**, **generation**.
- The resolver classifier: **test_failure / system_failure / environment_failure**.
- Vocabulary: **"intents"**, not "screenplays".
- The `/qa-finish` skill — ported verbatim, prompts updated for TS implementation files.

### 10.3 Harness method surface

The TypeScript `ReplayHarness` exposes the same methods as the Python one:

```typescript
class ReplayHarness {
    async navigate(path: string): Promise<void>
    async click_by_text(text: string, opts?: { exact?: boolean }): Promise<void>
    async click_by_selector(selector: string): Promise<void>
    async click_by_test_id(testId: string): Promise<void>
    async fill_by_placeholder(placeholder: string, value: string): Promise<void>
    async fill_by_selector(selector: string, value: string): Promise<void>
    async select_by_label(selector: string, label: string): Promise<void>
    async press_key(key: string, opts?: { selector?: string }): Promise<void>
    async scroll(direction: 'up' | 'down'): Promise<void>
    async wait_for_visible(selector: string, timeoutMs?: number): Promise<void>
    async wait_for_hidden(selector: string, timeoutMs?: number): Promise<void>
    async wait_for_text(text: string, timeoutMs?: number): Promise<void>
    async screenshot(label: string): Promise<void>
}
```

Implementation files become:

```typescript
// tests/ui/implementations/binders_add_cards_full_flow.ts
import type { ReplayHarness } from '../replay';

export async function steps(h: ReplayHarness) {
    await h.navigate('/binders');
    await h.wait_for_text('Trade Binder');
    await h.click_by_text('Trade Binder');
    await h.wait_for_visible('.detail-view.active');
    await h.click_by_text('Add Cards');
    await h.wait_for_visible('#add-cards-modal.active');
    await h.fill_by_placeholder('Search by name...', 'pr');
    await h.wait_for_text('Preacher of the Schism');  // (Pokémon equivalent)
    await h.click_by_text('Preacher of the Schism');
    await h.click_by_text('Add Selected');
    await h.wait_for_hidden('#add-cards-modal.active');
    await h.wait_for_text('Preacher of the Schism');
    await h.screenshot('final_state');
}
```

### 10.4 Intent corpus

See §15 for the full classified list (175 ported + 25 new for binder-browse =
~200 first-wave intents). No deferrals; we grow the corpus to full coverage
as we implement features, with `/qa-finish` driving each new chunk.

---

## 11. Deployment

Port DeckDumpster's deploy infrastructure with two changes:

1. **Container is multi-stage Rust build → distroless runtime + binary +
   SvelteKit `dist/`**. Image ~20–40 MB.
2. **No `ANTHROPIC_API_KEY` required at server runtime** — only the intents
   harness uses it.

```
deploy/
├── seed.sh                # build seed shared.sqlite volume, one-time
├── setup.sh               # per-instance: <instance> [--init|--test]
├── deploy.sh              # rebuild image + restart
├── teardown.sh            # stop + remove [--purge]
├── pkdump.container       # Quadlet template with {{INSTANCE}}/{{PORT}}
└── mac-*.sh               # macOS variants (no systemd)
```

Service: `pkdump-<instance>`. Default instance: `prod`. CI auto-deploys `prod`
on push to `main`. Per-instance volumes carry `<user>.sqlite`; `shared.sqlite`
lives on a shared volume across instances (re-cloneable from seed in seconds).

**Remote access**: WireGuard. No app-level auth in v1. Documentation in
`README.md` shows the WireGuard setup; this is a personal-only tool.

---

## 12. Milestones

Each milestone is a self-contained PR-able chunk. Sequencing prioritizes "can
I see the binder page work?" first, then deepens.

### M1 — Repo bootstrap + shared catalog (~3 days)

- Cargo workspace, all crates skeletoned, `cargo build` succeeds.
- `refinery` migrations for `shared.sqlite` v1.
- `pkdump-ingest::pokemon_tcg_data` clones the GitHub repo and seeds cards/sets.
- `pkdump-ingest::pokemontcg` fills the tail of newest sets.
- `pkdump-ingest::tcgcsv` fills sealed products + tcgplayer_groups.
- `pkdump-ingest::overrides` reads `data/overrides/variant_augmentations.json` and runs the three-layer expansion.
- `pkdump setup` end-to-end populates `shared.sqlite`.
- Trivial homepage at `/` shows "cards loaded: N, sets: M, printings: P" sanity check.
- Containerfile + `deploy/setup.sh --test`.

### M2 — Per-user schema + collection basics (~3 days)

- `refinery` migrations for `<user>.sqlite` v1.
- ATTACH plumbing in `pkdump-db`; tested with cross-DB join.
- `pkdump-server` routes for `/api/collection` (CRUD), `/api/cards/by-set-cn`, `/api/card/:set/:number`.
- SvelteKit pages: `/collection` (table view + search), `/card/:set/:number/:variant`.
- `/ingest/manual` page.
- `ts-rs` codegen wired into `cargo build`.

### M3 — Binder-page browsing (~4 days, headline)

- `/api/sets/:code/binder?page=&layout=&include=` endpoint.
- `/browse/:set/[page]` SvelteKit page with 3×3 grid.
- Slot component: image + checkboxes + pips.
- Variant modal component (pkmn.gg-style picker).
- Inline checkbox → optimistic UI → undo toast.
- `include` chrome, layout dropdown, master-set stats.
- Pagination + URL persistence + sort modes.
- Mobile breakpoints.

### M4 — Binders + decks + sealed + orders (~4 days)

- `/api/binders`, `/api/decks`, `/api/sealed/*`, `/api/orders/*`.
- SvelteKit pages: `/binders`, `/binders/:id`, `/decks`, `/decks/:id`, `/sealed`, `/orders`, `/orders/:id`, `/ingest/order`.
- TCGplayer paste parser (Rust port of DeckDumpster's logic).
- Picker-search assign flow (shared component across binders and decks).
- `deck_id ⊕ binder_id` enforcement at repo layer + CHECK constraint.

### M5 — Wishlist + saved views + multi-select + recent/batches (~2 days)

- Wishlist panel on `/collection`.
- `collection_views` save/load.
- Multi-select bulk ops (delete, assign to binder, assign to deck, mark wanted).
- `/recent`, `/batches`, `/batches/:id`.

### M6 — Set value + sets list (~1 day)

- `/sets` with completion %.
- `/sets/:code` analytical view (value, rarity split, owned %).

### M7 — CSV import (~2 days)

- ManaBox parser → preview → commit.
- TCGplayer mass-entry parser.
- `/ingest/csv` page.
- CSV export (ManaBox round-trippable).

### M8 — Intents framework + first wave (~7 days, parallelizable)

- Port `harness.ts`, `replay.ts`, `generator.ts`, `resolver.ts`.
- Build `tests/ui/fixtures/test-data.sqlite` Pokémon seed.
- Port `.claude/skills/qa-finish/SKILL.md`.
- Write `ux-descriptions/*.md` for every page.
- Write ~200 intents per the corpus in §15. Use `/qa-finish` per page.

### M9 — Daily refresh + deploy to prod (~1 day)

- systemd timer running `pkdump data refresh` nightly.
- CI auto-deploy of `prod`.
- Backup script for per-user `.sqlite` files.

### M10 — pkmn.gg userscript (post-v1, gravy)

- `scripts/pkmngg_export.user.js` Tampermonkey script.
- Documented manual fallback.
- Variant-string mapping from pkmn.gg's conventions.

**Rough total to v1**: ~27 days for a single dev. M3 (binder browse) and M8
(intents) are the biggest chunks. M8 is parallelizable across pages.

---

## 13. Decisions log

Resolved through interactive review on 2026-05-17 → 2026-05-18.

| # | Decision | What was considered | What we picked | Why |
|---|---|---|---|---|
| 1 | Card identity model | Two-layer oracle/printings vs. flat cards/printings | Flat | Pokémon has no meaningful oracle layer — each set's "Charizard ex" is a different card |
| 2 | Variant axis | Flat enum vs. orthogonal axes (rarity × finish × stamp) | Flat enum | Matches how upstream APIs and collectors talk about variants; constraint tables for axes would be heavy |
| 3 | Quantity aggregation | Strict 1-row-per-copy vs. `quantity > 1` for bulk | Strict 1-row-per-copy | DeckDumpster's existing preference (author wrote both); audit log clarity > row count |
| 4 | Variant augmentation home | Python dict / YAML / JSON-file overlay | JSON files in `data/overrides/`, applied as final phase of ingest | Pokedex pattern, proven; per-record blameable |
| 5 | Crack-a-pack | Port DeckDumpster's vs. drop | Drop | No equivalent UX motivation for Pokémon |
| 6 | Decks | Drop entirely vs. minimal model | Minimal model | User builds decks with daughter; needs to track where cards live |
| 7 | Deck planning-mode | Include "expected cards" lists vs. owned-only | Owned-only | User explicitly excluded planning |
| 8 | Binder layout | RH inline as separate slots vs. stacked one-per-number | Stacked | User preference; matches pkmn.gg pattern |
| 9 | Variant selector | In-place strip vs. modal | Modal | pkmn.gg pattern user already knows |
| 10 | Inline shortcuts | None vs. quick-toggle checkboxes for common pair | Quick-toggle checkboxes | User asked specifically; 80% case gets one-click access |
| 11 | Master Set semantics | Layout toggle vs. completion-counting policy | Completion-counting + filter pill | Stacked layout makes the layout-toggle interpretation moot |
| 12 | DB split | Single DB vs. shared/per-user split | Shared + per-user, ATTACHed | DeckDumpster's existing pattern; forward path to multi-tenant without auth layer |
| 13 | Remote access | App auth vs. localhost-only vs. WireGuard | WireGuard | User already uses this for personal tools |
| 14 | Stack — backend | Python (DeckDumpster) vs. Rust vs. TS | Rust | User preference; matches `pokedex`; single binary deploy |
| 15 | Stack — frontend | HTMX vs. Vanilla TS vs. SvelteKit | SvelteKit | Best DX for the binder UX's reactivity |
| 16 | Type sync (Rust ↔ TS) | Manual vs. `ts-rs` codegen | `ts-rs` | Auto-syncs API contract; one less thing to drift |
| 17 | Variant expansion | Data-driven only vs. rules-only vs. three-layer | Three-layer | Each layer covers a real case the others can't |
| 18 | Override directives | `add` only vs. `add` + `remove` | Both | `remove` needed for correcting spurious Layer-1 inferences |
| 19 | Removed-printing policy | Hard-delete vs. soft-deprecate | Soft-deprecate | Never break user-owned data |
| 20 | Augmentation curation | Manual + scraper vs. manual only vs. self-refining | Self-refining + lazy manual | User: "I'm not that neurotic of a collector" |
| 21 | CSV import — primary | ManaBox vs. Collectr vs. pkmn.gg | ManaBox | De-facto interchange format |
| 22 | CSV import — runner-up | Collectr vs. TCGplayer mass-entry vs. pkmn.gg | TCGplayer mass-entry | Likely to receive order CSVs anyway |
| 23 | Intents corpus first wave | Curated ~80 vs. all-relevant ~175 | All relevant | User: "don't even wave it" |
| 24 | Intents framework language | Python (port verbatim) vs. TypeScript | TypeScript | Backend is Rust; tests stay in same ecosystem as frontend |
| 25 | "intents" vs. "screenplays" naming | DeckDumpster's "intents" vs. pokedex's "screenplays" | "intents" | Framework is being ported from DeckDumpster |
| 26 | pkmn.gg migration | Userscript v1 vs. deferred | Deferred post-v1 | Get the bones working with a test dataset first |
| 27 | SQL access layer | sqlx (compile-time checked) vs. rusqlite | rusqlite | User preference; matches `pokedex`; query correctness covered by integration tests |
| 28 | Cross-DB FK enforcement | App-layer validation vs. single DB | App-layer validation | Accepted; revisit ("andon cord") if it gets janky |
| 29 | Deck lifecycle states | None (minimal) vs. 3-state idea/ready/built | 3-state lifecycle | Domain-generic, not MTG-specific; useful for kitchen-table "planning vs. built" distinction |

---

## 14. Open questions

Genuinely open, not just deferred:

1. ~~sqlx vs. rusqlite~~ — **RESOLVED: rusqlite.** Matches `pokedex`, simpler, full control over ATTACH. Cost: no compile-time query checking; integration tests cover that ground instead.

2. **TanStack Query vs. native Svelte stores for client cache.** TanStack Query gives optimistic updates + invalidation for free; native stores are simpler but reinventing some wheels. Leaning TanStack Query but it's the kind of dep that grows tendrils.

3. **shadcn-svelte vs. roll-your-own components.** shadcn-svelte is the modern default for unopinionated UI primitives; rolling our own keeps the dep tree small. Leaning shadcn-svelte for modal + dropdown + toast (the three components I'd hate to write from scratch), vanilla for everything else.

4. **Sealed product contents — hand-curate v1 or wait until "open product" is requested?** Currently planned: wait. But Pokémon pack recipes are well-documented and the data is small (~10 slots × 100 products). Could be a fast win. Probably v2 unless you say otherwise.

5. **Backup story.** A nightly `sqlite3 .backup` on `<user>.sqlite` to a separate disk is the minimum. Anything fancier (encryption, off-host, etc.) is overkill for personal use. Confirm minimum is fine.

6. ~~Deck lifecycle states~~ — **RESOLVED: add a 3-state lifecycle**
   (idea → ready → built). See §7 and decision #29.

---

## 15. Intents corpus — classified

From DeckDumpster's 210 intent files. **Keep** = port directly. **Rename**
= port with name swap (mostly `printing` → `variant`). **Drop** = MTG-specific
or cut feature. **New** = PokeDumpster-specific.

### Keep verbatim (143)

```
batches:    all 9
binders:    all 9
csv:        all 9
edit:       all 5  (edit_order_*)
homepage:   all 6
manual:     all 9
order:      all 10
orders:     all 2
recent:     all 10
recents:    1
sealed:     all 13
set:        all 7  (set_value_*)
views:      1

collection (32 of 35):
  collection_add_from_modal
  collection_add_second_card_no_refresh
  collection_brand_logo_links_home
  collection_card_modal_detail
  collection_header_sticky_on_scroll
  collection_inline_deck_creation
  collection_modal_close
  collection_modal_overlays_sticky_header
  collection_more_menu_in_viewport
  collection_multiselect_delete
  collection_multiselect_delete_individual_copies
  collection_multiselect_individual_copies
  collection_multiselect_new_deck
  collection_multiselect_toggle
  collection_price_chart
  collection_search_autocomplete_keywords
  collection_search_debounced
  collection_search_deck_filter
  collection_search_error_handling
  collection_search_help_page
  collection_search_status_default
  collection_search_unowned
  collection_shared_card_list
  collection_shared_card_list_empty
  collection_skeleton_loading_state
  collection_stats_modal_reflects_filter
  collection_stats_modal_shows_breakdown
  collection_syntax_help_pill
  collection_table_row_opens_modal
  collection_table_scroll_reveals_more_cards
  collection_table_virtual_scroll_renders_cards
  collection_view_toggle
  collection_wishlist_panel
  collection_filter_rarity        (Pokémon rarities)

card_detail (15 of 19, see Rename):
  card_detail_add_copy
  card_detail_add_form_toggle
  card_detail_binder_assign
  card_detail_copy_history
  card_detail_deck_assign
  card_detail_delete_copy
  card_detail_direct_navigation
  card_detail_dispose_copy
  card_detail_dispose_listed_unlist
  card_detail_from_collection_modal
  card_detail_move_deck_to_binder
  card_detail_not_found
  card_detail_price_chart
  card_detail_receive_copy
  card_detail_want_toggle

deck (18 of 26):
  deck_builder_add_and_remove_cards
  deck_builder_create_and_view
  deck_builder_homepage_nav_link
  deck_builder_page_initial_state
  deck_create_redirects_to_detail
  deck_detail_add_cards_from_collection
  deck_detail_add_cards_hides_assigned
  deck_detail_add_individual_copy
  deck_detail_card_links_to_card_page
  deck_detail_card_row_opens_modal
  deck_detail_delete_redirects_to_list
  deck_detail_direct_navigation
  deck_detail_edit_properties
  deck_detail_list_view_no_overflow
  deck_detail_rich_card_table
  deck_detail_select_and_remove_cards
  deck_list_links_to_standalone_detail
  deck_share_button_copies_url

decks (8 of 14):
  decks_create_and_add_cards
  decks_create_minimal
  decks_create_modal_backdrop_close
  decks_create_modal_opens
  decks_create_modal_validation
  decks_delete_keeps_cards
  decks_exclusivity_enforcement
  decks_list_card_content
  decks_list_empty_state
  decks_manage_from_card_modal
```

### Rename (port with semantic adjustment) (6)

```
card_detail_change_printing_picker        → card_detail_change_variant_picker
card_detail_change_printing_execute       → card_detail_change_variant_execute
collection_modal_change_printing          → collection_modal_change_variant
deck_builder_swap_printing_modal          → deck_builder_swap_variant_modal
deck_builder_swap_printing_execution      → deck_builder_swap_variant_execution
deck_builder_swap_unavailable_constructed → deck_builder_swap_unavailable_built
```

### Drop (MTG-specific or cut feature) (35)

```
corners:        all 5  (no image ingestion)
crack:          all 6  (no crack-a-pack)
disambiguate:   all 2  (image-ingest disambiguation)
jumpstart:      all 3  (MTG-specific)
sheets:         all 7  (MTG Print Sheets)
upload:         all 2  (image upload)

card_detail (2 of 19):
  card_detail_dfc_flip            (no DFC in Pokémon)
  card_detail_flip_non_dfc

deck (4 of 26):
  deck_builder_commander_autocomplete         (no commander)
  deck_detail_zone_tab_switching              (no zones)
  deck_edit_change_commander
  deck_edit_clear_commander
  deck_edit_nominate_commander

decks (4 of 14):
  decks_import_expected_and_completeness  (no planning-mode)
  decks_import_moxfield_decklist          (no decklist-import feature — see note)
  decks_precon_origin_metadata            (no precons)
  decks_reassemble_unassigned_cards       (precon-specific)
```

**Note on one borderline drop:**

- `decks_import_moxfield_decklist` is dropped because Moxfield is
  MTG-specific AND we have no decklist-import feature planned. The capability
  ("paste a decklist, create a deck") is generic and could return later
  sourced from a Pokémon decklist format (PTCGL / Limitless export).

(`deck_builder_swap_unavailable_constructed` was a borderline drop in an
earlier revision; with the 3-state deck lifecycle confirmed (§7, decision
#29) it is kept, renamed to `deck_builder_swap_unavailable_built`.)

### New (PokeDumpster-specific) (~25)

```
browse_loads_set_first_page
browse_pagination_next_prev
browse_layout_toggle_9_to_4
browse_include_secret_rares_toggle
browse_include_subset_toggle
browse_include_promos_toggle
browse_master_set_completion_stats_visible
browse_incomplete_only_filter
browse_sort_mode_change                 (set-number vs. rarity-grouped)
browse_slot_renders_image_number_pips
browse_slot_inline_checkbox_reg_rh
browse_slot_inline_checkbox_holo_rh
browse_slot_pips_reflect_ownership
browse_slot_click_opens_variant_modal
browse_variant_modal_lists_all_variants
browse_variant_modal_add_increments_count
browse_variant_modal_remove_decrements_count
browse_variant_modal_edit_copies
browse_optimistic_ui_undo_toast
browse_session_creates_batch
browse_secret_rares_appended_after_numbered
browse_subset_section_appended
browse_mobile_bottom_sheet
browse_mobile_breakpoints
browse_url_state_persists
```

### Tally

- Kept verbatim: **143**
- Renamed (semantic): **6**
- Dropped: **35**
- New (binder browse): **25**
- **Corpus total: ~174** — no waves, no deferrals; grown to full coverage as features ship.

(Some "kept" intents will need fixture-data swaps to use Pokémon cards;
mechanical. A handful that regression-test DeckDumpster-internal refactors —
e.g. `collection_table_renders_after_shared_extraction` — get reframed to the
equivalent SvelteKit concern during `/qa-finish`, not dropped.)

---

## 16. What this plan does NOT cover

- CSS / visual design system specifics (we'll establish during M3 from a
  baseline of shadcn-svelte defaults + a Pokémon-inspired palette).
- Microcopy and empty-state text.
- Error message taxonomy.
- Backup strategy beyond nightly `sqlite3 .backup` of `<user>.sqlite`.
- Internationalization beyond `language` columns existing.
- Telemetry / analytics (none — personal app).
- Performance benchmarks at scale (will measure after M1 seed completes).
- PWA / native mobile.
- Pokémon TCG Live integration.

---

*End of plan. Ready for implementation; revisit Open Questions §14 when starting M1.*
