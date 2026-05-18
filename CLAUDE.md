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

PokeDumpster is a single-user Pokémon TCG collection + sealed-collection
tracker — a Rust rebuild of DeckDumpster's collection feature set, against
pokemontcg.io + TCGCSV instead of Scryfall + MTGJSON. The headline feature is
**binder-page browsing**: sets render as virtual 3×3 binder pages and you
click slots to register the printings you own.

`PLAN.md` is the frozen v1 design record (see its banner); `RESEARCH.md` is
the research it rests on. Living truth = beads + the code + this file.

## Build & Test

```bash
# Backend — Rust workspace
cargo build                      # build all crates
cargo test                       # run all tests
cargo test -p pkdump-db          # test a single crate
cargo clippy --all-targets       # lint (must be clean before commit)
cargo fmt                        # format

# CLI / server
cargo run --bin pkdump -- <args> # run the pkdump CLI
pkdump setup                     # rebuild the shared catalog DB   (lands M1.9)
pkdump serve                     # start the HTTP server           (lands M1.10)

# Frontend — SvelteKit                                             (lands M2)
cd frontend && npm install && npm run build

# Intents UI tests — TypeScript + Playwright                       (lands M8)
cd tests/ui && npm test
```

## Architecture Overview

Cargo workspace, five crates (`crates/`):

- **pkdump-core** — domain types + pure logic (variant expansion, query
  compiler). No IO.
- **pkdump-db** — rusqlite persistence. Owns the shared/user DB split and
  refinery migrations (`migrations/{shared,user}/`).
- **pkdump-ingest** — upstream catalog ingestion (pokemontcg.io,
  pokemon-tcg-data, TCGCSV). Cache-population only — never called at request
  time.
- **pkdump-server** — Axum HTTP app; JSON API under `/api` + serves the
  SvelteKit static build.
- **pkdump-cli** — the `pkdump` binary; clap command tree.

Frontend: SvelteKit in `frontend/`, built static, served by Axum.

Two SQLite databases in `~/.pkdump/`:

- **shared.sqlite** — immutable card catalog, fully reproducible from
  upstream. Rebuilt by `pkdump setup`.
- **&lt;user&gt;.sqlite** — per-user mutable collection. The only thing worth
  backing up.

At runtime the user DB `ATTACH`es the shared DB read-only.

## Conventions & Patterns

- **Card data access** — all runtime card lookups read the local DB. The
  upstream APIs are touched only by `pkdump setup` / `pkdump data refresh`.
  See `architecture/CARD_DATA_ACCESS.md`.
- **No fallback logic.** Errors propagate. No silent defaults, no swallowed
  exceptions, as few error paths as possible — let it crash visibly.
- **Strict one row per physical card** in `collection`; no quantity
  aggregation.
- **Edition 2024**, toolchain pinned in `rust-toolchain.toml`. `cargo fmt`
  and `cargo clippy` clean before every commit.
- **Migrations** — refinery, two dirs under `crates/pkdump-db/migrations/`.
  refinery owns version history; there is no hand-rolled `schema_version`
  table.
- **Workspace dependencies** are declared in the root `Cargo.toml`
  `[workspace.dependencies]`; crates opt in with `dep.workspace = true`.
- **Tests that demonstrate bugs must fail** until the bug is fixed.
- **Dirty-data overrides** live in `data/overrides/*.json` as flat patch
  records, applied as the final phase of ingest; `data/known_issues.md`
  documents the upstream bugs they work around.
- **Decisions** — significant architectural decisions become
  `bd create --type=decision` issues. `PLAN.md` is frozen; do not edit it
  per-task.
- **Checkpoints** — commit after every closed beads task; reference the
  issue id (`Closes pokedumpster-xxx`) in the commit message.
