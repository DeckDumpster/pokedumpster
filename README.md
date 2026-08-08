# PokeDumpster

A single-user Pokémon TCG collection tracker. Browse sets as virtual binder
pages, click slots to register the printings you own, and keep track of
condition, price, decks, binders, sealed product, wishlist, and order
history alongside.

Rust + Axum + SvelteKit on a local SQLite catalog built from pokemontcg.io
and TCGCSV. A Pokémon-flavored rebuild of
[DeckDumpster](https://github.com/DeckDumpster/deckdumpster).

## What's in the box

- **Binder-page browsing** (`/browse/<set>`) — every set rendered as a grid;
  click a slot to register a printing, with one colored pip per variant.
- **Variant fidelity** — every printing the catalog knows about (normal,
  holo, reverse holo, pattern overlays like Poké Ball / Master Ball /
  Energy Symbol, stamped promos, Cosmos Holo cross-group reprints, …) is a
  distinct printing row tied to the upstream TCGplayer product, so prices
  query directly.
- **Strict one-row-per-physical-card** collection model — no quantity
  aggregation; each copy carries its own condition, batch, binder/deck
  assignment, status history, and provenance.
- **Set analytics** — completion bars (base vs. master set), rarity
  breakdowns, value totals.
- **Adjacent collections** — binders, decks, sealed product, wishlist,
  orders. Mutual-exclusion invariants (a copy lives in a binder OR a deck,
  not both).
- **Bulk import** — CSV ingest (ManaBox-shaped today; pokemontcg.io and
  TCGplayer formats also recognised).

## Status

In active use. Backend, frontend, ingest pipeline, and rootless-Podman
deployment all live. Running in production on a self-hosted instance via
the `deploy/` scripts.

Work is tracked in **beads** — run `bd ready` to see what's next; `bd list`
for the full backlog.

## Quick start (dev)

```bash
# Backend — Rust workspace
cargo build && cargo test     # also regenerates ts-rs TypeScript bindings
cargo clippy --all-targets    # lint (must be clean before commit)
cargo fmt                     # format

# Catalog (downloads pokemontcg.io + TCGCSV, runs variant expansion)
cargo run --bin pkdump -- setup

# Frontend
cd frontend && npm install && npm run build

# Serve (defaults to 127.0.0.1:8080; opens collection DB from ~/.pkdump/)
cargo run --bin pkdump -- serve
```

Set `$PKDUMP_HOME` to relocate the data dir (defaults to `~/.pkdump/`).
Set `$PKDUMP_USER` to switch tenant (defaults to `collection`). A user is a
`handle` joined to an opaque ULID `database_id` in `$PKDUMP_HOME/registry.sqlite`;
their collection lives at `$PKDUMP_HOME/tenants/<database_id>.sqlite`, sharing the
one `shared.sqlite` catalog. `pkdump tenant create|list|rename|detach` is the
operator surface — note that `tenant remove` now *detaches* (frees the handle,
keeps the data); hard deletion is `pkdump tenant purge <database-id> --yes`.
`deploy/TENANTS.md` is the runbook.

## Deployment

A rootless-Podman + systemd-user deployment lives under `deploy/`:

```bash
deploy/setup.sh prod          # first-time bring-up (image + Quadlet unit + timers)
deploy/seed.sh prod           # populate the catalog (pkdump setup in a one-off container)
deploy/deploy.sh prod         # rebuild image + restart
deploy/restore-litestream.sh prod   # restore the collection from S3 (see deploy/RESTORE.md)
```

Off-box backup is the `pkdump-litestream-prod` sidecar (continuous S3
replication, 6-month point-in-time recovery). See `deploy/README.md` for the
full flow and `deploy/RESTORE.md` for the disaster-recovery runbook. From other devices, reach the
instance over WireGuard — no application-level authentication.

## Project layout

```
crates/
  pkdump-core/      domain types + pure logic (variant parsing, query model)
  pkdump-db/        rusqlite persistence + refinery migrations
  pkdump-ingest/    upstream catalog ingestion (pokemontcg.io, TCGCSV)
  pkdump-server/    Axum HTTP app + JSON API + SvelteKit static serve
  pkdump-cli/       the `pkdump` binary (setup / data / serve / seed-fixture)
frontend/           SvelteKit SPA (built static, served by pkdump-server)
deploy/             rootless-Podman + systemd-user deployment scripts
data/
  variants.json     canonical variant display metadata (label/short/rank/color)
  overrides/        hand-curated patches over upstream ingest
  known_issues.md   documents the upstream bugs the overrides work around
architecture/       focused design notes (see CARD_DATA_ACCESS.md)
tests/ui/           intent files for an in-progress Playwright harness
PLAN.md             frozen v1 design record
RESEARCH.md         research that informed the design
CLAUDE.md          / AGENTS.md   instructions for AI coding agents
```

See `CLAUDE.md` for the full command reference, architecture, and conventions.

## License

MIT
