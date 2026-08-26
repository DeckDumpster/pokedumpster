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
cargo test -p pkdump-ship        # the shipper: part planning and gap
                                 #   detection, the sealed envelope, Parquet —
                                 #   and tests/shipping.rs, the four claims
                                 #   pd-dxn3 asks for, end to end
cargo test -p pkdump-erase       # the deletion path: the prefix-confined sweep,
                                 #   the proof and both its vacuity guards —
                                 #   and tests/deletion.rs, the whole path over
                                 #   really-shipped holdings, seen green AND red
cargo test -p pkdump-lakehouse   # partition choice, replay, the comparator —
                                 #   and tests/row_identical.rs, the acceptance
                                 #   matrix, against the shipped binary
bash tests/lake/derive.sh        # the whole CATALOG from raw/, in the shipped
                                 #   image, on an --internal network with the
                                 #   socket-to-1.1.1.1 egress assertion
bash tests/lake/value_snapshots.sh
                                 # the transform tier: value snapshots for EVERY
                                 #   registered tenant, byte-identical to the
                                 #   Rust aggregate they replace — and §10, the
                                 #   shipped wrapper its timer runs
bash tests/lake/tenant_zone.sh   # the tenant zone's credential boundary, BOTH
                                 #   directions, seen green AND red — plus the
                                 #   90-day retention, mechanically
bash tests/lake/shipper.sh       # the shipper against a real bucket under the
                                 #   real tenant policy: killed mid-run and
                                 #   resumed, and the catalog role unable to
                                 #   read a byte of what it wrote (seen red)
bash tests/lake/phase3.sh        # Phase 3: a collection valued from the TENANT
                                 #   ZONE, which is now the ONLY way to value
                                 #   one — and §6, the change that never
                                 #   shipped, where the valuation MUST NOT move
                                 #   (and §6b, where shipping moves it)
bash tests/lake/deletion.sh      # the deletion path against a real bucket with
                                 #   VERSIONING ON: the drop leaves a noncurrent
                                 #   version of a really-shipped part, which is
                                 #   fetched back and proven unopenable. Seen red
                                 #   too — the same verify one step earlier
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
cargo run --bin pkdump-lake-derive -- diff --left a.sqlite --right b.sqlite \
    --exclude raw_derivation     # row-by-row, never byte-by-byte
cargo run --bin pkdump-ship -- run       # outbox -> tenant zone, every tenant
cargo run --bin pkdump-ship -- status    # what is unshipped, and any gaps
cargo run --bin pkdump-ship -- decrypt --key tenant/… --json
                                 # read one shipped part back
cargo run --bin pkdump-erase -- verify --tenant alice
                                 # attempt every read path; change nothing
cargo run --bin pkdump-erase -- delete --tenant alice --yes --reason "closed"
                                 # tombstone the key, drop the partition, PROVE
                                 #   it. Exit 4 = it ran and is NOT proven
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

Cargo workspace, ten crates (`crates/`):

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
- **pkdump-ship** — the shipper (`pkdump-ship`): the ownership outbox into
  the tenant zone, encrypted per tenant. Offline and bin-plus-lib; it is a
  separate crate because it must read SQLite, and `pkdump-lake` deliberately
  links no SQLite at all. See "The shipper" below.
- **pkdump-erase** — the deletion path (`pkdump-erase`): tombstone the
  tenant's key, drop their partition, and then prove the result by attempting
  every read path and requiring each to fail. Offline and its own crate for
  the same reason the shipper is — it needs the tenant credentials and the
  master key, and `pkdump-cli` must not link either. See "The deletion path"
  below.
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
network, which is what every Litestream gate uses. **The store that gets wedged
is prod's**: a CI gate's cleanup removes the directory, and
`pkdump_store_netns_repair` only ever runs for a store the shell opted into,
which prod never does. That is what failed `pkdump-value-snapshots@prod` nightly
from 2026-08-12 (pd-3zjt), silently — only user-defined networks are affected, so
`pkdump-refresh@` stayed green.

So the sharing is **removed**, not repaired around: activation writes each
non-prod store a `containers.conf` naming its own `[engine] tmp_dir`
(`CONTAINERS_CONF_OVERRIDE`, the merge-on-top spelling), which is the only knob
that moves the scaffolding — `--root`/`--runroot`/`--tmpdir` do not. Prod is
given no config at all. Podman **pins `tmp_dir` in the libpod DB at store
creation**, so a store that predates this keeps sharing until
`deploy/store-teardown.sh` is run once against it; the stale-netns repair remains
for that case (never `podman system migrate`: that kills the per-user pause
process prod's store shares), and `pkdump_store_netns_ensure` is what the two
jobs on a user-defined network call before they start — it probes with `podman
unshare --rootless-netns true`, repairs when only its own containers are on the
namespace, and refuses when anything else is.

**That repair is not finished when the restart returns** (pd-p39v). `systemctl
--user restart` returns when the *container* is running; Nessie is a JVM and does
not answer for another 30-40s, so the repair raced its own remedy — the job it
was clearing the way for died on a connection error, the condition self-healed by
the next run, and the unit paged for something already fixed. So a repair that
RESTARTED something waits for it to ANSWER: the caller passes a readiness command
(`pkdump_lake_catalog_answering`, `deploy/lake-lib.sh` — an HTTP GET of Nessie's
`/api/v2/config` from a throwaway container ON the network, the same path the job
itself takes), it is polled to a 120s deadline, and a deadline that passes FAILS
the repair rather than proceeding. A caller that restarts something and offers no
way to confirm it came back is refused rather than defaulted: an unverifiable
repair *is* this bug. The wait is paid only when something was restarted.

See `deploy/store-lib.sh`, the README's "Container storage", and
`tests/store/netns_split.sh` — the one store gate that is not hermetic, because
`[engine] tmp_dir` moving the scaffolding is a fact about podman, not about this
repo.

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

**Seventeen of the gates run in parallel, two at a time** (pd-2nl9; lowered from
three on 2026-08-13 — see `PKDUMP_CI_JOBS` in `deploy/ci-parallel.sh`) —
litestream, drill, alarming, recreate, upgrade, tenant-header, keys,
schema-version, the eight lake gates and refresh. They do not run where they are written: each
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
one of the seventeen gate scripts is still queued exactly once under a real tier.
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
  **"As it stood on date D" is the same rule, not a second one.** Only arm 1
  is date-sensitive in a way that changes its shape (`latest_prices` is a
  materialized "newest, full stop"), so `market_price_expr_from!` takes the
  feed relation and arm 3's cutoff, and both `market_price_expr!` (today) and
  `market_price_expr_asof!` (the value-history backfill, over its per-date
  `_prices_asof` TEMP table) are one line each. Backfill used to carry its own
  query with arm 1 alone, so every historical chart point silently lacked the
  curated and hand-entered prices today's point had — re-running it against
  prod rewrote 60 dates ~2.3% low (pd-3lg8). The gate is
  `value_history::tests::snapshot_today_and_backfill_agree_on_the_same_date`:
  both paths over one fixture, one date, rows compared column for column.

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

### The tenant zone

Everything above is the **catalog zone**. The same bucket also holds the
**tenant zone** under `tenant/` — holdings and valuations, and it is a
different object under different governance that happens to share a bucket
(pd-uz8q, item 2 of the inbound-leg epic pd-8lw7). The standing "tenant data
never enters the lake" rule is **restated by it, not broken**: that rule was
always about the catalog — cross-tenant, shared, retained forever.

```text
tenant/database_id=<id>/dataset=<holdings|valuations>/as_of=YYYY-MM-DD/part-NNNN.parquet
```

Five things are decisions, not implementation:

- **`database_id` is the FIRST partition**, above `dataset=`, so deleting a
  tenant is ONE prefix drop covering their holdings *and* their valuations.
  Derived artifacts inherit the deletion obligation; a layout with `dataset=`
  on top would make every future dataset another thing a deletion has to
  remember.
- **Plain partitioned Parquet, not Iceberg.** Iceberg records absolute paths
  in its metadata, so a later bucket split would mean rewriting manifests.
  It also gives up snapshots/time-travel deliberately — holdings want current
  state per tenant, deletable, not history. The catalog wants the opposite,
  which is why the catalog *is* Iceberg.
- **90-day retention, and it is a product limit rather than a tunable.** The
  catalog's indefinite window is justified by "we may need to rebuild any
  historical price"; nothing equivalent covers holdings. 90 days IS the
  backfill window, and it is what bounds a missed deletion's blast radius.
  Enforced by a lifecycle rule that `deploy/setup-tenant-zone.sh` applies
  **and then reads back** — a PUT that scoped itself to the wrong prefix
  succeeds identically to one that did not.
  **That read-back has three answers, and the exit code is which one**
  (pd-2hnp): 3 ABSENT, 4 CANNOT VERIFY, 1 present but wrong. It printed one
  sentence for the first two — "there is no retention rule" and "I am not
  allowed to look" are opposite facts, and the direction that hurts is the
  inverse of the one it was found in: once the rule IS applied, an operator
  whose credentials cannot read it is told it was never applied, and the repair
  for that is to apply or widen one. An unrecognised error is 4 as well, never
  3. The script also resolves and PRINTS its identity before acting, because
  with `--profile` omitted it acts as whatever is ambient — which on 2026-08-26
  was another project's backup user against this project's lake bucket.
  Applying retention needs a *third* identity (`role/pokedump-data`, via
  `AWS_PROFILE=pkdump`): both zone credentials deny the lifecycle actions by
  design. `deploy/TENANT_ZONE.md` §4a-§4b.
- **Separate credentials from day one**, and the boundary is a TEST. One
  bucket means a pair of IAM documents is the *only* thing separating the
  zones. `TenantZoneConfig` refuses an unset `PKDUMP_TENANT_AWS_PROFILE`, and
  refuses one equal to `AWS_PROFILE` — one profile for both zones is not a
  wide policy, it is no boundary, and it looks exactly like a correct one.
- **The explicit `Deny` statements are load-bearing.** A whole-bucket grant
  added *beside* either policy still cannot cross, because an explicit Deny
  beats any Allow — so a later broad grant elsewhere cannot silently widen a
  zone. Asserted (gate §6b), not assumed.

The prefixes and the window live in three places that cannot share code —
Rust (the shipper reads them at runtime), the policy documents (AWS reads
them) and the bash script — so `tests/lake/tenant_zone.sh` §8 holds them
together. A prefix changed in one and not the others does not fail loudly; it
silently widens a policy.

`tests/lake/tenant_zone.sh` is the gate, and every claim in it is seen **both
green and red**: the boundary in both directions, then the *same assertion
functions* re-run against a credential replaced by a whole-bucket grant; the
retention check refused three separate ways of getting the rule wrong. That gate's
own fixtures leave the zone **empty**, deliberately — it is about the
governance, and `tests/lake/shipper.sh` is what puts real (invented) holdings
through it. Runbook: `deploy/TENANT_ZONE.md`.

### The catalog/tenant zone guard

`tests/lake/tenant_isolation_test.sh` is the source-level boundary gate (lint
tier, hermetic, ~2s). It shipped with the lakehouse epic asserting one rule —
*the LAKE holds no tenant data* — over directory globs. The tenant zone makes
that premise false **by design**, so pd-7x83 re-cut its axis rather than
carving holes in it: it is now the **catalog zone** (`raw/`, `lake/` —
cross-tenant, shared, forever) against the **tenant zone** (`tenant/` —
tenant-keyed by construction, governed separately). Four things about it are
decisions:

- **The catalog zone keeps every assertion it had.** No Iceberg field is
  tenant-identifying, `crates/pkdump-lake` links no SQLite at all, the Python
  write path imports no `sqlite3`, the derive resolves no tenant.
- **The tenant zone's rules are INVERTED, not relaxed.** It must be
  tenant-keyed — every key builder in `tenant.rs` takes a `database_id`,
  because that prefix is what a deletion drops — and it must resolve no
  identity: being handed an id is the contract, looking one up is not. The
  shipper reaches no catalog prefix, no catalog entry point and no catalog
  credential; the online path (`pkdump-db`, `pkdump-server`, `pkdump-keys`)
  links neither zone, which is what makes the outbox the only way holdings
  leave a collection.
- **The carve-out is by ZONE and it is TOTAL.** Every Rust file in
  `crates/pkdump-lake` must be classified into exactly one zone, and §12 fails
  if the three lists do not cover the directory. A per-file exemption list is
  what erodes; a classification that has to cover the directory cannot be
  added to silently.
- **It has been seen red.** `tests/lake/tenant_isolation_selftest.sh` (lint
  tier, ~45s) copies the source trees, injects ONE violation at a time and
  requires the *specific* assertion to be the one that fails — 25 of them,
  including a tenant column added to a catalog table and every fail-closed
  case. Four cases assert the opposite: the tenant zone being legitimately
  tenant-keyed must fire nothing, because a guard whose first contact with
  real work is a false positive is a guard that gets an exemption list.

The credential half of the same boundary is not here and cannot be — it is two
IAM documents against a real bucket, and `tests/lake/tenant_zone.sh` §4-§6 is
where it is asserted, in both directions, seen red.

### Key custody for the tenant zone

`pkdump-keys` is crypto-shredding for the tenant zone, defence in depth beside
the partition-drop deletion. One **master key** on the box (mode 600, in
`~/.config/pkdump/<instance>/` beside `litestream.env`), per-tenant keys
**derived** — `HKDF-SHA256(master, database_id)` — and a `tenant_key` table in
`registry.sqlite` mapping `database_id` to `active | tombstoned`. No key
service, no per-tenant secret to rotate or lose. Runbook: `deploy/KEYS.md`.

The whole thing is arranged around one property:

> **A lost key is indistinguishable from a deleted tenant, by design.**

True of the ciphertext, and it must not become true of the *system* — the two
call for opposite responses (do nothing; page somebody). Four things hold it:

- **Backup and destruction are different code paths**, and the difference is
  structural rather than nominal. `backup.rs` is about a **file** and never
  opens the registry; `destroy.rs` is about a **row** and never opens the key
  file. `crates/pkdump-keys/tests/separation.rs` reads both sources and fails
  if either grows a reference to the other's world, so it stays true of code
  nobody has written yet. Sharing a helper is how "we lost it" becomes "we
  destroyed it"; sharing a name is not.
- **The order of the two lookups in `derive::tenant_key` is the design.** The
  registry is consulted FIRST, so a tombstone answers *even with the master
  key gone* — and a live tenant whose key is missing answers
  `MasterKeyUnavailable`, never `Tombstoned`. Invert it and the two collapse
  exactly when somebody is trying to tell them apart.
- **`KeyError::is_deliberate_revocation()` is the only sanctioned way to ask
  "was this on purpose".** It answers `true` for one variant. Item 8's
  deletion path goes through it; `Err(_) => "deleted"` is one keystroke from
  turning a missing backup into a compliance claim.
- **Absence is not permission.** An unregistered `database_id` is refused, not
  derived for — a registry restored empty is missing its tombstones too, and
  fail-closed is the direction where that is loud. The tombstone table has no
  foreign key to `user`, deliberately: it must outlive the row it names or
  `tenant purge` would un-revoke the key it just destroyed.

The backup **mechanism is matched, not invented**: the master key goes in the
operator's password manager, exactly like the Litestream bootstrap key, and
comes back in `deploy/RESTORE.md` Scenario C. Nothing replicates it to S3 —
that is where the data it protects lives.

The trade is stated, not hidden: one master key means destroying *it* destroys
everything, which is why `keys init` refuses to overwrite (no `--force`) and
why the destruction path cannot reach the file. A tombstone stops OUR code
deriving a key; it is not itself an unrecoverable erasure. Stored per-tenant
random keys are a documented future option — a re-encrypt of at most 90 days,
the zone's whole retention window — and are deliberately not built.

Gates: the hermetic crate tests (45 of them, including RFC 5869 vectors and
the separation gate) and `tests/keys/run.sh`, the container tier, which stats
the **deployed** key file for mode 600 — "the code sets 600" and "the file is
600" are different claims — and re-runs the crux on a real box.

### The shipper

`pkdump-ship` is what moves the ownership outbox into the tenant zone
(pd-dxn3, item 4 of the inbound-leg epic). It is the seam between the outbox
(pd-5m54), the zone (pd-uz8q) and key custody (pd-ulds), and it is the ONLY
thing in the workspace that writes under `tenant/`.

```text
tenant/database_id=<id>/dataset=holdings/as_of=<date>/part-seq-<from>-<to>.parquet.enc
```

It is a **separate crate** for a structural reason, not a tidiness one: the
shipper must open a tenant's SQLite, and `crates/pkdump-lake` links no SQLite
crate at all — which is what makes pd-cgi9 §1 ("no lake write path can open a
tenant database") true by construction rather than by review. `pkdump-lake`
still owns where the bytes go and hands out the handle
(`open_tenant_zone`); `pkdump-ship` decides what is in one.

Five things are decisions:

- **It reads no clock.** `as_of` is the UTC date of the event's own
  `occurred_at`, and a part is a maximal run of consecutive rows sharing that
  date. So re-shipping a range on a later day lands in the partition it landed
  in the first time, and there is no `--date` for a scheduler to lend
  (contrast the transform tier, where choosing the day is the whole point).
- **A part is addressed by the sequence range it carries, never an ordinal.**
  Delivery is at-least-once — the cursor is written *after* the PUT, so a
  crash in between re-ships a part, which is the direction that repeats events
  instead of losing them. An ordinal would make the retry a second object
  beside the first; a range makes it land on the object it is retrying. That
  is the whole of idempotence.
- **The encryption is deterministic**, so the retry is byte-identical rather
  than merely equivalent. AES-256-GCM under the derived key, with a synthetic
  nonce hashed from the object key and the plaintext, and **the object key as
  associated data** — so a part authenticates only under the prefix it was
  written to, and one moved into another tenant's partition fails to open
  rather than decrypting into the wrong holdings. `.parquet.enc` on every key
  in the zone, because every object in it is sealed.
- **The cursor and the gap ledger live in the tenant's own database**
  (`ownership_outbox_cursor`, `ownership_outbox_gap`), beside the outbox they
  point into, and are excluded from the portable JSON envelope for the reason
  the outbox is: transport state, not collection state. An envelope carrying a
  cursor would make a restore skip events it had never shipped.
- **A gap is recorded, alarmed, and shipped past.** `seq` is gap-free by
  construction, so a hole means an event was **lost**. The missing range is
  written to the ledger *before* the cursor moves past it — once past, nothing
  can detect it again — and the run ends at exit 3. It does not stop: the
  missing rows are already gone, and withholding the rows that survive would
  be a second loss caused by detecting the first.

**Four exit statuses, because there are four things to say** — 0 clean, 2 some
tenants skipped (warned, `SuccessExitStatus=2`), 3 SEQUENCE GAP (paged, with
its own message naming the tenant and the range), 1 the run never started or
shipped nobody at all. A tombstoned tenant is **not** an anomaly: the key
refuses to derive, the tenant is skipped, and the run is still clean. An
*unregistered* one is a warning naming `pkdump keys register`, because absence
is not permission.

It does not branch on where an outbox row came from. Item 5 (backfill/redrive)
emits synthetic events *through the outbox* with a provenance column; a
consumer that treated those differently would make the backfill a second code
path instead of the same one. It does **carry** that column — a part is
`pkdump_db::outbox::Event`'s seven fields, all of them, and there is exactly
one struct for an outbox row (pd-mixm). Two spellings of one table is how a
column added to it stops reaching the bucket; one means the schema fails to
compile instead. Carrying is not branching, and it is what lets
`encode::decode` compose with `pkdump_db::outbox::project` — so a reader
reduces a shipped part with the SAME implementation of the resolution rule
the collection's own gate uses, rather than a second one that agrees today.

**A part carries `source_table` beside `row_id`, and that pair is the
identity** — `row_id` alone is not (pd-4gop). `collection` and
`sealed_collection` number their rows independently, so the first single and
the first sealed lot are both `row_id = 1`, which is the ordinary shape of a
collection rather than a corner case. A reader that grouped a holdings part by
`row_id` would merge two unrelated streams into one projection and produce a
plausible wrong number. The shipper itself keys nothing on either: its only
key is `seq`, unique across the whole outbox.

**It is installed on a timer and armed by nobody yet** —
`pkdump-ship@<instance>.timer`, at 07:00 with the rest of that wave, `After=`
the landing, the derive and the price build. Since pd-i08u its wrapper runs
BOTH halves of the round trip (`pkdump-ship run`, then `pkdump-ship holdings`)
and the transform is the unit that waits on it, so the chain is land → derive
→ prices → **ship (+ read back)** → transform. Do not arm it on prod before
the backfill has run: the shipper ships the OUTBOX, an existing collection's
outbox starts empty (pd-whsw), and armed early it faithfully ships every
change made from tonight and nothing anybody already owns. `pkdump outbox emit
--all --all-tenants` (pd-385w) is what makes the outbox describe the
collection that is already there; arming is the step after it, per instance.
**And it is no longer optional beside the transform**: with the online
holdings read deleted, a box running `pkdump-value-snapshots@` without this
unit records no value history for anybody.

Gates: `cargo test -p pkdump-ship` (hermetic — planning, the envelope, Parquet,
and `tests/shipping.rs`, which proves gap detection, idempotence, resumability
and encryption-under-the-right-key over a `DirStore`, plus the seam with item
5: a backfilled collection ships as ordinary events, dated from the rows' own
timestamps rather than the day the backfill ran) and
`tests/lake/shipper.sh` (container tier — the shipped image against a real
MinIO under the real tenant policy, a real process killed mid-run and resumed,
the catalog role's denial seen both green and red, and `deploy/ship.sh`'s four
exit statuses).

### The deletion path

Deleting an account from the tenant zone is `pkdump-erase` (pd-qbrf), and it
is **two acts plus a proof**, in that order:

```
1. tombstone   registry.sqlite : tenant_key(<id>) -> tombstoned
2. drop        tenant/database_id=<id>/  emptied, object by object
3. verify      every read path attempted, every one required to fail
```

Five things about it are decisions, not implementation:

- **Neither act is sufficient, and the verification says so separately.** A
  drop without a tombstone leaves a live key, so any copy that survived
  anywhere is readable; a tombstone without a drop leaves the objects there,
  and the design says the drop is the erasure. The proof names `derivation`
  and `partition` as different checks for exactly that reason.
- **The tombstone goes FIRST**, and the order is the difference between an
  interrupted deletion that is safe to resume and one that reverses itself.
  There is no transaction across SQLite and an object store. Tombstone-first,
  a crash leaves a tenant nothing can derive a key for and whose remaining
  objects are ciphertext — *more* deleted than intended, and a re-run
  finishes it. Drop-first, a crash leaves an ACTIVE tenant whose partition
  vanished: their key still derives, the shipper still ships, and tonight puts
  fresh holdings back under a prefix that was supposed to be gone.
- **"Proven" means the proof cannot be vacuous**, and there are two ways it
  could be. A box with no master key derives nothing for *anybody*, so
  `machinery` runs first and the stray-copy check refuses to conclude anything
  without it. And the `derivation` check insists on `is_deliberate_revocation`
  rather than on any error — an unregistered id refuses too, and accepting
  that would let "we never heard of them" be filed as "we destroyed their
  data". That is pd-ulds's distinction enforced from the reader's side.
- **The claim is checked against a copy that SURVIVED**, not only against an
  empty prefix. The drop has to find every copy; the tombstone does not, and
  that asymmetry is the whole reason crypto-shredding is in the design. So
  `--stray <file> --stray-key <key>` opens real bytes taken before the
  deletion, and `tests/lake/deletion.sh` runs on a **versioned** bucket where
  the drop genuinely leaves a noncurrent version behind. A "copy" that is not
  a sealed object makes the check FAIL rather than pass — a text file does not
  open either.
- **Exit 4 is not exit 1.** A deletion that ran and cannot be proven is a
  different event from one that never started: the data may well be gone and
  what is missing is the evidence, so the two need different first questions.
  `deploy/erase.sh` alarms on 4 itself, because there is no unit here and
  therefore no `OnFailure=`.

**There is no timer, deliberately.** Every other container job under `deploy/`
is fired by a calendar; a deletion is an act somebody decides to perform on
one named account, and a scheduled deleter is a thing that can delete the
wrong account at 3am with nobody watching.

The **online** half — releasing the handle, removing the collection database
and its replica — stays `pkdump tenant detach` / `pkdump tenant purge`. Doing
it here would put a tenant-zone credential and the master key inside the
binary that serves requests.

`ObjectPurge` (`pkdump-lake`) is the third and narrowest zone handle: list a
prefix, delete a key, and nothing else — no `get`, so the job that deletes a
tenant's holdings never reads them. Confinement to ONE tenant's prefix is a
level up, in `pkdump_erase::sweep`, which refuses a key outside it fatally
rather than skipping it.

Gates: `cargo test -p pkdump-erase` (hermetic — the sweep, both vacuity
guards, and `tests/deletion.rs`, which runs the whole path over holdings the
real shipper really wrote, then runs every check one step EARLIER and requires
all of them to report the path open) and `tests/lake/deletion.sh` (container
tier — the shipped image against a versioned MinIO under the real tenant
policy, the objects gone as seen by the bucket root rather than merely hidden
from the role that deleted them, the surviving version fetched back and proven
unopenable, and `deploy/erase.sh`'s three exit statuses). Runbook:
`deploy/DELETION.md`.

### The transform tier

`lake/src/pkdump_lake/value_snapshots.py` is the first job that *reads* the
lake: it values **every registered tenant's** collection from `catalog.prices`
and `catalog.sealed_prices` at a pinned Nessie commit and writes
`collection_value_snapshot` into that tenant's own database. Three rules hold
it in shape:

- **The unit of work is the registry, not the current user.** The refresh used
  to end with a step 7 that snapshotted the one collection `$PKDUMP_USER`
  resolves to, and reported success for everybody (pd-s5yn). Any successor to
  it walks the registry, or it has reintroduced the bug.
- **A failing tenant is logged and skipped; the run finishes and exits 2.**
  Exit 0 means every tenant, 1 means the run never started **or snapshotted
  nobody at all** — "some tenants" and "nobody" are different nights, and a
  warning is the wrong volume for the second. Silence over a half-completed
  run is the failure mode being replaced.
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
  asserted to only ever READ the lake. Since the inbound leg that guard's axis
  is the **catalog zone against the tenant zone** rather than the lake against
  everything else — see "The catalog/tenant zone guard" below.

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

`catalog.prices` itself is still built by hand between the two (pd-up36).

### Sealed is its own value series (pd-bbv7)

A collection is worth its loose cards **plus its sealed product**, and the
chart reported only the first — prod read $10,636.81 with $10,351.47 of sealed
product across 140 units invisible beside it. The fix is a second series, not a
bigger number:

- **`catalog.sealed_prices`** is a second Iceberg table, written by the SAME
  run of `pkdump-lake-build-prices` from the SAME raw partition, with the
  identical `pkdump.raw-runs` provenance — so the two tables cannot silently
  come from different nights. Grain `(product, price_type, observed_date)`,
  no `sub_type_name` (mirroring `shared.sealed_prices`, which is
  `UNIQUE(product, observed_at)`), partitioned the same way. **The sealed
  price bytes were already in `raw/`**: `tcgcsv.rs::import_prices` splits one
  payload, so this is no new upstream, ingest or fetch. What counts as sealed
  is read from the same partition's `products` dataset by the same
  discriminator the two Rust importers use (a `Number` for category 3, a
  `CardType` for Japan's 85, category read off the part's URL); a missing
  products partition is **fatal**, because classifying nothing as sealed
  produces a table saying every tenant owns nothing sealed.
  **`catalog.prices` is unchanged** — it still holds every product's prices —
  which is what makes the cards half provably untouched.
- **`zone_sealed_holdings`** is the second staging table the read-back writes,
  from `sealed_collection`, beside `zone_holdings`. Two tables, never one:
  `row_id` is unique only within a source. A source with no staging table is
  declined **by name** now, per table, because the single number it printed
  before is how sealed stayed invisible.
- **`dimension='sealed'`** in `collection_value_snapshot`, bucket NULL.
  `card_count` is **UNITS** (`SUM(quantity)`) not lots; `cost_basis`
  multiplies by quantity (a lot's price is per unit); there is **no condition
  multiplier**, because nothing in the app prices a box off its condition; and
  a lot whose product nobody quotes is **skipped by the sum and counted in the
  units**, never valued at zero. Written only when the tenant owns some, the
  same rule `set` and `binder` buckets follow.
- **`dimension='all'` still means the loose cards**, to the cent. Every row
  ever written under it was computed over cards alone and widening it would
  restate months of chart silently. There is **no stored combined total**: the
  `all` dimension of `GET /api/collection/value-history` answers with two
  series (cards at `bucket = null`, sealed at `bucket = "sealed"`) and the
  chart and the home page sum them at read time.
- **Never join a sealed holding through `tcgcsv_products`.** That table holds
  single-card products; no sealed id is in it, so such a join drops every
  sealed row and reports a collection with no sealed product in it. The
  `sealed` dimension is one bucket and therefore joins nothing at all; where a
  catalog attribute is ever wanted it comes from `sealed_products`, by
  `product_id`.
- One spelling of what a sealed lot is worth —
  `pkdump_db::prices::sealed_market_price_expr_from!`,
  `COALESCE(market_price, mid_price)` off ONE observation — spent by the
  `/sealed` page and by both value-history paths.

**An existing box gets sealed history only if it asks.** The sealed line
starts empty and advances from the first night the transform runs with this
build. `pkdump data backfill-value-history` reconstructs it from
`shared.sealed_prices`, which prod already holds for every past night — but it
rewrites the *card* rows for those dates too, so it is an explicit operator
step rather than something a deploy does. That rewrite is safe now for the
reason pd-3lg8 made it safe: both value-history paths spend the same two price
rules, and `snapshot_today_and_backfill_agree_on_the_same_date` /
`…_on_the_sealed_row` hold them to it.

The hard gate is that the cards must not move, and it is stated as a real
before-and-after rather than against a constant:
`value_history::tests::sealed_holdings_do_not_move_the_cards_dimensions`
(hermetic — one collection snapshotted with its sealed lots and again without,
`all`/`set`/`binder` required identical) and `tests/lake/value_snapshots.sh`
§5b, which empties the sealed staging table and re-runs the real transform.
§5c is the inverse — a transform that never wrote a sealed row would pass §5b
— and bob, who owns no sealed product, is the tenant who must get no sealed
row at all. `tests/lake/prices.sh` §5b asserts the two price tables name the
same raw run, and the verifier now compares `catalog.sealed_prices` to
`shared.sealed_prices` **exactly, in both directions**.

### Phase 3: valuing a collection from the tenant zone

The transform reads its holdings out of the **tenant zone** (pd-szh2) rather
than out of `collection`. That is Phase 3 of the cycle — land raw, build the
catalog, ingest tenant state, *compute valuations*, publish back — and it
closes the half of the loop the epic exists for: the write moved offline, the
read stayed online.

It shipped **alongside** the online path behind `--holdings`, with a
`--compare` that valued every tenant both ways and diffed the rows; that came
back clean (2 tenants, 7 rows, 0 differing) and pd-i08u deleted the online
read. **There is now one valuation path and no flag selects it** — no
`--holdings`, no `--compare`, and no fallback when the zone has not been read.
A deleted path that leaves its flag behind is a path a runbook can still reach
for, and the argument for the deletion was that one number should have one
provenance.

Five things about it are decisions:

- **The seam is a table, because neither language may implement the other's
  half.** The envelope, the key derivation and the resolution rule have one
  implementation each and it is Rust (`pkdump-ship`); `catalog.prices` is
  Iceberg and `pyiceberg` is the only client here. So `pkdump-ship holdings`
  reduces the zone with `pkdump_db::outbox::project` into `zone_holdings`, and
  the transform's existing SQL reads that name instead of `collection`.
  **One token differed**, which is what made a difference between the two
  valuations a difference in *holdings* and not one in arithmetic. A
  from-scratch offline computation could differ for a dozen reasons and the
  proof would have to rule out each. That is why the switchover was a table
  name and nothing else, and why it stayed reviewable as its own change.
- **`zone_holdings` is derived, never declared.** Created from `collection`'s
  own `pragma_table_info`, so a column added there reaches it with nothing to
  remember — a hand-written mirror in `schema_user.sql` would be the *third*
  place the collection's shape lives (`encode.rs` declined to be the second).
  Being created also means it carries no triggers, so materialising cannot
  emit outbox events and a Phase 3 run cannot feed itself back into the zone
  it just read. Both it and `zone_holdings_run` are in `TRANSPORT_TABLES`.
- **A stale materialisation is refused, not valued.** `zone_holdings_run.
  max_seq` behind `ownership_outbox_cursor.shipped_thru` means the read
  predates the last ship, and left alone it would value today's collection at
  older holdings while every number looked reasonable. That is the quiet
  failure this item could otherwise become. A tenant whose *outbox* is ahead
  of the zone is not refused: that means the shipper skipped them, and the
  shipper says so in the same nightly run. Refusing here would report one
  shipping failure twice and withhold a valuation of holdings that are
  genuinely what the offline side was told.
- **The read-back is scheduled with the shipment, not on its own.**
  `deploy/ship.sh` runs `pkdump-ship run` and then `pkdump-ship holdings`:
  `zone_holdings` is only correct when it was read immediately after a ship,
  and both halves need the master key and the tenant profile that nothing else
  on the box holds. Two units would be two chances to arm one of them. The
  wrapper's four statuses compose 3 > 1 > 2 > 0 — a GAP outranks even a failed
  read-back, being the only one no later run can repair — and a shipment that
  shipped *nothing* does not attempt the read-back at all.
- **What replaced the equivalence proof is stronger than it was.** With one
  path there is no second computation to diff against, so `tests/lake/
  phase3.sh` §5 and `tests/lake/value_snapshots.sh` §5 both diff the zone
  valuation against the rows Rust `value_history::snapshot_today` computed
  over the same collection — a holding lost, duplicated or wrongly resolved
  anywhere between the outbox and `zone_holdings` is a changed number. And
  **phase3.sh §6 is the section that matters**: a collection changed without
  shipping must leave the valuation UNMOVED, and §6b requires shipping and
  reading back to move it. A Phase 3 that quietly read the live table fails
  the first half; one that is frozen or cached fails the second.

What the zone does not carry: the condition multiplier and
`manual_prices`/`user_printings` are read from the tenant's own database
(neither applies to a sealed lot — see the sealed series above). **Deleting the online holdings read did not remove the
tenant-database dependency** — the valuation still opens each tenant's SQLite
to read those, to read `zone_holdings`, and to write the snapshot back. Phase
3 narrowed which table the *copies* come from and nothing else. Runbook:
`deploy/TENANT_ZONE.md` §7.

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
- **The upstream fallback is temporary and LOUD.** A URL missing from `raw/`
  means coverage has regressed; the run says so per-URL and in a summary,
  `deploy/derive.sh` pushes a warning, and `--no-upstream-fallback` makes it
  fatal. Item 4 of the epic removes it, as its own change, once row-identity is
  proven in production. **Do not remove it as a side effect of anything else.**

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
from the refresh unit's own bounds, and the chain is land → derive → ship →
transform.
The derive unit has **no `SuccessExitStatus=`**, unlike the transform: that job
writes N tenant databases and "some of them" is normal, this one writes ONE
catalog and a smaller catalog reads as cards that do not exist.

Gates: `crates/pkdump-lakehouse/tests/row_identical.rs` (hermetic — row-identity
over two days, idempotence, reproducing an older date, both refusals, a
corrupted payload, the fallback loud one way and fatal the other) and
`tests/lake/derive.sh` (the container tier, shipped image, `--internal`
network, socket-to-1.1.1.1). Runbook: `deploy/LAKE.md` §8.

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


### When an upstream is having a bad day

On 2026-08-11 `api.pokemontcg.io` answered 5xx to ~45% of requests for most of
a day. Neither client retried anything and the tail is fetched first, so a
single 500 on `/v2/sets?page=1` ended the whole refresh in its first second —
**before TCGCSV**, so no prices were imported at all. A day's prices cannot be
re-fetched later; a day's set list can (pd-nons). Two changes, and three things
about them are decisions:

- **A bounded retry is not fallback logic.** `crates/pkdump-ingest/src/retry.rs`
  retries transport failures, 429 and 5xx — four attempts, 500ms doubling to an
  8s cap, no jitter — and every other non-2xx exactly once, because a 404 is a
  fact about the URL. Nothing is defaulted or substituted; when the budget is
  spent the original error propagates as it always did. It lives in
  `landing::fetch_bytes`, the one place any client executes a request, so a
  client added later cannot forget to retry. `PKDUMP_HTTP_RETRY_ATTEMPTS` /
  `PKDUMP_HTTP_RETRY_BASE_MS` widen it without a rebuild — on the `podman
  exec` or on the container, since the refresh unit execs into a running
  instance.
- **Only the FINAL failure reaches the manifest.** `complete` is computed from
  `failures.is_empty()`, so logging the attempts a retry recovered from would
  mark a whole night's raw partition incomplete for a hiccup it survived. A
  failure record means "this URL was not fetched". The retries are still loud —
  on stderr, in the unit's journal.
- **The tail may fail without ending the run; TCGCSV may not.** `acquire`
  carries a tail error into `Report.tail_error` instead of returning it, so the
  perishable half is still acquired and every local phase still runs.
  `pkdump data refresh` then exits **2** (0 whole / 2 partial / 1 failed) and
  says so; `pkdump-lake-derive` bails and refuses to record provenance, because
  its claim is that the catalog *is* the partition's derivation.

**Do not "fix" this by fetching TCGCSV first.** That was tried and reverted.
`tcgcsv::import_groups` links each group to the `sets` rows already in the
database, so running it ahead of the tail on a catalog that lacks them leaves
every `tcgplayer_groups.set_code` NULL until the *next* derivation — and an
offline rebuild from `raw/` starts from an empty catalog every time, so a
replayed catalog stops matching the online one it exists to reproduce.
`crates/pkdump-lakehouse/tests/row_identical.rs` fails on it. The exposure that
reordering would close is a tail that *hangs* rather than errors, and that is
already bounded by the 30s request timeout times the retry budget against
`TimeoutStartSec=1800`.

Exit 2 is deliberately **not** wired to `SuccessExitStatus=` on
`pkdump-refresh@`, unlike the transform tier — that job has a wrapper that
pushes its own warning and this one does not, and a set list that silently
stopped advancing is exactly what nothing else on the box would report. A
partial run still pages. What changed is that the night is no longer lost.

Gates: `crates/pkdump-ingest/tests/retry.rs`,
`crates/pkdump-derive/tests/tail_failure.rs`, and
`tests/refresh/tenant_bytes.sh` §9 (container tier — the shipped binary's exit
status, its retry count against the `/v2-down` fixture prefix, and that the
derivation continued past the failure).


### The ownership outbox

The inbound leg — online tenant state into the lakehouse — starts at
`ownership_outbox` in `schema_user.sql` (pd-5m54). Every change to a tenant's
**holdings** is appended there as an event, in the SAME TRANSACTION as the
change. The offline side is fed from those events, so it is eventually
consistent *by construction*: a dual write to SQLite and a bucket has no
atomicity, and the disagreement a crash leaves behind is undetectable.

**Both halves of the holdings are in it** — `collection` (singles) and
`sealed_collection` (sealed product), listed in `outbox::SOURCE_TABLES`.
Sealed was deferred by pd-5m54 and settled IN by pd-4gop: sealed product is a
holding like any other — a catalog id, a value that moves with the market —
and a collection that reports a tenant's worth while silently omitting it
**under-reports**, which is the wrong direction to be wrong in. Whatever the
outbox emits for singles it emits for sealed. It cost three more triggers, one
entry in that list and no schema change, which is exactly what `source_table`
was put there for.

**The writer is triggers, not the call sites, and that is the whole point.**
A trigger fires inside the statement's own transaction, so there is no
instant at which a holding has changed and the event has not — no window to
crash in, and nothing to remember to call. It also covers the paths that
write those tables in raw SQL (`orders.rs`, `import.rs`, `json_backup.rs`,
the fixture seeder) and the ones no Rust performs at all (`ON DELETE SET
NULL` from `binders`/`decks`), without any of them knowing the table exists.

Five things about it are decisions:

- **`seq` is AUTOINCREMENT**, so a number is never reused after the shipper
  trims a shipped prefix and a missing one means an event was LOST rather
  than deleted. A rolled-back write burns nothing (`sqlite_sequence` rolls
  back with it) — asserted, because phantom gaps would make gap detection
  useless. `occurred_at` is metadata; `datetime('now')` ties inside one
  transaction and cannot order anything.
- **One sequence over both sources, and `row_id` is unique only WITHIN
  one.** The two tables number their rows independently and both start at 1,
  so **a consumer projects on the `(source_table, row_id)` pair.** Replaying
  on `row_id` alone silently merges a single and a sealed lot that share a
  number, which is the normal case rather than a rare one — the atomicity
  gate fails on iteration 0 if you try it.
- **`payload` is the whole row as JSON** — post-image for insert/update,
  pre-image for delete. Whole, so a later consumer needing a column nobody
  anticipated costs no schema change here, and so nothing can be silently
  omitted: `outbox.rs` asserts the payload keys against `PRAGMA
  table_info(<source>)` for every source, which is what catches a column
  added to a table and forgotten in the hand-written `json_object` lists.
  A sealed lot's `quantity` is carried, never expanded — one lot is one
  event, and a consumer that wants copies multiplies.
- **The outbox is not collection state**, so `pkdump export --json` does not
  carry it and an import neither restores nor clears it — the import's own
  deletes and inserts fire the triggers and describe the restore correctly.
  This is the one exception to pd-yj40's "no exclusion list in the exporter",
  and it is in one place (`json_backup::envelope_tables`, filtering on
  `outbox::TRANSPORT_TABLES`). Both holdings tables *are* collection state
  and both stay in the envelope.
- **`outbox::SOURCE_TABLES` is a claim about the schema, not a copy of it.**
  `every_triggered_table_is_emittable` reads the triggers off `sqlite_master`
  and compares both directions, so a third source wired up and not declared —
  or declared with no triggers — fails in a second. That gate is what made
  this change land safely on top of the emitter: it went red the moment the
  sealed triggers arrived and stayed red until the backfill covered them, so
  singles could not be backfilled while sealed was silently missed. Adding a
  source is: the table, three triggers, one entry, and the gates come free.
  The entry's second element is the column that dates a row which has never
  emitted an event — `acquired_at` for singles, and `added_at` rather than
  `purchase_date` for sealed, because it is NOT NULL and machine-written, so
  `strftime` always parses it.

Changing a trigger body needs a deliberate `DROP TRIGGER` in the schema file
— `IF NOT EXISTS` will not replace one an existing collection already
carries, and a stale trigger writes a stale payload forever.

Gates: `outbox.rs`'s unit tests (every mutation path on both sources, the
payload-coverage comparison, the shared interleaved sequence, the shared
`row_id`, rollback, concurrent writers contending on both tables, the
envelope rules, the `SOURCE_TABLES` drift check) and
`crates/pkdump-db/tests/outbox_atomicity.rs` — a child process writing
batches, SIGKILLed mid-transaction, the outbox replayed from seq 1 and
compared to the holdings tables row by row, sixteen times. It fails unless at
least one kill actually landed inside a transaction, because a crash test
that never crashed anything proves nothing, and unless BOTH sources left rows
behind, because a run that only wrote singles would stay green with the
sealed triggers deleted.

**Every batch mutates both sources inside ONE transaction**, rather than
alternating batches. A kill has to be able to land *between* the two tables'
writes — precisely where a per-table outbox would tear — and it keeps the two
tables' ids in lockstep, so every single has a sealed lot sharing its
`row_id` and a projection keyed on `row_id` alone cannot accidentally pass.

**The child acquires, SELLS and deletes** — all three ops, and the middle one
is the one to keep. An insert or a delete lost in a crash shows up as a wrong
row COUNT; a stale UPDATE payload leaves the counts identical and one row's
contents wrong, which is the only divergence the projection can carry
silently. A child that only inserted and deleted let the update trigger be
deleted outright with the gate still green. The in-flight marker carries two
counts for the same reason — a kill inside an update batch moves no rows in
or out, and against a single count would look like a batch that never
started.

Nothing ships the outbox yet. The shipper is its own change (pd-dxn3), and it
is the thing that has to honour the `(source_table, row_id)` pair.

### Backfill, redrive and DR reconcile are ONE command

The triggers above only fire on FUTURE mutations. On a collection that
already holds cards when they are created — which is every existing box —
every current holding generated no event and never will. **Arm the shipper
against that outbox and the tenant zone silently covers only post-deployment
changes, and every valuation computed from it under-reports.** That is the
gap `pkdump outbox emit` closes (pd-385w), and it is why this must ship
before the shipper is armed on prod.

```bash
pkdump outbox emit --all                # backfill this collection
pkdump outbox emit --all --all-tenants  # ...every registered one
pkdump outbox emit --seq 1200..1310     # redrive a slice the shipper lost
pkdump outbox emit --row collection:481        # redrive one holding
pkdump outbox emit --row sealed_collection:12  # ...sealed is one too
pkdump outbox status                    # what has been emitted, and when
```

**`--row` names a `TABLE:ID` pair and does not default the table** (pd-4gop).
A bare row id names one row in *each* holdings table — `collection` and
`sealed_collection` number their rows independently and both start at 1 — so
`--row 481` would redrive a single and an unrelated sealed lot together.
Defaulting to `collection` would be worse than ambiguous: it is the same
"singles are the real holdings" assumption the sealed source exists to delete.
The operator always has the pair to hand, because they read it off the event
they are redriving, and the ledger records `row:collection:481` for the same
reason — `row:481` would not say which holding was covered.

**One command over a scope, not three tools**, and that is the point rather
than tidiness: the rare uses run under pressure, at 3am, after something is
already broken. A backfill that shares its code with the everyday path has
been exercised every day; a separate `--repair` script has been exercised
never. Backfill, redrive and DR reconcile differ only in the scope argument.

What makes any of it tractable is the payload being **the whole row**, not a
delta — replay is then an upsert to the same value, where `+1` applied twice
is a corruption. If you find yourself shrinking the payload for size, stop:
that trade destroys backfill, redrive and DR reconcile together.

Four rules, and none of them is an implementation detail:

1. **Through the outbox, never straight to the zone.** `emit` appends
   ordinary outbox rows and writes no holding. Two writers with different
   code paths means the rare one is untested, and the zone can then disagree
   with the outbox with nothing able to detect it.
2. **Provenance without different handling.** Every event carries `source` —
   `trigger`, `backfill` or `redrive`. **The shipper must NOT branch on it.**
   The moment it does, backfill stops being the same path. The column
   defaults to `'trigger'` rather than being named in the three trigger
   bodies, because `IF NOT EXISTS` will not replace a trigger an existing
   collection already carries — every writer that is not `emit` is a trigger,
   so the default IS the rule.
3. **Last-write-wins by `occurred_at`, tie-broken by `seq`** — implemented
   once, in `outbox::project`, which is the reduction the tenant zone holds.
   A redrive appends a snapshot with a NEW, higher `seq`; resolving by `seq`
   alone would let stale state overwrite a live mutation that landed in
   between. Resolving by `occurred_at` cannot, **because an emitted event
   carries the row's own last-known change time** — the newest `occurred_at`
   the outbox already holds for it, else the row's own timestamp column
   normalised through `strftime`, else a floor. Never the moment of
   re-emission. Every read during an emit is bounded to the `max(seq)` seen
   when the transaction opened, or the run's own events would feed back into
   that lookup and date themselves from what they just wrote.
4. **Re-running is safe but not silent.** Every run lands a row in
   `ownership_emit_log`, in the same transaction as its events; a second full
   backfill without `--force` is refused, naming when the first completed.
   Idempotent does not mean invisible.

Two more decisions:

- **The unit of work is the registry, not the current user.** `--all-tenants`
  walks `pkdump tenant list`. This is pd-s5yn's lesson applied before the
  bug: a backfill of the one collection `$PKDUMP_USER` resolves to would
  report success for everybody while every other tenant stayed invisible to
  the zone. Under that flag a failing tenant is named and skipped, the run
  finishes, and it exits **2** — 0 whole / 2 partial / 1 failed or never
  started, the same three answers the transform tier gives. Over ONE
  collection there is no partial state to be in, so a failure exits 1; the
  distinction follows the *flag*, never the number of tenants that happen to
  be registered, or the exit code a runbook was written against would change
  the day somebody signs up.
- **A full backfill emits current rows; a redrive also re-emits removals.**
  A backfill's job is "these are the rows", against a zone being rebuilt from
  nothing. A redrive exists because a slice of the stream was lost, and if
  that slice removed a holding, replaying only the survivors leaves the zone
  holding a card the tenant does not own.

Gates, all in `outbox.rs` and all seen red:

- **The headline proof** —
  `a_zone_rebuilt_by_backfill_equals_the_zone_incremental_shipping_built`:
  throw every event away (that is what "delete the zone" means — the zone
  holds exactly this projection), rebuild by backfill, assert the projection
  equals what the triggers produced one mutation at a time. The row-identical
  discipline of the lake-as-source design applied to the inbound leg, and the
  only test that shows the two paths *agree* rather than that both run. Seen
  red by making the backfill skip rows that are not `status = 'owned'` — a
  plausible optimisation that silently under-reports.
- **Rule 3 in the failing direction** —
  `a_stale_redrive_with_a_higher_seq_does_not_clobber_a_live_mutation`,
  constructed directly. Seen red by ordering `project` by `seq` alone.
- `an_emitted_event_carries_the_rows_own_time_not_the_moment_of_emission` —
  the property rule 3 rests on. Seen red by stamping `now`.
- `every_triggered_table_is_emittable` — reads the outbox triggers out of
  `sqlite_master` and asserts they fire on exactly `SOURCE_TABLES`. It earned
  its keep immediately: adding the sealed triggers turned it red, and it
  stayed red until the emitter covered them.
- `a_row_scope_redrives_only_its_own_table` — the pair rule on the emit side.
  Seen red by dropping the table guard from `scope_predicate`: a redrive of
  one single then emits an unrelated sealed lot sharing its number.
- `a_collection_from_before_the_sealed_triggers_gains_them_and_backfills` —
  **the upgrade path every existing box takes.** A collection older than the
  sealed triggers holds lots that generated no event and never would have;
  opening it must hang the triggers (re-applying `schema_user.sql` is what
  makes three `CREATE TRIGGER IF NOT EXISTS` statements a migration) and the
  backfill must then cover those lots. Nothing about a box in the failing
  state looks broken — its singles ship normally while its sealed holdings
  stay invisible — which is why this is a test and not a runbook step.
- The rule-4 refusal, the DR reconcile, idempotence under `--force`, the
  payload being byte-identical to the trigger's, and all three scope refusals
  (a backwards range, a range starting below 1, a table the outbox does not
  carry).

The fixture every emit proof is stated over — `a_collection_with_history` —
holds **both** sources, and the surviving sealed lot deliberately shares its
`row_id` with a surviving single. A fixture of singles alone would let a
backfill that skipped sealed pass the headline proof, which is the failure
this bead exists to prevent arriving through the test suite instead of
through the code.

**Still owed before this is armed on prod**: the proof is stated against
`outbox::project` rather than against a tenant zone, because the shipper
(pd-dxn3) is being built in parallel and does not exist yet. `project` is the
contract between them — the shipper writes that reduction — so re-stating the
headline proof against real Parquet in the zone is a container-tier gate that
belongs with the shipper (pd-880q, filed rather than forgotten).

**The sealed triggers have landed** (pd-4gop), so `SOURCE_TABLES` carries
`("sealed_collection", "added_at")` and every claim above is a claim about
both halves of the holdings. It cost exactly what was predicted: one entry in
that list, and nothing else in the emitter — `emit` already looped over
`SOURCE_TABLES`, the payload already came from `pragma_table_info`, and
`per_table` already reported per source. The one thing that was NOT free is
`Scope::Row`, which had to grow its table, because a scope naming a bare row
id stopped identifying a holding the moment there were two sources.

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
