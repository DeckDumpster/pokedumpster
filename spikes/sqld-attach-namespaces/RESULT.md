# Result — ATTACH-across-namespaces in self-hosted `sqld`

**Issue:** pokedumpster-5jv · **Date:** 2026-06-06 · **Image:** `ghcr.io/tursodatabase/libsql-server:latest`

## Verdict: ✅ PASS (conditional on one config flag)

Self-hosted `sqld` **does** support `ATTACH` across namespaces in server mode. A
per-tenant namespace can attach a separate **read-only** catalog namespace and
`JOIN` across them in a single SQL statement — i.e. **PokeDumpster's
shared-catalog architecture survives a libSQL/`sqld` move** ("Path A").

Proof (from `run.sh`, clean container):

```
ATTACH "catalog" AS cat
SELECT col.id, c.name, col.condition
  FROM collection col JOIN cat.cards c ON c.id = col.card_id
=> 10 | Charizard | NM
   11 | Mew       | LP
   12 | Charizard | MP   (3 rows, catalog names resolved across namespaces)
```

## The catch: `allow_attach` must be enabled on the *attached* namespace

A fresh namespace defaults to `allow_attach=false`. Attaching it returns:

```
403 Forbidden: Namespace `catalog` doesn't allow attach
```

Fix: set `allow_attach=true` in the **target** namespace's config (the catalog,
not the tenant). The config endpoint requires the **full** config object, so
GET → patch → POST:

```bash
curl -s "$ADMIN/v1/namespaces/catalog/config" \
  | python3 -c 'import json,sys;c=json.load(sys.stdin);c["allow_attach"]=True;print(json.dumps(c))' \
  | curl -s -X POST "$ADMIN/v1/namespaces/catalog/config" -H 'content-type: application/json' --data @-
```

This is actually a clean fit for our model: only the shared catalog needs
`allow_attach`; tenant DBs never need to be attachable.

## Things learned (save the next person the thrash)

- **Syntax matters.** `ATTACH "catalog" AS cat` (double-quoted *identifier* =
  namespace) is correct. `ATTACH 'catalog'` (single-quoted *string*) is parsed
  as a legacy file-path attach and rejected: *"unsupported statement"*.
- **ATTACH must be inside a transaction** (`BEGIN … ATTACH … SELECT … COMMIT`).
- **Attached namespace is read-only** — exactly our catalog's role. ✓
- **Namespace routing is by `Host` header** (first label → namespace name).
- **Image gotcha:** `docker-entrypoint.sh` auto-appends `--db-path`,
  `--http-listen-addr` (from `$SQLD_HTTP_LISTEN_ADDR`), `--grpc-listen-addr`.
  Pass only the flags it does *not* set (`--admin-listen-addr`,
  `--enable-namespaces`) or `sqld` aborts on duplicate args.
- Config defaults seen: `durability_mode=relaxed`, `max_db_size="1000.0 PB"`,
  `block_reads/writes=false`, `jwt_key=null`.

## Maps to PokeDumpster

- `catalog` namespace = shared `shared.sqlite` role (written by the single
  `pkdump setup`/refresh writer), created with `allow_attach=true`.
- `<user>` namespace per friend = the `collection.sqlite` role.
- Per-request: `BEGIN; ATTACH "catalog" AS cat; <join queries>; COMMIT`.

## Open questions this spike did NOT cover (next spikes if we proceed)

1. **TEMP VIEW pattern.** We currently expose catalog tables via per-connection
   TEMP VIEWs (`crates/pkdump-db`). In Hrana, a "connection" is per stream/baton,
   so TEMP VIEWs would need recreating per stream — verify the binder-page query
   still composes, or qualify with the `cat.` alias instead of views.
2. **Rust `libsql` client** end-to-end (we proved it via Hrana HTTP; the Rust
   client hits the same server, but confirm `libsql`/`libsql-rusqlite` ergonomics
   for the BEGIN/ATTACH/COMMIT envelope).
3. **bottomless** S3 replication + restore for backup simplification.
4. **JWT auth** + wildcard DNS for namespace-per-tenant routing.

## Reproduce

```bash
spikes/sqld-attach-namespaces/run.sh          # full run + teardown (prints VERDICT)
KEEP=1 spikes/sqld-attach-namespaces/run.sh   # leave container up on :18080 / admin :19090
```
