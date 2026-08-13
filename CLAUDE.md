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
cargo test -p pkdump-lake        # raw-landing key layout, manifest, config
cargo test -p pkdump-ingest --test raw_landing
                                 # the real HTTP clients against a local
                                 #   upstream: landing is a tee, a retry never
                                 #   overwrites, a short run says so
cargo test -p pkdump-lakehouse   # partition choice, replay, the comparator —
                                 #   and tests/row_identical.rs, the acceptance
                                 #   matrix, against the shipped binary
bash tests/lake/derive.sh        # the whole CATALOG from raw/, in the shipped
                                 #   image, on an --internal network with the
                                 #   socket-to-1.1.1.1 egress assertion
PKDUMP_REAL_DERIVE=1 bash tests/lake/real_upstream_derive.sh
                                 # NOT in CI: the same claim against the REAL
                                 #   upstreams at real catalog scale, ~5min and
                                 #   ~1,350 live fetches. Run per milestone.
bash tests/lake/value_snapshots.sh
                                 # the transform tier: value snapshots for EVERY
                                 #   registered tenant, byte-identical to the
                                 #   Rust aggregate they replace — and §10, the
                                 #   shipped wrapper its timer runs
bash tests/refresh/tenant_bytes.sh
                                 # the other half: a real `pkdump data refresh`
                                 #   over a data dir with two tenants in it
                                 #   leaves every tenant database byte-identical
cargo clippy --all-targets       # lint (must be clean before commit)
cargo fmt                        # format

# CLI / server
cargo run --bin pkdump -- setup  # build the shared catalog (downloads upstream)
cargo run --bin pkdump -- serve  # start the HTTP server
cargo run --bin pkdump -- data refresh   # incremental catalog refresh (ONLINE)
cargo run --bin pkdump-lake-derive -- shared --ingest-date 2026-08-11 \
                                 # the same derivation, OFFLINE, replaying raw/
                                 #   a URL not in raw/ refuses; there is no
                                 #   fallback to the live upstream
cargo run --bin pkdump-lake-derive -- diff --left a.sqlite --right b.sqlite \
    --exclude raw_derivation     # row-by-row, never byte-by-byte
cargo run --bin pkdump -- seed-fixture   # build the deterministic UI-test fixture

# Portable collection backup — every user table in one versioned JSON
# envelope. A whole-database restore, not a merge (--force to overwrite).
cargo run --bin pkdump -- export --json -o collection.json
cargo run --bin pkdump -- import --json collection.json

# Frontend — SvelteKit (Svelte 5, vite, adapter-static)
cd frontend && npm install && npm run build
cd frontend && npm run check     # svelte-check / TypeScript
cd frontend && npm test          # design-token gates (WCAG AA contrast, layer
                                 #   split, raw-colour + raw-dimension ratchets)

# Deploy scripts — container-store resolution, the low-disk guard, the unit-file
# install, and the transform tier's scheduling (0/2/1, ordering). Hermetic.
bash tests/deploy/run.sh

# Shell-harness self-tests — sub-second, no container. The second one also
# greps tests/ and deploy/ for a picked host port and fails on one. The third
# reads the Containerfile: builder and runtime must name the SAME Debian
# release, and the target cache id must name it too.
bash tests/lib/diagnostics_test.sh
bash tests/lib/ports_test.sh
bash tests/container/base_images_test.sh

# Browser tier — every route screenshotted at 1440 and 768 against a
# throwaway container instance, plus the DOM assertions a screenshot cannot
# make (/collection renders a viewport-sized WINDOW of a 56k-row result). A
# pixel diff fails; approving one is explicit.
bash tests/visual/run.sh         # check
bash tests/visual/run.sh --update  # approve — see tests/visual/README.md

# UI test harness — Playwright + intent YAMLs              (in progress)
cd tests/ui && npm test
```

`$PKDUMP_HOME` overrides the data dir (default `~/.pkdump/`).
`$PKDUMP_USER` overrides the active tenant (default `collection`).

## Architecture Overview

Cargo workspace, eight crates (`crates/`):

- **pkdump-core** — domain types + pure logic (variant code parsing,
  override matching, import format adapters). No IO.
- **pkdump-db** — rusqlite persistence. Owns the shared/user DB split,
  the schema files (`src/schema_{shared,user}.sql`, re-applied idempotently
  on every open — single-instance project, no migration history), the
  variants display table, and the binder-page query.
- **pkdump-derive** — the catalog derivation itself: the body of what
  `pkdump data refresh` used to do inline, moved out **verbatim** so the
  online CLI and the offline job run the *same* code. It knows nothing about
  `raw/`; it takes a `Wire` that is empty, or landing, or replaying. Its
  `DeriveClock` is read once by the landing side and recovered from the
  manifest by the deriving side, which is what makes a rebuild reproduce
  timestamps rather than approximate them.
- **pkdump-lakehouse** — the offline job (`pkdump-lake-derive`), the ONLY
  thing that reads `raw/` on the derivation path. Bin-only, so no online
  target can link it. Partition choice and its refusals, the URL-keyed
  replay + the temporary loud fallback, and the row-by-row catalog
  comparator. See "The offline catalog derive" below.
- **pkdump-ingest** — upstream catalog ingestion (pokemon-tcg-data,
  pokemontcg.io, TCGCSV). Variant expansion lives here, as does the
  Pokémon Japan pipeline (`japan.rs`). Touched only by
  `pkdump setup` / `pkdump data refresh` — never at request time.
  `landing.rs` is the one place an upstream response becomes bytes, so
  "land what we fetched" is a property of a single function.
- **pkdump-lake** — the raw landing zone: the `raw/` key layout, the
  run manifest, the object stores, and the `lake.env` config. Write-only
  and offline-only; nothing on the serving path touches it. See
  `deploy/LAKE.md`.
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

Provisioning is `pkdump tenant create|list|rename|detach`, driven by the
**user registry** (`registry.sqlite` at the data root — see
`crates/pkdump-db/src/registry.rs`), not by the directory listing. A user is
a `handle` joined to an opaque ULID `database_id`; their collection is
`tenants/<database_id>.sqlite`, so a handle is never a path component and a
rename never moves a file.

**`pkdump tenant remove <name> --yes` no longer deletes anything.** It is an
alias for `detach`: the handle is released for reuse and the database and its
S3 replica are kept. Hard deletion is the explicit second step `pkdump tenant
purge <database-id> --yes`, addressed by id so it cannot be reached by
mistyping a live person's name.

Two migrations, each with a rollback, because a box can be at either point:
`pkdump tenant adopt` moves a pre-`tenants/` data dir into `tenants/`
(`revert` rolls it back), and `pkdump tenant migrate` puts handle-named
databases already in there onto opaque ids, registry row and all
(`unmigrate` rolls that back). `migrate` is idempotent and refuses a busy
database. Layout rationale + both runbooks: `deploy/TENANTS.md`.

**Neither migration gates startup**, deliberately. `pkdump serve` serves an
un-migrated data dir exactly as it finds it and prints which database it
opened, because a required migration is what took prod down on the first
automated deploy of the last one (`pd-uoph`). What it will not do is come up
*empty*: `pkdump_db::tenants::resolve` is the single resolution point, and
every branch either finds real bytes or fails naming the command that makes
them exist. `tests/tenants/upgrade.sh` is the container-tier gate — an
OLD-LAYOUT volume through the shipped image, migrated, rolled back, and
migrated again, with the served collection asserted byte-identical
throughout. Fresh instances do not exercise the upgrade path; `deploy/setup.sh
--test` builds its volume in the current layout, which is exactly how the last
one shipped untested.

A rename changes the file name, and `deploy/litestream.yml` derives each
replica prefix *from* the file name — so `migrate`/`unmigrate` **delete**
Litestream's per-database state directory instead of moving it with the file
(`adopt`/`revert`, which keep the name, still move it). Carrying that state
across a prefix change is what left prod replicating nothing at
`txid.replica=0` while the unit reported healthy (`pd-1717`).

Tenant *resolution* lives in `pkdump-server/src/tenant.rs` and is **off by
default**: `pkdump serve` opens the one collection named by `$PKDUMP_USER`
and does not read the tenant header at all. `--multi-tenant` (or
`PKDUMP_MULTITENANT=1`) switches on per-request resolution from the
`x-pkdump-tenant` header. **Nothing authenticates that header** — identity is
a separate epic — so the flag must stay off in production. That is enforced
rather than trusted: with the flag on and a non-loopback `--host`, the server
refuses to start unless `PKDUMP_MULTITENANT_INSECURE_BIND=1` is also set.
Single-tenant mode is unaffected at any address. A container publishes a port,
so its entrypoint binds `0.0.0.0` — every containerised multi-tenant instance
needs that second opt-in, which is why `tests/tenants/handles.sh` sets it and
nothing under `deploy/` does. That gate's §3 runs the same container *without*
it and asserts the refusal, so the escape hatch cannot quietly become the
default.

What the header carries is a *handle*, and it is only ever a lookup key: the
user registry (`registry.sqlite`, `pkdump-db/src/registry.rs`) maps it to an
opaque `database_id`, and that id — never the header — is what
`pkdump_db::tenant_db_file` turns into `tenants/<database_id>.sqlite`. Isolation is
structural: `AppState` holds no connection, `blocking()` takes the tenant from
the request scope, and one connection per tenant is opened against that
tenant's own file. See `deploy/TENANTS.md`.

The header is still validated at the boundary, before the lookup, and the two
refusals are different answers: not a handle is a **400** quoting
`pkdump_db::HANDLE_RULE` (and never the value sent), a handle nobody actively
holds is a **404**. The rule has two enforcers that cannot share code — that
validator and the `handle` `CHECK` in `schema_registry.sql` — so they share the
`paths::HANDLE_CASES` corpus, run through each by a test on each side. Add a
character to one and the other's test fails. `tests/tenants/handles.sh` is the
container-tier gate for the status codes, single-tenant mode included.

## Deployment

Rootless Podman + systemd-user, scripts in `deploy/`:

```bash
deploy/setup.sh prod        # one-time: build image, install Quadlet unit + timers
deploy/seed.sh prod         # populate the catalog (pkdump setup in a one-off container)
deploy/deploy.sh prod       # rebuild + reinstall the unit files + restart
deploy/restore-litestream.sh prod   # restore the collection from S3 (see deploy/RESTORE.md)
deploy/teardown.sh prod [--purge]
```

Non-prod container storage does not belong on the disk prod runs from.
`PKDUMP_STORE_ROOT=<dir>` puts an instance's image, layers, volume and
Buildah cache in an alternate rootless store. Which directory that is on a
given box is **host config** — `~/.config/pkdump/store.env`, alongside
`alerts.env` and `litestream.env` — never inferred from the machine's disk
layout; `deploy/ci.sh` reads it, an explicit environment variable beats it,
and unconfigured means Podman's default store. **Prod never opts in**
(`setup.sh` does not read `store.env` at all) — its unit and volumes are
untouched by construction. The generated Quadlet unit records the store so
teardown removes from the same one. `deploy/store-teardown.sh` removes a store
outright — `teardown.sh` only ever removes an *instance*, so nothing collected
the store itself.

A second store is not free. Podman 4.9 gives each store its own rootless-netns
file but one shared scaffolding directory, and whichever store cleans up last
deletes it — leaving the other unable to start any container on a user-defined
network, which is what every Litestream gate uses. `pkdump_store_activate`
detects the stale netns file and drops it (never `podman system migrate`: that
kills the per-user pause process prod's store shares). See `deploy/store-lib.sh`
and the README's "Container storage".

The image runs `pkdump serve`; the data volume holds both `shared.sqlite`
and the per-user collection DB. Off-box backup is the
`pkdump-litestream-<instance>` sidecar (continuous S3 replication, 6-month PITR;
no local snapshots). Nightly catalog refresh comes from
`pkdump-refresh@<instance>.timer`.

Every base image in the `Containerfile` **names its Debian release**, and the
builder's release is the runtime's. A moving tag is not a pin: upstream retagged
`rust:1.94-slim` from bookworm to trixie, the builder started linking against
glibc 2.39, and every image built after that shipped a binary the bookworm
runtime could not exec (`pd-pejn`). The target cache mount carries an `id=`
naming that same release, because cargo fingerprints do not record which base
image produced the objects — a shared cache handed the next build the trixie
artifacts, cargo relinked nothing, and re-pinning the `FROM` line alone looked
like a fix while changing nothing. Move the base, move the id.
`tests/container/base_images_test.sh` asserts all three in under a second, and
`deploy/ci.sh` runs it before anything builds the image.

There's a `deploy` skill in this repo that wraps these scripts for AI use.

## CI

`.github/workflows/ci.yml` is a thin wrapper around `deploy/ci.sh` — the same
script a developer and a polecat run. Every step in it is a named **tier**
(`lint rust deploy frontend container litestream tenants browser schema lake
refresh`), and which tiers run can be selected from the paths a change touched.

`deploy/ci-select.sh` is the rule, and it is two patterns wide on purpose:

    docs (**.md, docs/**, wiki/**)  -> the lint tier, and nothing else
    frontend/**                     -> frontend + container + browser
    ANYTHING ELSE                   -> every tier

Per-path requirements are **unioned** and nothing subtracts, so a mixed change
can only ever run more. It fails closed at every edge — an unrecognised path,
an empty list, no list at all. **Adding a bucket is a deliberate edit with a
test beside it; the cost of forgetting is a slow run, never a missed gate.**

`frontend/**` is **not** subdivided, and that is load-bearing. The obvious
refinement — routes take the browser tier, shared lib files do not — is exactly
pd-tf4h, where visual baselines went un-re-recorded because a change did not
look like a UI change. A token, a shared primitive or a rule in `app.css`
repaints every route at once, so *any* change under `frontend/` screenshots
every route.

Selection is **opt-in per run**: `deploy/ci.sh` runs everything unless handed
`PKDUMP_CI_CHANGED_FILES`, and the only thing that hands it one is a
`pull_request` in `ci.yml`. A developer, a polecat, `workflow_dispatch` and any
future push-triggered run therefore get the full suite by construction —
nothing has to remember to ask for it. `PKDUMP_CI_SELECT_ONLY=1` prints the plan
and runs nothing.

`tests/ci/select_test.sh` is the gate (hermetic, sub-second, in the lint tier,
so a docs-only PR still runs it). A tier renamed in one file and not the other
fails it in a second, and a guard on a name that is not a tier is fatal rather
than a silent skip.

### CI triggers on the PR's BASE branch — master and `integration/**`

`ci.yml` is `on: pull_request: branches: [master, 'integration/**']`, and that
filter matches the **base**, not the head. A PR opened into anything else gets no
CI at all — not queued, not cancelled, not awaiting approval. No run is created.

`[master]` alone was the original, which meant **every child PR of every epic went
untested**, because an epic's children target its integration branch. Six PRs
(#31-#36) sat that way on 2026-08-13, and it is why polecats had been reaching for
`workflow_dispatch`: it was the only thing that produced any signal.

Two things made it invisible:

- the ruleset's required `test` check guards `master` only, so nothing complained
- an admin bypass reports `CLEAN` regardless, so the merge box looked fine

**An epic's children are the reviewable unit** — a polecat branch is the PR source.
Gating only the eventual `integration -> master` PR hides a broken child until the
whole epic lands, which is the opposite of what the integration branch is for.

If you add another long-lived base branch, add it here too, or its PRs are
silently untested.

### Never `workflow_dispatch` a branch that is going to become a PR

**Open the PR first and let `pull_request` be the only trigger.** Running CI by
hand on a branch before its PR exists permanently forfeits that commit's PR check.

GitHub creates **one check suite per (commit, app)**. A `workflow_dispatch` run
creates that suite; opening the PR on the same SHA afterwards then produces no new
run, because the suite already exists. The dispatch run's check is *back-linked* to
the PR — `commits/<sha>/check-suites` even reports `pull_requests=[N]` — but GitHub
only surfaces a check on a PR when the suite was created by a PR-triggering event.
So the merge box sees the required `test` context as **missing**, not as green.

On 2026-08-13 five of six open PRs were in exactly that state. Every one showed no
checks in the UI, `gh pr checks` reported "no checks", and `statusCheckRollup`
returned zero entries — while two of them had in fact FAILED their dispatch run.

Two traps that made it invisible for hours:

- **`mergeStateStatus` is not a safety signal for an admin.** The ruleset's bypass
  is `RepositoryRole 5 / bypass_mode: always`, so a repository admin is told
  `CLEAN` regardless of whether a required check is missing or red. Untested and
  failing PRs read exactly like passing ones.
- **`gh pr checks` returning "no checks" means "no PR-triggered suite", not "not
  tested".** It is silent about a dispatch run that failed.

**Verify CI with `gh api repos/<owner>/<repo>/commits/<sha>/check-runs`** — it
reports the conclusion whatever the trigger was. Never conclude a PR is green from
`gh pr checks` or from `mergeStateStatus`.

If a branch has already been dispatched and needs a real PR check, push another
commit: a new SHA gets a new suite, and the `pull_request` event creates it.

**Eleven of the gates run in parallel, two at a time** (pd-2nl9; lowered from
three on 2026-08-13 — see `PKDUMP_CI_JOBS` in `deploy/ci-parallel.sh`) —
litestream, drill, alarming, recreate, upgrade, tenant-header, schema-version,
the three lake gates and refresh. They do not run where they are written: each
**queues** itself under its own tier guard and `deploy/ci-parallel.sh` runs the
queue at the end. What makes that safe is not new — every one of those scripts
already derives every name it uses (network, container, volume, image tag, unit
prefix, temp dir) from its own prefix plus a hash of the checkout path, because
concurrent polecats have run whole suites beside each other for months.

Four things about it are decisions:

- **The cap is a resource decision, not a tuning knob.** Three by default, four
  at the ceiling, on a four-core 15G box that also runs prod and where each
  gate stands up two or three containers. Above that, resource exhaustion
  starts looking like flaky gates. `PKDUMP_CI_JOBS=1` is the serial run, and
  the first thing to try when a parallel run misbehaves.
- **The disk floor is checked before every dispatch**, not once at startup —
  the same `diskcheck.sh --floor` over the same two filesystems. Below the
  floor with gates running it **holds** (the gate about to tear itself down is
  what gives the space back); below the floor with nothing running it fails,
  naming the gates that never started.
- **A failing gate no longer stops the ones beside it.** The wave finishes,
  every gate's output is printed whole under its own name, and the run ends red
  naming all of them. One red run now reports everything that is broken instead
  of only the earliest failure.
- **Output is buffered per gate and printed on completion.** Concurrent writers
  shred each other, and a shredded CI log is a gate nobody can diagnose.

`tests/ci/parallel_test.sh` is the gate — hermetic, lint tier, ~4s. It asserts
the cap is reached and never exceeded, that a failure among passes is red and
named, that output survives concurrency, that the *real* `diskcheck.sh` trips
against an impossible floor, that the hold branch waits rather than aborts, that
a background job of the *caller's* is never mistaken for a gate, and that every
one of the eleven gate scripts is still queued exactly once under a real tier.
That last one is the refactor's own failure mode: a gate queued nowhere runs
never, and a green run cannot show you that.

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
- `data/overrides/catalog_prices.json` seeds `catalog_price_overrides` in
  the shared catalog: a price for a **catalog** printing TCGplayer does not
  price. A gap in the feed is identical for every tenant, so it is patched
  once, in the catalog, in git — not re-entered per user. `latest_prices`
  always wins over it, so an override left behind after upstream catches up
  is inert rather than wrong. The tenant's own `manual_prices` survives for
  exactly one thing: a printing that tenant invented (`user_printings`);
  `manual_prices::insert` refuses anything else with a 400. One rule for
  "what is this printing worth" — `pkdump-db::prices::MARKET_PRICE_EXPR`.

When you find logic that should be data, file a `bd create
--type=decision` issue and propose the schema before writing more code
against it.

### Card data access

All runtime card lookups read the local DB. The upstream APIs are touched
only by `pkdump setup` / `pkdump data refresh`. See
`architecture/CARD_DATA_ACCESS.md`.

### The raw landing zone

`pkdump data refresh --land-raw` (and `pkdump setup --land-raw`) writes every
upstream response it fetches to S3, immutably, **before parsing it** —
`raw/source=…/dataset=…/ingest_date=…/run=<ULID>/part-NNNN.<ext>.zst` plus a
`_manifest.json` carrying the URL, HTTP status, byte count and SHA-256 of
every part. `run=<ULID>` is what makes a retry after a partial failure land
*beside* the first attempt rather than on it.

Three rules that are settled, and are the ones easiest to erode:

- **`images.pokemontcg.io` is never landed.** The retention arithmetic that
  justifies keeping `raw/` forever is for JSON only; card art would change it
  completely. `symbols.rs` fetches without landing, deliberately.
- **The lake bucket is a different bucket from the Litestream backup
  bucket**, and its name is host config in `~/.config/pkdump/lake.env` with
  **no default**. Asked for and unconfigured is a refusal that names the
  file, never a silent skip.
- **There is deliberately no lifecycle rule on `raw/`.** Indefinite
  retention is measured — ~7.4 MB/night in the bucket across all four
  datasets, from a real run (`deploy/LAKE.md` §2) — and intentional.

Landing is opt-in and off by default: with the flag absent, `lake.env` is
never read and the fetch path is exactly what it was. `deploy/LAKE.md` is the
runbook.

**The nightly refresh runs in its own container** (`deploy/refresh.sh`), like
the derive and transform jobs beside it — it does not `podman exec` into the
running server. That call drops the caller's environment, so the documented
drop-in that turns landing on set `PKDUMP_LAND_RAW=1` somewhere the refresh
could never read it, and the timer went green having landed nothing (pd-kncd).
`-e` alone would not have been enough: `podman exec` cannot add a mount to a
running container either, and the alternative — putting the lake's credentials
on the app container — would give the always-on web server ambient write access
to the lake bucket, which is the exact coupling `pkdump-lake` is offline-only to
prevent. **Nothing that serves a request may hold a lake credential.** The
wrapper's last line is the guard that matters: landing asked for and no landing
zone opened **fails the unit**, because that is the silent green no-op the
whole landing zone is worthless without.

### The transform tier

`lake/src/pkdump_lake/value_snapshots.py` is the first job that *reads* the
lake: it values **every registered tenant's** collection from `catalog.prices`
at a pinned Nessie commit and writes `collection_value_snapshot` into that
tenant's own database. Three rules hold it in shape:

- **The unit of work is the registry, not the current user.** The refresh used
  to end with a step 7 that snapshotted the one collection `$PKDUMP_USER`
  resolves to, and reported success for everybody (pd-s5yn). Any successor to
  it walks the registry, or it has reintroduced the bug.
- **A failing tenant is logged and skipped; the run finishes and exits 2.**
  Exit 0 means every tenant, 1 means the run never started. Silence over a
  half-completed run is the failure mode being replaced.
- **Tenant data never enters the lake.** Prices come out of Iceberg; the
  collection is read from, and the snapshot written back to, SQLite. Neither
  ever travels the other way, and `tests/lake/value_snapshots.sh` §9 asserts
  the catalog still holds nothing but `catalog.prices` after a run.
  That is the runtime half. The static half is
  `tests/lake/tenant_isolation_test.sh` (lint tier, hermetic, pd-cgi9): no
  Iceberg schema field name is tenant-identifying, and no lake write path *can*
  open a tenant database — `crates/pkdump-lake` links no SQLite crate and the
  Python write-path modules import no `sqlite3`, so both hold by construction
  rather than by review, the way the closed `Source` enum holds "images are
  never landed". Adding a tenant column or a tenant DB open now fails in a
  second instead of nothing at all. The transform tier is the deliberate
  exception the guard encodes: it opens every tenant's database, and is
  asserted to only ever READ the lake.

The aggregate itself is a transliteration of `value_history.rs` — two
implementations of one calculation, deliberately, because the rewrite has to
be *observably a no-op* before it is trusted. The container gate is what holds
them together: it diffs the transform's rows against the ones Rust's
`snapshot_today` computed over the same fixture.

**It is on a timer** (pd-8m5c) — `pkdump-value-snapshots@<instance>.timer`, whose
service runs `deploy/value-snapshots.sh`. With step 7 deleted this job is the
only thing that records today's value for anybody, so a job nobody scheduled is
a value history that stops advancing, quietly, on every box. Three things about
the scheduling are decisions:

- **The ordering is declared, not timed.** `After=pkdump-refresh@%i.service`
  is the guarantee that the two never run beside each other; the timer's
  `OnCalendar=07:00` is *derived* from the refresh unit's own bounds (06:00 +
  `RandomizedDelaySec` + `TimeoutStartSec`) and `tests/deploy/run.sh` §10
  recomputes it. Not `Wants=`: the refresh is a oneshot without
  `RemainAfterExit`, so pulling it in would re-run the catalog fetch nightly.
- **`SuccessExitStatus=2`.** A skipped tenant is a partial run — a database
  mid-import, a restore in flight — not a failure, and a unit that paged on it
  would page on a normal night. It is not silent either: the wrapper names the
  skipped tenants and pushes a warning. Exit 1 still fires `OnFailure=`.
- **The date comes from the scheduler.** The job refuses to default `--date`
  from the clock (backfilling an older day is the same operation), so the
  wrapper — the one component that is allowed to know what day it is — names it.

### The nightly price build

`catalog.prices` is what the transform tier reads, and until pd-up36 nothing
built it on a schedule — `pkdump-lake-build-prices` was a hand-run podman
invocation. The transform therefore valued every tenant's collection from
whatever day someone last built: **correct arithmetic over stale prices,
advancing every night, with nothing anywhere saying the numbers had stopped
moving.** Same failure class as pd-s5yn — a job that looks like it ran.

`pkdump-prices@<instance>.timer` closes it, running `deploy/prices.sh`. Two
things about it are decisions:

- **The nightly build passes `--allow-incomplete`; the alarm is on AGE.** A
  hand run should refuse a day holding no complete run, but `complete` is
  conservative across datasets — a pokemontcg.io tail that died marks the
  *prices* manifest incomplete on a night when every price fetch succeeded,
  which is the normal shape of a flaky night. A unit that failed there would
  page most nights, and a pager that cries wolf gets ignored (pd-me6h). So the
  day is built, the snapshot records `pkdump.raw-complete=false`, and
  `pkdump-lake-prices-age` (`lake/src/pkdump_lake/freshness.py`) pages when the
  newest partition falls more than two days behind. That check runs on **every**
  run, not only after a failed build: a check wired to the failure path fires on
  almost no night and nobody would notice it had broken — and on the success
  path it is the only thing that asks the table rather than believing the
  build's report of itself.
- **0 / 2 / 1 are three answers**, as for the transform: built-and-fresh, a
  missed day over a still-fresh table (`SuccessExitStatus=2`, warned not paged),
  and a stale table or an age that could not be established at all — both page.

The chain is now total and every link is declared, never inferred from three
timers sharing 07:00: **land → derive → prices → transform**. Arming one of the
lake timers without the others is what reintroduces the bug, which is why
`deploy/setup-lake.sh` names them together. Gates: `tests/deploy/run.sh` §13
(units + the wrapper's whole exit mapping, hermetic) and `tests/lake/prices.sh`
§10–§12 (the real freshness job and the shipped wrapper against a real lake).

### The offline catalog derive

`shared.sqlite` can be rebuilt from one `raw/` partition by
`pkdump-lake-derive shared --ingest-date <date>`, replaying every upstream
response instead of fetching it (pd-1uem). Four things about it are decisions,
not implementation:

- **It is a separate binary because of where it runs, not what it does.**
  *Only lakehouse code reads `raw/`.* A `--from-raw` flag on `pkdump data
  refresh` would put a raw reader inside `pkdump-cli`, on the ONLINE side,
  which is exactly the coupling that rule exists to break. `pkdump-lakehouse`
  is bin-only and `pkdump-cli` does not depend on it.
- **It is a relocation, not a second implementation.** Both callers run
  `pkdump_derive::derive`. That is what makes "row-identical" a claim about
  provenance; two implementations agreeing would only be evidence about the
  second one.
- **Idempotence is keyed on the PARTITION, never the clock.** No default
  `--ingest-date`; the partition asked for must exist and be complete, with no
  fallback to the newest available; re-deriving a date replaces it; and
  `shared.raw_derivation` records which run ULIDs produced the catalog, so a
  rerun is *identifiable* rather than merely tolerated. `observed_at` stays
  distinct from `ingest_date` — they differ for exactly the run that crossed
  UTC midnight.
- **A URL missing from `raw/` is FATAL, unconditionally.** Coverage has
  regressed; the run refuses, naming the URL and saying to re-land the date.
  Item 2's temporary fallback — fetch live, print `!! raw coverage has
  REGRESSED`, finish anyway — and its `--no-upstream-fallback` opt-out are
  **gone** (item 4, pd-6yql), removed once pd-vves proved row-identity against
  the real bucket. The flag is rejected by name rather than ignored, so an old
  invocation fails loudly instead of appearing to work. A fallback is not a
  safety net to restore: it makes the landing zone decorative, producing a
  correct catalog whose lineage cannot be reproduced, which surfaces on the day
  an upstream is down — the day the lake was bought for.
- **Set symbols are not an exception to that rule, and never went through it.**
  `symbols::normalize_all_symbols` takes `(&mut Connection, &Path)` — no `Wire`
  — and builds its own HTTP client, because images are deliberately outside
  `raw/`. So an offline derive still fetches them live, and a fetch that fails
  is counted, logged, and **not** fatal: the set keeps its upstream URL, which
  still renders. `row_identical.rs::a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`
  is the gate, on a catalog whose symbols have never been normalised — the shape
  pd-vves's proof could not exercise, because prod's catalog was already
  normalised and both sides skipped the phase (pd-5w4n).

Anything that lands in a ROW is passed in rather than read: `DeriveClock`
carries the one instant a run read, `Manifest.started_at` is where the landing
side wrote it down, and `expand_all_printings` takes `deprecated_at` for the
same reason. A clock read inside the derivation is the one value an offline
rebuild can never reproduce, which is why `crates/pkdump-derive/src/clock.rs`
is mostly argument.

Two units, and the split is the point (item 5): `pkdump-refresh@` LANDS,
`pkdump-derive@` DERIVES, so a derive can run against yesterday's raw on a
night the fetch failed. Ordering is declared (`After=`, never `Wants=` — the
refresh is a oneshot without `RemainAfterExit`), the calendar entry is derived
from the refresh unit's own bounds, and the chain is land → derive → transform.
The derive unit has **no `SuccessExitStatus=`**, unlike the transform: that job
writes N tenant databases and "some of them" is normal, this one writes ONE
catalog and a smaller catalog reads as cards that do not exist.

Gates: `crates/pkdump-lakehouse/tests/row_identical.rs` (hermetic — row-identity
over two days, idempotence, reproducing an older date, both refusals, a
corrupted payload, the fallback loud one way and fatal the other) and
`tests/lake/derive.sh` (the container tier, shipped image, `--internal`
network, socket-to-1.1.1.1). Runbook: `deploy/LAKE.md` §8.

Both of those upstreams are fixtures, so a third gate exists and is deliberately
**not** in CI: `tests/lake/real_upstream_derive.sh` makes the same claim against
the REAL tcgcsv.com and api.pokemontcg.io, at real catalog scale (pd-aer9). On
2026-08-13 it landed 1,345 real responses and rebuilt them into a catalog
row-identical across all 21 tables — 47,640 cards, 75,627 printings, 289,255
prices. What it cannot yet be run against is **prod's own `raw/`**: the lake's
only partition (2026-08-11) predates `Manifest.started_at` and is refused for
having no clock, and nothing has landed since because `pd-kncd` is unmerged. The
sequence is pd-kncd → one night's raw → derive that date from the bucket. Do not
arm `pkdump-derive@prod` before then: it has no `SuccessExitStatus=`, so a
refusal every night is a page every night.

One phase cannot be replayed: set-symbol normalisation fetches images, and
images are deliberately not landed (pd-5w4n).

**The catalog refresh writes no tenant database at all** (pd-hkbc). Step 7 is
gone; `pkdump data refresh` touches `shared.sqlite` and, with `--land-raw`, the
`raw/` prefix, and nothing else. `tests/refresh/tenant_bytes.sh` is the gate: a
real refresh through the shipped image over a data directory with two
provisioned tenants, every tenant database byte-identical afterwards. Its
upstream is `tests/refresh/upstream.py`, a fixture that publishes nothing —
reached through `PKDUMP_TCGCSV_BASE_URL` / `PKDUMP_POKEMONTCG_BASE_URL`
(`crates/pkdump-ingest/src/upstream.rs`, test-tier, and an override announces
itself on stderr so a catalog can never be quietly built from the wrong place).

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
- **Schema** — the full schema for each database lives in
  `crates/pkdump-db/src/schema_{shared,user}.sql` and is re-applied with
  `CREATE … IF NOT EXISTS` on every open. No migration history, no
  refinery: additive change travels by idempotent re-application.
- **Schema versions** — every database carries its schema version in
  `PRAGMA user_version`, and a file written by a *newer* build is
  **refused, not opened** (`crates/pkdump-db/src/schema_version.rs`).
  Lower or 0 is adopted in place; equal is a no-op. That refusal is what
  makes rollback (`pkdump tenant revert`) safe — an older binary must
  stop rather than quietly operate on a schema it does not know.
  `Database::version()` is the one place the numbers live; bump one only
  when a change cannot be expressed as `CREATE … IF NOT EXISTS`, and
  never as a substitute for a migration you have not written.
  One database per tenant means they can legitimately differ, so
  `pkdump tenant list` reports each tenant's own version and whether it is
  behind, current, or ahead of the running build — reading, deliberately,
  is not gated: a tenant the server refuses to open is exactly the one the
  report exists to name (`deploy/TENANTS.md`).
  `tests/schema-version/run.sh` (container tier, run by `deploy/ci.sh`)
  proves all of it against a prod-shaped instance.
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

Colour is not the whole layer. `tokens.css` also declares **space**
(`--space-*`), **type** (`--text-*`), **radius** (`--radius-*`) and
**elevation** (`--shadow-*`), and `raw-dimension-budget.json` is the same
ratchet pointed at those: every `padding`/`margin`/`gap`, `font-size`,
`border-radius` or `box-shadow` declaration that still spells out a length
instead of spending a step. Same rules — down only, delete at zero, never
raise. The unit is the *declaration*, not the literal, so `padding: 0.4rem
0.6rem` is one. Unitless `0` doesn't count; a `calc()` multiplier over a token
doesn't either. It was seeded at the counts of the day it landed and is
deliberately not a migration — routes shed theirs as they get touched.

### UI primitives

`frontend/src/lib/components/ui/` is the visual vocabulary — `Panel`, `Button`,
`Field`, `Badge`, `ProgressBar`, `SectionHeader`, `EmptyState`, `Toolbar`,
`SearchField`, `Segmented`, `Menu`, `Pager`, re-exported from
`$lib/components/ui`.
Routes render; they do not decide surfaces, fills, rules or spacing.

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
- **Responses are compressed** — one `CompressionLayer` on the outermost
  router (`compression()` in `crates/pkdump-server/src/lib.rs`), so it
  covers `/api`, the SPA shell and the SvelteKit bundle alike. br or gzip,
  whichever the client's `Accept-Encoding` asks for; a client that asks for
  neither still gets valid uncompressed bytes. Two exclusions, both
  deliberate: anything `image/*` (already-compressed PNG/JPEG — gzipping
  those spends CPU to grow the payload) and anything at or below
  `COMPRESS_MIN_BYTES` (1 KB, which already fits one TCP segment). JSON of
  this shape compresses ~9x, which is what makes a catalog-wide result set
  affordable at all. Nothing buffers a whole body to compress it, so
  `/api/export/*` costs no more memory than before.

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
