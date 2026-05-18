# Card Data Access Policy

## Rule: the local database is the single source of truth

All card, set, printing, and price data at runtime MUST come from the local
SQLite catalog (`shared.sqlite`). The upstream APIs are accessed only during
explicit cache-population commands:

- `pkdump setup` — full rebuild of the shared catalog
- `pkdump data refresh` — incremental nightly refresh (new sets + prices)

Every other code path — the HTTP API, the binder-browse endpoint, collection
queries, CSV-import resolution, exports — reads exclusively from the local DB
via the `pkdump-db` repository layer.

## Why

A single data source (the bulk cache) guarantees consistent IDs throughout
the system and eliminates runtime network dependencies. The server runs with
no network access required and no upstream rate-limit exposure.

## How to add a feature that needs card data

1. Assume `shared.sqlite` is populated (the user has run `pkdump setup`).
2. Query through `pkdump-db` repositories — never call `pkdump-ingest`'s HTTP
   clients at request time.
3. If the data isn't cached, return an error telling the user to run
   `pkdump data refresh`. Do NOT fall back to the live API.
4. If you need data not in the current schema, extend the ingest pipeline and
   add a migration — not a runtime fetch.
