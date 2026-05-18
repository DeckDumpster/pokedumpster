# PokeDumpster Research Notes

Research compiled before designing PokeDumpster, a Pokémon TCG collection
tracker modeled on **DeckDumpster** (Ryan's MTG collection tool at
`github.com/DeckDumpster/deckdumpster`).

The mandate: **feature-by-feature port of DeckDumpster's collection / sealed
collection capabilities**, swapped from Magic to Pokémon, with image-ingestion
flows replaced by a **binder-page browsing** ingestion model. The novel
**intents-based UI testing framework** is to be carried over wholesale.

---

## 1. DeckDumpster Deep Dive

### 1.1 Stack & shape

| Layer | Choice | Notes |
|---|---|---|
| Language | Python 3.10+ | `uv` for env management — never pip/venv |
| Web server | `http.server` (stdlib) in `crack_pack_server.py` | 7 900 LOC, single threaded HTTP file, SSE for long ops |
| Frontend | Vanilla HTML/JS/CSS, no build step | One `.html` per page in `mtg_collector/static/` |
| Persistence | SQLite (`~/.mtgc/collection.sqlite`) | Plus a shared `shared.sqlite` ATTACHed for read-only catalog tables |
| Card catalog | Scryfall bulk download (3 API calls), MTGJSON for pricing + sealed | Cached locally — *zero runtime network calls* (`architecture/CARD_DATA_ACCESS.md`) |
| Deploy | Rootless Podman Quadlet, per-instance isolation | One image, per-instance volumes / ports / env files |
| Tests | pytest + Playwright + Claude Vision | Layered intents framework — see §1.4 |

Architectural rules baked into `CLAUDE.md`:

- **Store data in the local DB. No website queries at runtime.**
- **No fallback logic.** Errors propagate. No silent defaults, no swallowed exceptions.
- **Tests that demonstrate bugs must fail.** Test passes = bug fixed.
- **Aggressively limit modality.** Defaults are good enough.

### 1.2 Data model (the join chain)

```
cards (oracle_id PK)               — abstract card identity (name, colors, text)
  └─ printings (printing_id PK)    — specific printing (set, CN, art, image, rarity)
       └─ collection (id PK)        — one row per physical card owned
            ├─ orders (id PK)       — purchase order (seller, totals)
            ├─ decks  (id PK)       — named deck
            └─ binders (id PK)      — named binder (mutually exclusive w/ deck)

sets (set_code PK)                 — set metadata + cache freshness
collection_views (id PK)           — saved filter configs
status_log / movement_log          — append-only audit trails
batches                            — unified group across all ingest flows
sealed_products (uuid PK)          — sealed catalog from MTGJSON
sealed_product_cards               — pre-resolved contents (for "open product")
sealed_collection                  — user-owned sealed inventory
sealed_prices                      — time series from TCGCSV
prices                             — card-level time series (MTGJSON-sourced)
latest_prices VIEW                 — most recent observed_at per source/type
```

Key invariants:

- **One physical card = one row** in `collection` (not aggregated by printing).
- **`deck_id` ⊕ `binder_id`** — mutually exclusive; the repo returns HTTP 409 on conflict, and `move_cards()` is the atomic reassign primitive.
- **JSON arrays stored as TEXT** (`colors`, `finishes`, `promo_types`) — Python-side `json.loads`, never SQL array ops.
- **Prices join on `(set_code, collector_number)`, not `printing_id`** — there is no FK from `prices` to `printings`.
- Schema is **versioned (currently v43) with auto-migrations** in `schema.py`. Each migration is a numbered function that runs once.

The `collection_view` and `sealed_collection_view` SQL views denormalize the
chain into a single queryable shape — the UI talks to those views.

### 1.3 Feature surface (what we are porting)

The CLI is *not* the port target — the user explicitly excludes image
ingestion. The web UI feature surface is what matters:

| Page | URL | Role |
|---|---|---|
| Homepage | `/` | Nav hub + global settings (image display, price floor, price source) |
| Collection | `/collection` | Primary browser: table/grid/orders views, Scryfall-style query bar, 13-dim sidebar, multi-select bulk ops, saved views, wishlist panel, "include unowned" mode |
| Card detail | `/card/:set/:cn` | Standalone card page — copy list, price chart, change-printing, deck/binder assign, want toggle, dispose, DFC flip |
| Decks | `/decks` | Deck list with completeness, precon origin tracking, deck builder, zone tabs |
| Binders | `/binders` | Binder list + detail, add cards via picker search, exclusivity with decks |
| Sealed | `/sealed` | Sealed product collection: add, edit, dispose, open product → resolve to cards |
| Recent | `/recent` | Per-batch ingestion timeline with assignment shortcuts |
| Batches | `/batches` | Unified batch view across all ingest flows |
| Orders | `/orders` | Per-seller order history, batch-receive flow |
| Order ingest | `/ingest-order` | Paste TCGPlayer/CK order, resolve, commit |
| Manual ID ingest | `/ingest-ids` | Rarity-letter + CN + set entry (fastest non-photo path) |
| CSV import | `/import-csv` | Moxfield/Archidekt/Deckbox CSV → resolve → commit, with deck assignment |
| Set value | `/set-value` | Per-set analytical view (value, rarity split, owned %) |
| Search help | `/search-help` | Query-language reference |

The user has explicitly said:

- **Skip**: image-based ingestion (corners, OCR, full-card photos, the entire `ingest2/*` API surface, `/upload`, `/ingestor-ocr`, `/ingestor-corners`).
- **Skip**: deck-building / crack-a-pack / explore-sheets / Jumpstart (these are MTG-format-specific concepts that have weak Pokémon analogues).
- **Keep**: collection, binders, sealed collection, orders, manual ingest, CSV import, set value, recent batches, search/filter, saved views, wishlist, card detail page.
- **New**: **binder-page browsing ingestion** — a 3×3 grid showing cards in numerical order for a set; click a slot to register the printing(s) you own.

### 1.4 The intents UI testing framework — five layers

This is the prize asset Ryan wants replicated. It is an unusually clean
layering: each artifact answers exactly one question.

| Layer | Path | Purpose | Who writes it |
|---|---|---|---|
| **1. UX descriptions** | `tests/ui/ux-descriptions/{page}.md` | Structured spec of every interactive element on a page — IDs, types, descriptions, behaviors. Reads like reference documentation. | Hand-written |
| **2. Test plans** | `tests/ui/ux-descriptions/{page}.test-plan.md` | Brainstorm of candidate intents derived from the UX description. | Generated by Claude from the UX doc |
| **3. Approved intents** | `tests/ui/ux-descriptions/{page}.approved-intents.md` | Curated subset of the test plan with justifications for include/defer. Documents *what was deliberately not covered* and why. | Hand-curated (with Claude assist) |
| **4. Intent YAML** | `tests/ui/intents/{name}.yaml` | First-person user-centric description: "I can…", "When I…". Frontmatter includes `related: issues / pull_requests`. | Hand-written |
| **5. Hint YAML** | `tests/ui/hints/{name}.yaml` | `start_page`, `involves` (DOM-level element list), `fixture_data`, step-by-step narrative `notes`. | Hand-written or `/qa-finish` |
| **6. Implementation Python** | `tests/ui/implementations/{name}.py` | A `steps(harness)` function calling Playwright-ish methods on a `ReplayHarness`. | Hand-written or generated |

There are **210 intent files** across **22 feature categories**. Distribution
(top categories):

```
35 collection        13 sealed       9 manual         7 sheets
26 deck              10 recent       9 csv            7 set
19 card              10 order        9 binders        6 homepage
14 decks                              9 batches        6 crack
```

The runtime has **three execution modes**:

1. **Replay mode** (`ReplayHarness`) — deterministic Playwright using stable selectors (text, placeholder, test-id, CSS). Zero LLM calls. Default for every intent with an implementation.
2. **Vision mode** (`UIHarness`) — Claude Vision agent loop. Receives screenshot + numbered element list each turn, chooses one of `navigate / click / fill / done / fail`. Used only when no implementation exists yet.
3. **Generation mode** — runs the vision harness once in recording mode, then emits a deterministic replay script that goes into `implementations/`. CLI: `pytest tests/ui/ --generate <name>` or `--generate-missing`.

On failure, the **resolver** (`tests/ui/resolver.py`) makes a single Claude
call to classify into one of three categories:

- **`test_failure`** — implementation is stale (selector renamed, DOM shifted). Intent still valid. → regenerate.
- **`system_failure`** — feature genuinely broken. → fix the app.
- **`environment_failure`** — container down, missing fixture data. → fix env.

Test data lives in `tests/fixtures/test-cards.sqlite`. The conftest does a
**SQLite `.backup()` snapshot before each test and a `cp` restore after**, so
tests can mutate freely without polluting each other. Both `collection.sqlite`
and a shared `shared.sqlite` are snapshotted.

The `/qa-finish` skill (`.claude/skills/qa-finish/SKILL.md`) is the
end-to-end workflow that turns a freshly shipped feature into intent / hint /
implementation files:

1. Subagent analyzes the diff → proposes 2–5 intents.
2. Deploy a test container, walk the feature with `curl` to harvest selectors.
3. Write intent YAML, hint YAML, hand-written implementation.
4. **Run the tests before teardown** — every test must pass.

Conventions that matter:

- Intents are first-person, observable, behaviorally scoped: *what the user
  sees and does*, not *what the code does*.
- Intents are **immutable once approved**. Implementations get regenerated;
  intents only change if the spec changes.
- Approved-intents docs explicitly **defer** items that overlap existing
  coverage — quality-over-quantity discipline. The `collection` page rejected
  50 proposed intents down to 7+5 because the page was already well-covered.

### 1.5 Sealed product handling

Three tables form the sealed pipeline:

- **`sealed_products`** — catalog from MTGJSON, includes `contents_json` (the
  recipe), `tcgplayer_product_id` for price linking, set FK, category
  (`booster_box`, `bundle`, `collector_booster`, etc.), purchase URLs.
- **`sealed_product_cards`** — pre-resolved contents (which printings live in
  this product). Populated once during MTGJSON import by parsing
  `contents_json`. Powers the "Open Product" flow that bulk-adds cards.
- **`sealed_collection`** — user's owned sealed inventory. Statuses:
  `owned / listed / sold / traded / gifted / opened`. Multiple acquisitions of
  the same product aggregate in the list view but list separately in detail.

Prices flow `TCGCSV → sealed_prices (time series) → latest_sealed_prices VIEW`.

The Pokémon analogue of MTGJSON's sealed catalog is the weakest part of
upstream data — there is no MTGJSON-equivalent canonical sealed catalog for
Pokémon. We'll have to assemble it from TCGCSV groups + manual annotation.
(More in §2.)

### 1.6 Deployment story

Rootless Podman Quadlet, no sudo, one image (`mtgc:latest`) aliased per
instance. Three lifecycle scripts:

- `deploy/seed.sh` — one-time, builds the reusable seed data volume (15–30
  min, runs full `mtg setup`).
- `deploy/setup.sh <instance> --init` — clones seed volume, creates env file,
  registers Quadlet unit, picks a port.
- `deploy/setup.sh <instance> --test` — uses a pre-baked fixture DB (~27 MB)
  for instant startup with no network.
- `deploy/deploy.sh <instance>` — rebuild image + restart.
- `deploy/teardown.sh <instance> [--purge]` — stop + remove (volume kept unless `--purge`).

CI auto-deploys `prod` on push to main. Other instances via workflow dispatch.
Per-instance env files at `~/.config/mtgc/<instance>.env`, Quadlet units at
`~/.config/containers/systemd/mtgc-<instance>.container`.

For PokeDumpster, this whole apparatus should port over with only string
renames.

---

## 2. Pokémon TCG Data Sources

### 2.1 TL;DR

Mirror the MTG pattern with a **three-source stack**:

1. **pokemontcg.io v2 API** — the Scryfall analog. Live, per-card lookups. Returns **both TCGplayer USD and Cardmarket EUR pricing on the same response**. Still operational despite being absorbed under the Scrydex brand. Free, 1k req/day anon or 20k/day with key.
2. **`PokemonTCG/pokemon-tcg-data` GitHub repo** — the MTGJSON `AllPrintings.json` analog. One JSON file per set, identical schema to the API minus prices. Gotcha: lags the live API by 2–3 months on newest sets.
3. **TCGCSV** (`tcgcsv.com/tcgplayer/3/`) — the daily TCGplayer price bulk dump. No auth, no rate limit, updated 20:00 UTC daily. The freshest set-metadata source — has new groups before the API does. Pairs cleanly with the MTGJSON-equivalent for spot pricing.

Reserved upgrades: **Scrydex** (paid, $29+/mo) for eBay-sold/PSA data + true price history; **TCGdex** (open-source, multilingual, GraphQL) for Japanese and OCG sets.

**Do not build against**: TCGplayer's direct affiliate API (closed to new applicants since late 2024 post-eBay acquisition), Cardmarket API (not accepting new applications, mid-2026 v1→v2 migration deadline). Both are already piped through pokemontcg.io for free anyway.

### 2.2 Sample card response (pokemontcg.io v2, Charizard ex 6/165 from "151")

```json
{
  "id": "sv3pt5-6",
  "name": "Charizard ex",
  "rarity": "Double Rare",
  "set": { "id": "sv3pt5", "name": "151", "ptcgoCode": "MEW", "releaseDate": "2023/09/22" },
  "number": "6",
  "images": {
    "small": "https://images.pokemontcg.io/sv3pt5/6.png",
    "large": "https://images.pokemontcg.io/sv3pt5/6_hires.png"
  },
  "tcgplayer": {
    "updatedAt": "2026/05/17",
    "prices": {
      "holofoil": { "low":4.98, "mid":10.33, "high":87.18, "market":10.45, "directLow":14.95 }
    }
  },
  "cardmarket": {
    "updatedAt": "2026/03/11",
    "prices": {
      "averageSellPrice":6.88, "lowPrice":3.49, "trendPrice":7.30, "avg30":6.85,
      "reverseHoloTrend":0.0
    }
  }
}
```

### 2.3 Source comparison

| Source | Live API | Bulk dump | TCGplayer USD | Cardmarket EUR | Price history | Images | Rate limits | License | Notes |
|---|---|---|---|---|---|---|---|---|---|
| pokemontcg.io v2 | ✅ | via GitHub | ✅ | ✅ | ❌ (avg7/30 only) | 245/734-wide CDN | 1k/d anon, 20k/d key | Free | Best default |
| pokemon-tcg-data (GH) | n/a | ✅ | ❌ | ❌ | n/a | n/a | n/a | Permissive | Set-by-set JSON; lags live |
| TCGdex | ✅ + GraphQL | ✅ | ⚠ often null | ✅ | ❌ (avg7/30) | 828KB hi-res | None advertised | MIT | Multilingual, JP+OCG; explicit `variants` boolean map |
| TCGCSV | n/a | ✅ daily | ✅ | ❌ | snapshot daily yourself | n/a | None | Free | Freshest set list; CSV+JSON |
| Scrydex | ✅ | ❌ | ✅ | ✅ | ✅ | yes | tier-gated | $29+/mo | eBay-sold + PSA prices |
| PokemonPriceTracker | ✅ | ❌ | ✅ | ⚠ | ✅ (12mo+) | yes | tier-gated | paid | Backup for graded data |
| JustTCG | ✅ | ❌ | ✅ NM/LP/MP/HP | ❌ | partial | yes | tier-gated | paid | Condition-split spot prices |
| TCGplayer direct | n/a | n/a | n/a | n/a | n/a | n/a | n/a | closed | Avoid |
| Cardmarket direct | n/a | n/a | n/a | n/a | n/a | n/a | n/a | closed | Avoid |

### 2.4 Critical answers

- **Variants**: TCGdex has the cleanest explicit variant map; pokemontcg.io is richer on rarity strings ("Hyper Rare", "Special Illustration Rare", "Trainer Gallery Rare Holo") and splits variants implicitly via price keys (`normal`/`holofoil`/`reverseHolofoil`/`1stEditionHolofoil`). For PokeDumpster's needs, pokemontcg.io's rarity + price-key split is sufficient — but we must **expand** those keys into separate `printing` rows ourselves.
- **Collector-friendly set codes**: TCGCSV `abbreviation` (`OBF`, `PAF`, `MEG`, `POR`) is canonical. pokemontcg.io exposes the same as `set.ptcgoCode`. **Internally we store the pokemontcg.io set id (`sv3pt5`, `swsh12pt5gg`), externally we display the `ptcgoCode`** — same pattern DeckDumpster uses with Scryfall set codes.
- **Price history**: nobody free, so we **roll our own** by snapshotting TCGCSV daily into a `prices` time series (same shape as DeckDumpster). One file per set per day, cheap.
- **Images**: `images.pokemontcg.io/<setid>/<num>_hires.png` (~690KB) is fine for our 3×3 grid. Cloudflare CDN with year-long `max-age` — we don't need to mirror.
- **AllPrintings equivalent**: `PokemonTCG/pokemon-tcg-data` GitHub repo. Use it for the initial seed; fall through to live API for the 2–3 month tail of newest sets.
- **Unified TCGplayer + Cardmarket**: yes — pokemontcg.io ships both blocks on every card response. No two-source plumbing needed for v1.

### 2.5 Sealed product data

This is the **weakest area of upstream Pokémon data**. There is no MTGJSON-style canonical sealed catalog. The practical assembly:

- **TCGCSV groups** → each group has a `products` listing that includes booster boxes, ETBs, bundles, tins, premium collections, special collections. Use this as the catalog.
- **No `contents_json` equivalent** — we cannot pre-resolve sealed products to card lists the way DeckDumpster does. The "Open Product" flow that bulk-adds pulled cards is therefore harder; we'd have to either (a) ask the user to manually log pulls, or (b) maintain a hand-curated `sealed_product_contents` table for at least booster contents (per-pack pull rates: 10 cards/pack, X commons, X uncommons, X rares + 1 reverse holo + ~1:Y ultra rare).
- **TCGCSV `ProductsAndPrices.csv`** gives spot pricing for sealed identically to single cards — slot it into the same `sealed_prices` time series.

### 2.6 Recommended data pipeline

```
Initial seed (one-time):
  1. Clone PokemonTCG/pokemon-tcg-data → seed every card + set into local DB
  2. Hit live pokemontcg.io for sets/cards newer than the repo's last commit

Daily refresh (cron, 22:00 UTC after TCGCSV's 20:00 publish):
  3. Download TCGCSV /tcgplayer/3/groups → detect new groupIds
  4. For each new groupId, ProductsAndPrices.csv → upsert products
  5. For all groups, /prices → snapshot into prices time series
  6. Weekly: pokemontcg.io /cards?q=set.id:<newest> → backfill rich metadata

Images: hotlink from images.pokemontcg.io (CDN). No mirror in v1.

Upgrades held in reserve:
  - Scrydex when graded comps / true price history matter
  - TCGdex when multilingual / JP coverage matters
```

---

## 3. pkmn.gg Feature Inventory

### 3.1 TL;DR

pkmn.gg is a **web-first** Pokémon TCG collection tracker. Strong at *set completion* and *variant tracking*; weak at *portfolio finance*, *physical-inventory realism*, *sealed product*, *import/export*, and *scanning*. Aggressively paywalled basics (sort-by-price, sort-by-rarity, more than 1 list, more than 1 deck). $5/mo or $50/yr for Pro.

### 3.2 What pkmn.gg models per card

- Card identity (English / Japanese / TCG Pocket).
- Variant (one row per printing — Normal/Holo, Reverse Holo, Pokémon Center Stamp, prerelease stamp, etc.); a Pro "stack/unstack" toggle controls display.
- Quantity (unlimited even on free tier).
- **Graded copies** (Pro): grading company, grade, cert #, URL, **manually entered value**.
- **Private notes** (Pro).

What pkmn.gg **does not** track:

- ❌ Condition (NM/LP/MP/HP/DMG) for raw cards — everything is implicitly NM.
- ❌ Acquisition date.
- ❌ Acquisition price / cost basis / purchase source.
- ❌ Sealed product (no booster boxes, ETBs, bundles, tins, collection boxes).
- ❌ CSV import (no ManaBox, no TCGplayer mass entry).
- ❌ Camera scanner.

### 3.3 Killer features to copy

1. **Dual master-set progress bars** — *Lite* (one of each card, any variant) and *True Master* (every variant including stamps). The single most-praised UX in pkmn.gg.
2. **Variant-as-first-class-citizen** with stack/unstack toggle.
3. **Grid + Table** views, with table optimized for "I just opened 10 packs, mark them fast."
4. **Binder View** (Pro) — page/slot calculation across 4/9/12/16-pocket layouts, drag-to-reorder on Lists to mirror physical binders.
5. **Per-variant price history** at 30D/3M/6M/1Y, plus **portfolio Value History**.
6. **Dynamic vs. Static lists** as an explicit concept — clean mental model.
7. **Multi-currency display** (TCGplayer USD → 10+ currencies).
8. **Gamification (Pokédex / Trainer Level)** — collecting cards captures Pokémon; stacking copies levels them up; cross threshold → shiny variant unlocks.
9. **Stream / overlay tools** for content creators.

### 3.4 Gaps to exploit (where PokeDumpster wins)

| Gap | DeckDumpster already does this? | PokeDumpster win |
|---|---|---|
| Condition tracking for raw cards | ✅ — NM/LP/MP/HP/DMG | Inherit verbatim |
| Acquisition price + date per copy | ✅ — `purchase_price`, `acquired_at` | Inherit verbatim |
| Purchase order tracking | ✅ — orders table, vendor/seller/totals | Inherit verbatim |
| Sealed product collection | ✅ — sealed_products + sealed_collection + sealed_prices | Inherit; sealed catalog from TCGCSV |
| CSV import | ✅ — Moxfield/Archidekt/Deckbox | Retarget — ManaBox, TCGplayer order CSV, pkmn.gg export, Collectr export |
| CSV export | ✅ — same | Retarget — round-trip ManaBox format |
| Per-printing audit trail | ✅ — status_log, movement_log | Inherit verbatim |
| Wishlist (oracle or printing-specific, w/ max price) | ✅ | Inherit; *also* expose dynamic-list pattern (pkmn.gg's model) for set-completion wants |
| Multi-select bulk ops | ✅ | Inherit verbatim |
| Saved filter views | ✅ — collection_views | Inherit verbatim |

### 3.5 Pricing model implication for PokeDumpster

Don't paywall. PokeDumpster is single-user (Ryan), so the question doesn't arise. But the *design tension* pkmn.gg illustrates — between "completion-focused collector" and "portfolio-focused investor" — is real. DeckDumpster already serves both: completion via wishlist + "include unowned"; portfolio via prices + per-copy purchase_price. We just need to preserve that duality.

---

## 4. Binder-View Ingestion UX

### 4.1 How sets actually paginate

There is **no canonical binder order** — the "Perfect Order Master Set" community (PokéCottage, Binder Forge, Cardrake) has roughly converged on three layouts:

1. **Set-number order, RH inline** — `001 regular | 001 reverse holo | 002 regular | 002 reverse holo | ...`. Most popular "Perfect Order" arrangement.
2. **Set-number order, RH stacked** — one slot per card number, reverse holo lives in the same pocket behind the regular. Saves pages; hides cards.
3. **Rarity-grouped** — Common → Uncommon → Rare → Holo Rare → Ultra Rare → IR → SIR → Hyper → Subset → Promos.

**Universal conventions**:

- Secret rares (card numbers `> setTotal`, e.g. `184/165`) always go *after* the numbered set.
- Subsets (Galarian Gallery `GG##`, Trainer Gallery `TG##`) have their own namespace, dedicated section after secrets.
- Black Star Promos (`SWSH###`, `SVP ###`) live in their own promo binder, never interleaved.
- 151 / Prismatic Evolutions Master Ball / Poké Ball reverse holos are treated as a *third* parallel printing alongside regular + standard RH.

**Default for PokeDumpster**: set-number order, RH **inline as separate slots**. Secret rares and subsets render as automatic "appended pages." Promos toggleable.

### 4.2 Variant catalog (SWSH → SV → Mega Evolution, May 2026)

Variants are a **flat enum** tagged with display metadata, NOT nested:

| Variant code | Display | Notes |
|---|---|---|
| `normal` | Regular / Non-holo | Common/Uncommon/basic Rare |
| `holo` | Holo Rare | Holo window on a numbered Rare |
| `reverse_holo` | Reverse Holo | Inverted holo; exists for most C/U/R |
| `pokeball_rh` | Poké Ball pattern RH | 151, Prismatic, Mega Evolution |
| `masterball_rh` | Master Ball pattern RH | 151, Prismatic; ~1:booster-box pull rate |
| `cosmos_holo` | Cosmos Holo | Bundle-exclusive holo pattern |
| `double_rare` | Double Rare (ex) | SV+, black ★★ |
| `ultra_rare` | Ultra Rare / Full Art | All modern, silver ★★ |
| `illustration_rare` | Illustration Rare (IR) | SV+, gold ★, full-art Pokémon |
| `special_illustration_rare` | Special IR (SIR) | SV+, gold ★★, full-art ex |
| `hyper_rare` | Hyper Rare / Gold | All modern, gold ★★★ |
| `rainbow_rare` | Rainbow Rare | SWSH (replaced by SIR in SV) |
| `shiny_rare` | Shiny Rare | SV, shiny baby symbol |
| `ace_spec` | ACE SPEC | SV (returning) |
| `mega_attack_rare` | Mega Attack Rare (MAR) | Mega Evolution era |
| `mega_hyper_rare` | Mega Hyper Rare (MHR) | Mega Evolution, ~1:1260 packs |
| `galarian_gallery` / `trainer_gallery` | Subset | Has own `GG##` / `TG##` number |
| `promo_blackstar` | Black Star Promo | `SWSH###` / `SVP ###` |
| `stamp_prerelease` | Prerelease stamp | Silver stamp |
| `stamp_buildbattle` | Build & Battle stamp | SV+ |
| `stamp_pokemoncenter` | Pokémon Center stamp | PC logo |
| `stamp_staff` | Staff stamp | Tournament, gold/silver |

**Critical data-model point**: rarity (what the card *is*) and printing/finish (how *this copy* was printed) are orthogonal. A single card number can have 4+ legitimate printings (`normal` + `reverse_holo` + `pokeball_rh` + `masterball_rh`). Both axes must be representable.

### 4.3 Resolved binder UX

**One slot per card number. Stacked variants. Secret rares appended. Click-to-modal with inline shortcuts for the common variant pair.**

```
┌─ 151 ──────  Base 158/165 96% ●●●●●●●●○  Master 412/487 85% ●●●●●●○○○  ─┐
│                                                         [Incomplete only ☐]│
│                                                                            │
│   Page 3 of 19                                       ← prev    next →     │
│                                                                            │
│   ┌────────────┐  ┌────────────┐  ┌────────────┐                          │
│   │  [image]   │  │  [image]   │  │  [image]   │                          │
│   │            │  │            │  │            │                          │
│   ├────────────┤  ├────────────┤  ├────────────┤                          │
│   │ 019 ☑ ☑    │  │ 020 ☑ ☐    │  │ 021 ☐ ☐    │                          │
│   │ ●●○○       │  │ ●○○○       │  │ ○○○○       │                          │
│   └────────────┘  └────────────┘  └────────────┘                          │
│   ...                                                                      │
│                                                                            │
└────────────────────────────────────────────────────────────────────────────┘
```

Each slot:

- **The image** is the canonical base printing for that card number.
- **Card number** in the slot footer.
- **Inline quick-toggle checkboxes** for the two most common variants for *this* card's rarity:
  - Common/Uncommon/Rare → `Reg` + `RH`
  - Holo Rare → `Holo` + `RH`
  - Ultra Rare / IR / SIR / Hyper Rare → no inline checkboxes (open modal)
- **Pips** below the checkboxes — one dot per *known* variant of this card number, filled if owned. Reflects ALL variants, not just the inline pair.

The pips + checkboxes are intentionally orthogonal:

- Checkboxes give one-click access to the 80% case.
- Pips give "how complete am I across all variants of this number, including fancy ones."

**Clicking the slot** (not the checkboxes) opens a **modal** with the full variant picker — image per variant, market price, owned count with +/-, and per-copy controls (condition, language, notes). pkmn.gg's pattern; user is already used to it.

```
┌── #024 Ninetales ────────────────────────────────[×]──┐
│                                                        │
│  Normal              ●  1 owned  [+] [-]  $0.50 mkt   │
│  Reverse Holo        ○  0 owned  [+] [-]  $1.20 mkt   │
│  Poké Ball RH        ○  0 owned  [+] [-]  $3.50 mkt   │
│  Master Ball RH      ○  0 owned  [+] [-]  $42.00 mkt  │
│  Special Illustration  ○  0 owned  [+] [-]  $85.00 mkt│
│  Prerelease stamp    ○  0 owned  [+] [-]  $4.00 mkt   │
│                                                        │
│  [Edit copies...]   [Open full card detail page →]    │
└────────────────────────────────────────────────────────┘
```

**Master Set semantics** (in a stacked layout):

- *Base set* completion = `card-numbers where ≥1 variant owned / total numbered cards`.
- *Master set* completion = `variants owned / total known variants (including secret rares, subsets, promos tied to this set)`.
- Both shown side-by-side at the top of the page.
- **`Incomplete only`** filter pill hides any slot where the pips row is full.

**Mobile**: 3 cols → 2 cols below 768px → 1 col below 480px. Inline checkboxes always visible (touchscreen-friendly tap targets). Variant modal becomes a bottom sheet at ~70% viewport height.

### 4.4 Image sizing math

| Layout | Slot size | Asset |
|---|---|---|
| Desktop, 1200–1600px container, 3 col | ~380–510px wide × 530–715px tall | `large` (`_hires.png`, ~734×1024) — `small` looks blurry at 2× DPR |
| Mobile, 360–430px viewport, 3 col | ~110–140px wide × 154–196px tall | `large` and let browser downscale; consider `srcset` w/ CDN medium later |

Hidden gotcha: many alt arts / SIRs are released with only `small` images for several weeks after a set drops. Our fetcher needs `last_checked_hires_at` and a backfill job.

### 4.5 Non-obvious gotchas

1. A single card number can have 4+ printings — variants must be **child rows**, not enum columns.
2. **Reverse holo set is NOT a strict subset of the base set** — Holo Rares and ex cards usually do *not* have reverse holo printings. Expected-printings map must be data-driven.
3. Subsets (`GG##`, `TG##`) are not separate sets in any API — tagged via `subtypes` or buried in rarity. Need a normalization layer.
4. Promos are temporally orphaned — a "151 promo" might ship 6 months later in a Pokémon Center ETB. Set-tying is editorial, not in API.
5. Hot-loading 9× ~400KB PNGs per page flip is slow → lazy-load adjacent pages, preload N+1 on idle, consider CDN WebP.
6. Master Ball reverse holos exist in English **sometimes** — 151 Master Balls were Japan-only; Prismatic Evolutions added them to English. Printing model needs language-scoped existence.
7. Mega Evolution era (Sept 2025+) re-introduced Mirror Holofoil for trainers — code assuming "trainers don't have ultras" is wrong now.
8. Stamped variants share artwork with their base printing — use the same image with an overlay badge, don't fetch duplicates.
9. "Master Set" definition drifts — when a new product drops a Pokémon Center stamp reprint, the master-set checklist grows. Version master-set definitions and notify on changes.

---

## 5. Feature Translation Table — DeckDumpster → PokeDumpster

Quick reference. Detailed plan lives in PLAN.md.

| DeckDumpster | PokeDumpster | Notes |
|---|---|---|
| `cards` (oracle_id from Scryfall) | `cards` (id from canonical Pokémon source) | "Oracle" doesn't map cleanly — Pokémon cards are identified by `set + number + variant`. May fold `cards` and `printings` into one table. |
| `printings` (set_code + CN, finishes JSON) | `printings` (set_code + CN + variant) | Pokémon "variant" (reverse holo, holo, full art, alt art, secret rare, promo stamp) is closer to "treatment" than to MTG's `finishes`. |
| `cards.colors`, `mana_cost`, `cmc` | `cards.types`, `hp`, `weakness`, `attacks`, `abilities` | Different domain — strip MTG-specific, add Pokémon-specific. |
| Scryfall bulk | Pokémon TCG API bulk (or TCGdex) | See §2 |
| MTGJSON prices | TCGCSV (TCGplayer prices) | TCGCSV is the canonical mirror. |
| MTGJSON sealed | TCGCSV groups + hand-curated mapping | No MTGJSON equivalent for Pokémon. |
| Crack-a-pack | **Drop** | No equivalent UX motivation for Pokémon (no booster sim culture as strong). |
| Explore sheets | **Drop** | MTG-specific (Print Sheets). |
| Decks (mainboard/sideboard/commander) | **Drop** | Skip; could come later if Ryan plays competitive. |
| Jumpstart | **Drop** | MTG-specific. |
| Corner/OCR ingestion | **Drop** | Per Ryan. |
| TCGPlayer/CK order import | **Keep, retarget** | TCGplayer order parser may largely carry over (same vendor). CK becomes irrelevant. Add: Pokémon Center order import? eBay order import? |
| Manual ID entry | **Keep** | Format: `set + collector_number + variant` instead of `rarity + CN + set + foil`. |
| CSV import (Moxfield/Archidekt/Deckbox) | CSV import (pkmn.gg / Collectr / TCG Collector / ManaBox export formats) | See §3 |
| Binders | **Promote to primary ingestion surface** | Per Ryan — binder page IS the ingest UI for Pokémon. |
| Sealed collection | **Keep** | ETBs, booster boxes, Pokémon Center exclusives, tins, etc. |
| Set value page | **Keep** | High-utility view. |
| Recent batches | **Keep** | Carries over verbatim. |
| Saved views, wishlist, multi-select | **Keep** | All carry over. |
| Intents framework | **Keep, port wholesale** | The whole `tests/ui/` tree, including `/qa-finish` skill. Re-grow the intent corpus from zero. |

---

*This document will be updated as the background research agents return.*
