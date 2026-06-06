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

---

# Follow-up: TEMP-VIEW spike (`run-temp-view.sh` / `temp_view_spike.py`)

Resolves follow-up #1: does our "ATTACH + TEMP VIEWs once per connection, then
query unqualified" pattern (`crates/pkdump-db`) port to sqld?

## Findings (clean, isolated connections)

| Behavior | Result |
|---|---|
| ATTACH is **connection-scoped** (sticks after COMMIT; attach once per conn) | ✅ YES |
| `CREATE TEMP VIEW` supported | ❌ NO — *"unsupported statement"* (TEMP objects can't be replicated) |
| `CREATE TEMPORARY VIEW` supported | ❌ NO — same |
| `CREATE VIEW` (permanent) referencing `cat.*` | ❌ NO — SQLite: *"view cannot reference objects in database cat"* |
| Qualified `cat.cards` reference (no view layer) | ✅ YES |

## What this means

**The TEMP-VIEW indirection does NOT port** — sqld rejects TEMP/TEMPORARY views
(they're non-replicable), and core SQLite forbids a *permanent* view from
referencing an attached database. So there is no view layer that lets catalog
tables be referenced unqualified.

**But the foundation is solid:** ATTACH is *connection-scoped*. You attach the
catalog **once** at connection open (`BEGIN; ATTACH "catalog" AS cat; COMMIT`),
and every subsequent query on that connection sees `cat.*` — even outside a
transaction.

## Recommended pattern for a libSQL/sqld port

1. **At connection open:** issue `BEGIN; ATTACH "catalog" AS cat; COMMIT` once
   (replaces today's "create TEMP VIEWs at open" step).
2. **In queries:** reference catalog tables **qualified** as `cat.<table>`
   (replaces the unqualified TEMP-VIEW names).

## Migration-cost note (revises the earlier estimate)

This is a **modest, real refactor** of `crates/pkdump-db`, not a verbatim port:
the TEMP-VIEW setup becomes ATTACH-at-open, and catalog-table references in the
binder-page query and friends must be `cat.`-qualified. Bounded and mechanical,
but it touches every query that joins the catalog — factor it into the libSQL
decision.

## Reproduce

```bash
spikes/sqld-attach-namespaces/run-temp-view.sh          # findings + guidance
KEEP=1 spikes/sqld-attach-namespaces/run-temp-view.sh   # leave container up
```

---

# Follow-up #2: Rust `libsql` client (`run-rust-client.sh` / `rust-client/`)

Validates the actual client path (`libsql` v0.9.30, `remote`+`tls` features)
that `crates/pkdump-db` would use.

## Findings

| Mode | Result |
|---|---|
| A — ATTACH at connection open, query in a separate later call | ❌ `no such table: cat.cards` |
| B — ATTACH + join inside ONE `conn.transaction()` | ✅ 3 rows |

The libsql **remote `Connection` does not pin a single Hrana stream across
top-level calls** — so the connection-scoped ATTACH measured at the raw-Hrana
layer (held baton, S0 above) does NOT survive the client abstraction. ATTACH
only holds within an explicit `transaction()`.

## Pattern (supersedes "attach once at open")

```rust
let tx = conn.transaction().await?;
tx.execute(r#"ATTACH "catalog" AS cat"#, ()).await?;     // read-only catalog
let rows = tx.query("... JOIN cat.cards ...", ()).await?; // cat.-qualified
tx.commit().await?;
```

## Migration cost — the three escalations, stated honestly

1. ATTACH works (needs `allow_attach` on the catalog ns).
2. TEMP views don't port → `cat.`-qualify catalog references.
3. **attach-at-open doesn't persist via the libsql remote client → every
   catalog-querying path becomes attach-inside-a-transaction.**

Net: the `pkdump-db` connection/query layer needs real restructuring — a
"with catalog attached" transaction wrapper around catalog reads, plus `cat.`
qualification throughout. Bounded and mechanical, but it touches the read path
broadly; one extra ATTACH per transaction (pipelined in-batch; modest).

There is **no libsql mode that gives both persistent attach AND ATTACH**:
embedded-replica mode pins a local connection but forbids ATTACH entirely.
Per-transaction attach (remote client) is the path.

## Gotchas captured

- `libsql` needs the **`tls`** feature even for plain `http://` (else it panics
  "you must provide your own http connector").
- Client routes to a namespace by URL host, so `tenant1.localhost` must resolve;
  it maps to `::1` here, so sqld must be published dual-stack
  (`-p 127.0.0.1:18080:8080 -p '[::1]:18080:8080'`).

## Reproduce

```bash
spikes/sqld-attach-namespaces/run-rust-client.sh          # build + run + teardown
KEEP=1 spikes/sqld-attach-namespaces/run-rust-client.sh   # leave container up
```
First run needs network (`cargo build` pulls libsql + deps).
