# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->


## Project Overview

PokeDumpster is a single-user Pokémon TCG collection tracker — a Rust
rebuild of DeckDumpster's collection feature set, against pokemontcg.io +
TCGCSV instead of Scryfall + MTGJSON. The headline feature is **binder-page
browsing**: sets render as a grid of clickable card slots; each owned
printing is registered as its own row, no quantity aggregation.

`PLAN.md` is the frozen v1 design record (see its banner); `RESEARCH.md`
is the research it rests on. Living truth = beads + the code + this file.

The app is live: backend, frontend, ingest pipeline, and a
rootless-Podman + systemd-user production deployment are all in place.
The user runs it on a self-hosted box and reaches it over WireGuard.

## Build & Test

```bash
# Backend — Rust workspace
cargo build                      # build all crates
cargo test                       # run all tests; also regenerates the TypeScript
                                 #   types in frontend/src/lib/types/ via ts-rs
cargo test -p pkdump-db          # test a single crate
cargo clippy --all-targets       # lint (must be clean before commit)
cargo fmt                        # format

# CLI / server
cargo run --bin pkdump -- setup  # build the shared catalog (downloads upstream)
cargo run --bin pkdump -- serve  # start the HTTP server
cargo run --bin pkdump -- data refresh   # incremental catalog refresh
cargo run --bin pkdump -- seed-fixture   # build the deterministic UI-test fixture

# Portable collection backup — every user table in one versioned JSON
# envelope. A whole-database restore, not a merge (--force to overwrite).
cargo run --bin pkdump -- export --json -o collection.json
cargo run --bin pkdump -- import --json collection.json

# Frontend — SvelteKit (Svelte 5, vite, adapter-static)
cd frontend && npm install && npm run build
cd frontend && npm run check     # svelte-check / TypeScript
cd frontend && npm test          # design-token gates (WCAG AA contrast, layer
                                 #   split, raw-colour ratchet)

# Visual regression — every route at 1440 and 768 against a throwaway
# container instance. A pixel diff fails; approving one is explicit.
bash tests/visual/run.sh         # check
bash tests/visual/run.sh --update  # approve — see tests/visual/README.md

# UI test harness — Playwright + intent YAMLs              (in progress)
cd tests/ui && npm test
```

`$PKDUMP_HOME` overrides the data dir (default `~/.pkdump/`).
`$PKDUMP_USER` overrides the active tenant (default `collection`).

## Architecture Overview

Cargo workspace, five crates (`crates/`):

- **pkdump-core** — domain types + pure logic (variant code parsing,
  override matching, import format adapters). No IO.
- **pkdump-db** — rusqlite persistence. Owns the shared/user DB split,
  the schema files (`src/schema_{shared,user}.sql`, re-applied idempotently
  on every open — single-instance project, no migration history), the
  variants display table, and the binder-page query.
- **pkdump-ingest** — upstream catalog ingestion (pokemon-tcg-data,
  pokemontcg.io, TCGCSV). Variant expansion lives here, as does the
  Pokémon Japan pipeline (`japan.rs`). Touched only by
  `pkdump setup` / `pkdump data refresh` — never at request time.
- **pkdump-server** — Axum HTTP app; JSON API under `/api` + serves the
  SvelteKit static build. One route module per resource
  (`routes/{sets,card,collection,binders,decks,sealed,wishlist,orders,batches,import,export,variants}.rs`).
- **pkdump-cli** — the `pkdump` binary; clap command tree
  (`setup`, `data`, `serve`, `seed-fixture`, `db`, `tenant`, `export`,
  `import`).

Frontend: SvelteKit (Svelte 5 runes mode) in `frontend/`, built static,
served by `pkdump-server`. Generated TypeScript bindings live under
`frontend/src/lib/types/` and are regenerated by `cargo test` via ts-rs.

Two SQLite databases under the data dir:

- **shared.sqlite** — immutable card catalog, fully reproducible from
  upstream. Rebuilt by `pkdump setup`. Opens with tuned PRAGMAs
  (WAL + `synchronous=NORMAL` + 64MB cache) so variant expansion stays
  fast (see `crates/pkdump-db/src/connection.rs`).
- **tenants/&lt;tenant&gt;.sqlite** (default `tenants/collection.sqlite`) — one
  mutable collection per tenant. The only thing worth backing up; replicated
  off-box to S3 by the Litestream sidecar (6-month PITR — see
  `deploy/RESTORE.md`).

At runtime the user DB `ATTACH`es the shared DB read-only and exposes
catalog tables through TEMP VIEWs so queries can join unqualified. That
ATTACH is identical for every tenant — the catalog is ONE copy on disk,
joined per query, never denormalised per tenant.

Provisioning is `pkdump tenant create <name>` / `pkdump tenant remove
<name> --yes`; `pkdump tenant adopt` migrates a pre-`tenants/` data dir and
`revert` rolls it back. Layout rationale + the production migration runbook:
`deploy/TENANTS.md`.

Tenant *resolution* lives in `pkdump-server/src/tenant.rs` and is **off by
default**: `pkdump serve` opens the one collection named by `$PKDUMP_USER`
and does not read the tenant header at all. `--multi-tenant` (or
`PKDUMP_MULTITENANT=1`) switches on per-request resolution from the
`x-pkdump-tenant` header. **Nothing authenticates that header** — identity is
a separate epic — so the flag must stay off in production. Isolation is
structural: `AppState` holds no connection, `blocking()` takes the tenant from
the request scope, and one connection per tenant is opened against that
tenant's own file. See `deploy/TENANTS.md`.

## Deployment

Rootless Podman + systemd-user, scripts in `deploy/`:

```bash
deploy/setup.sh prod        # one-time: build image, install Quadlet unit + timers
deploy/seed.sh prod         # populate the catalog (pkdump setup in a one-off container)
deploy/deploy.sh prod       # rebuild + restart
deploy/restore-litestream.sh prod   # restore the collection from S3 (see deploy/RESTORE.md)
deploy/teardown.sh prod [--purge]
```

The image runs `pkdump serve`; the data volume holds both `shared.sqlite`
and the per-user collection DB. Off-box backup is the
`pkdump-litestream-<instance>` sidecar (continuous S3 replication, 6-month PITR;
no local snapshots). Nightly catalog refresh comes from
`pkdump-refresh@<instance>.timer`.

There's a `deploy` skill in this repo that wraps these scripts for AI use.

## Conventions & Patterns

### The data model IS the product

**Anything data-shaped living in API or frontend code is a smell. Flag it
immediately.** Lookups, labels, ranks, display metadata, enum-like
typologies — all of it belongs in a table or seed file, never as a switch
statement in TypeScript or a Rust constant. The frontend should do nothing
but render; parsing of upstream strings lives in the ingest pipeline.

Concrete examples already in place:
- `data/variants.json` seeds the `variants` table at `pkdump setup` time;
  every `printings.variant` code has a row there with label, short tag,
  sort rank, and chip color. The four variant-display TS helpers
  (`variantLabel/Rank/Color/Tag`) are pure map lookups.
- `data/overrides/variant_augmentations.json` is the hand-curated patch
  layer applied as the last phase of variant expansion.

When you find logic that should be data, file a `bd create
--type=decision` issue and propose the schema before writing more code
against it.

### Card data access

All runtime card lookups read the local DB. The upstream APIs are touched
only by `pkdump setup` / `pkdump data refresh`. See
`architecture/CARD_DATA_ACCESS.md`.

### Variant expansion

`pkdump-ingest/src/overrides.rs::expand_all_printings` is the per-card
loop. It:

1. Pulls every TCGCSV product for the card's group (`variants_from_tcgcsv`).
2. Scans the MCAP catch-all group (2374) for cross-group products that
   resolve to this card via stamp tag *or* non-stamp pattern overlay
   (`preload_cross_group_products`, e.g. "Erika's Tangela (Cosmo Holo)").
3. Bulk-ensures every variant code that will be written has a row in the
   `variants` table (the per-(card,variant) `ensure_code` call is gone —
   it was the cliff source). Single bulk pass at function entry.
4. Inserts one printing row per resolved (variant, sub_type,
   tcgplayer_product_id).

Soft-deprecates dropped variants (sets `deprecated_at`); the UI still
shows them when the user owns one, dimmed.

Long Rust loops that write to disk MUST flush stdout per progress line —
default block-buffering hides multi-minute progress behind `tee`. See the
`PROGRESS_EVERY` block in `expand_all_printings`.

### New-set discovery

pokemontcg.io publishes new sets weeks-to-months late and goes down for
days; TCGCSV has the group the day the set lists. `pkdump-ingest/src/
set_discovery.rs` closes that gap: after the TCGCSV import, a group that
bridges to no set, whose name carries a numbered era prefix ("ME05: Pitch
Black"), and that has enough distinct collector numbers gets a
synthesized `sets` row + cards, so the binder is browseable that night.
Policy (product floor, denylist, era→series overrides) lives in
`data/overrides/tcgcsv_set_discovery.json`.

Derived set codes follow pokemontcg.io's convention (`ME05` → `me5`), so
upstream's eventual publish lands on the same row and supersedes the
synthesized data — `import_tail` treats a set row with NULL
`ptcgio_fetched_at` as not-yet-imported for exactly that reason. Sets in
that state carry `sets.discovered_from_group_id` and surface as
`SetSummary.synthesized`, which badges the `/browse` tile.

Groups the rule deliberately misses — unnumbered specials ("SV: Black
Bolt"), energy umbrellas, promo catch-alls — still take a hand-authored
entry in `data/overrides/tcgcsv_set_bridges.json`.

Discovery must run *after* the Japanese import: every category-85 group
is bridged to a `jp-` set, which is what keeps 450 Japanese groups out of
the unbridged pool discovery works from.

### The Japanese catalog

`pkdump-ingest/src/japan.rs` owns TCGCSV categoryId 85 (Pokémon Japan),
which has no pokemontcg.io counterpart — every Japanese set and card is
synthesized from TCGCSV alone. Rules that keep it from colliding with the
English catalog (category 3):

- **`set_code` is `jp-<tcgcsv_group_id>`.** Abbreviations are empty on
  ~40% of the 450 Japanese groups and duplicated across the rest.
- **Japanese groups never run through `tcgcsv::import_groups`.** Its
  abbreviation/name auto-linker would let JP "Pokemon Jungle" claim the
  English `base2` and "SV2a: Pokemon Card 151" claim `sv3pt5`.
- **Cards are discriminated by `CardType`, not `Number`.** ~2.4k vintage
  Japanese products (Mystery of the Fossils, City Gym Decks, …) carry no
  collector number; those take the synthetic number `p<product_id>`.
  Do not invent a 1..N sequence — TCGCSV lists those groups
  alphabetically, not in set order.
- Series buckets come from `data/japan_series.json` (era date ranges),
  never from a match arm.

Everything downstream is shared: JP products land in the same
`tcgcsv_products` / `prices` tables, so `import_prices`,
`expand_all_printings`, and `latest_prices` need no Japanese special case.
The exception is `set_discovery::series_from_sibling_group`, which reads a
new set's series off a same-era sibling group and has to skip Japanese
ones — JP names collide hard on the era pattern ("SV11B: Black Bolt",
"BW9: Megalo Cannon"), and a JP sibling would hand an English set a
"Pokémon JP — …" series.

### Other patterns

- **No fallback logic.** Errors propagate. No silent defaults, no
  swallowed exceptions, as few error paths as possible — let it crash
  visibly.
- **Strict one row per physical card** in `collection`; no quantity
  aggregation. Each copy carries its own condition / status / batch /
  binder/deck assignment.
- **Edition 2024**, toolchain pinned in `rust-toolchain.toml`. `cargo
  fmt` and `cargo clippy` clean before every commit.
- **Schema** — single-instance project (pokedumpster-luo). The full
  schema for each database lives in `crates/pkdump-db/src/schema_{shared,user}.sql`
  and is re-applied with `CREATE … IF NOT EXISTS` on every open. No
  migration history, no refinery. Schema changes: edit the file + apply
  the diff manually to the one prod box (`podman exec` + `sqlite3`).
- **Workspace dependencies** are declared in the root `Cargo.toml`
  `[workspace.dependencies]`; crates opt in with `dep.workspace = true`.
- **Tests that demonstrate bugs must fail** until the bug is fixed.
  Add a fail-first test in the same commit as the fix.
- **Decisions** — significant architectural decisions become
  `bd create --type=decision` issues. `PLAN.md` is frozen; do not edit
  it per-task.
- **Checkpoints** — commit after every closed beads task; reference the
  issue id (`Closes pokedumpster-xxx`) in the commit message. Short
  prefix per type: `M-feat:`, `M-bug:`, `M-fix:`, `M-UX:`, `M-perf:`.
- **Frontend reactive state** — Svelte 5 runes. Shared reactive state
  lives in `*.svelte.ts` files (e.g. `$lib/breadcrumbs.svelte.ts`,
  `$lib/variants.svelte.ts`) as classes with `$state` fields. Layout
  `+layout.ts` can `await` async setup before child pages render.

### Frontend conventions

- Generated TypeScript types live under `frontend/src/lib/types/` and are
  produced by `cargo test` via ts-rs `#[ts(export)]` — never hand-edit.
- API calls go through the typed `api` object in `frontend/src/lib/api.ts`.
- Variant display metadata is read from `$lib/variants.svelte`, never
  recomputed in components.
- Per-page leaf labels (e.g. set name in the breadcrumb) are pushed into
  `$lib/breadcrumbs.svelte` from the page's `$effect`.

### Design tokens

`frontend/src/lib/styles/tokens.css` is the only file in `frontend/src` that
may contain a raw colour literal. It is imported once, from `+layout.svelte`.

Two layers, and the split is load-bearing:

- **Reference** (`--pd-crimson-500: #e94560`) — named for what a value *is*.
  Theme-owned. **Components must never reference `--pd-*`.**
- **Semantic** (`--color-accent: var(--pd-crimson-500)`) — named for what a
  value *does*. This is the only layer components may use.

A future re-skin is then a new reference block, not a refactor; light mode is
the same mechanism (`:root[data-theme='light']`), designed for and deferred.

`frontend/npm test` enforces it — and enforces WCAG AA on every pairing
declared in `contrast-pairs.json`. Contrast is a test, not a review note: add
a colour role that gets painted on a surface, add its pairing.
`legacy-color-map.json` maps each raw colour still left in `frontend/src` to
the role that replaces it; migrations read the replacement off that file
rather than inventing one.

`raw-color-budget.json` is the ratchet toward zero raw colour. It records how
many literals each file still holds; the count may only go **down**. Exceed a
budget and the test reads it as a regression; drop below it and the test fails
too, printing the number to write. Migrating a file means lowering its entry
in the same commit, and deleting the entry when it reaches zero. When the
budget is empty the target is met and any literal anywhere fails the build.
Never raise a budget — a value that has nowhere to live needs a semantic role
in `tokens.css`, not an exception.

### UI primitives

`frontend/src/lib/components/ui/` is the visual vocabulary — `Panel`, `Button`,
`Field`, `Badge`, `ProgressBar`, `SectionHeader`, `EmptyState`, `Toolbar`,
re-exported from `$lib/components/ui`. Routes render; they do not decide
surfaces, fills, rules or spacing.

Every primitive is styled from the **semantic** token layer only — no colour
literal, no `--pd-*`. A route that needs a variant a primitive lacks **adds the
variant to the primitive**; the moment two routes patch the same primitive at
the call site, the system is back to taste.

`frontend/tests/primitives/` has one render test per primitive (renders,
respects its variants, emits no hardcoded colour). It renders components
server-side under Node's built-in test runner — no jsdom, no testing-library
— via the loader hook in `frontend/tests/support/svelte-hooks.js`, which
compiles `.svelte` on import (`npm test` wires it in with `--import`).

### Performance

- Shared catalog connection opens with WAL + `synchronous=NORMAL` +
  64MB cache (`open_shared` in `crates/pkdump-db/src/connection.rs`).
  This is what keeps variant expansion at ~5000 cards/s instead of
  collapsing to ~50/s past card 9000.
- Per-card transactions in `expand_all_printings` are intentional — they
  keep an interrupted refresh from leaving a card half-deprecated. The
  PRAGMA tuning above is what makes them cheap.
- Bulk-ensure FK targets at function entry; don't do a SELECT-per-row
  check inside the hot loop.

## Operating notes

- Production data volume is the Podman volume `pkdump-prod-data` (or
  whichever instance). On disk: `~/.local/share/containers/storage/volumes/pkdump-<instance>-data/_data/`.
- `pkdump-prod` listens on `8090` by default (set via the Quadlet unit).
- `bd dolt push` exports issues + memories to the Dolt remote at end of
  session.
- Backups are off-box on S3 via the Litestream sidecar (6-month PITR; no local
  snapshots). To recover, follow `deploy/RESTORE.md` (or
  `deploy/restore-litestream.sh <instance>`). The schema init runs on first open
  and is a no-op against an already-shaped restored DB.
