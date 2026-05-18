# PokeDumpster

A single-user Pokémon TCG collection + sealed-collection tracker. Browse sets
as virtual 3×3 binder pages, click slots to register the printings you own,
and track condition, price, decks, binders, and orders.

Rust + Axum + SvelteKit, on a local SQLite catalog built from pokemontcg.io
and TCGCSV. A Pokémon-flavored rebuild of [DeckDumpster](https://github.com/DeckDumpster/deckdumpster).

See `PLAN.md` for the design (frozen v1 record) and `RESEARCH.md` for the
research behind it. Work is tracked in beads — run `bd ready`.

## Status

Early development — **M1: repo bootstrap + shared catalog**. Not yet usable.

## Development

```bash
cargo build && cargo test      # backend
cargo clippy --all-targets     # lint
```

See `CLAUDE.md` for the full command reference, architecture, and conventions.

## Remote access

Local-only; no application-level authentication. Reach it from other devices
over WireGuard.

## License

MIT
