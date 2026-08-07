# VERDICT: **PASS**

One Litestream sidecar replicates N tenant SQLite databases, and any one of
them restores to a chosen point in time without disturbing the others.

- **Litestream version:** `v0.5.11` (`docker.io/litestream/litestream:latest`,
  image `345504712b64`, built 2026-04-08). This is the tag
  `deploy/pkdump-litestream.container` pins — it pins `:latest`, not a version,
  so **re-run this spike whenever that image moves.**
- **Storage:** MinIO (S3-compatible), path-style, in-cluster endpoint. A real
  bucket is `pd-fof4`'s problem, not this bead's.
- **Evidence:** `run.sh`, 26/26 checks, exit 0, from a cold start.

The epic's premise holds. `pd-gckl` is not blocked on backup capability.

## What was actually proven

| # | Claim | Result |
|---|---|---|
| 1 | N databases replicate from ONE `litestream replicate` process | 3 tenants, one process, one config, one bucket |
| 2 | Writes are genuinely concurrent, not serialised | 50 rounds × 3 tenants, all three writers `&`-forked per round and `wait`ed |
| 3 | Independent point-in-time restore | tenant #2 rolled back to a marker; tenants #1 and #3 stayed current, **byte-identical** (sha256 before/after) |
| 4 | The **second** tenant, not the first | `TENANTS=(alpha bravo charlie)`; the victim is `bravo`, a genuine middle element |

Restore was exercised three ways: out-of-place PIT (`-timestamp … -o`),
latest-for-every-tenant, and a real in-place DR drill (delete the live file,
restore over it). After the rollback, an untouched tenant still restored to
current — one tenant's recovery does not consume or invalidate another's
replica.

Per-tenant streams are fully separate in the bucket: distinct prefixes,
distinct LTX levels, distinct TXID sequences (`txid.db` advances per database).

## Constraints discovered — read these before building on this

### 1. Schedules are process-global. Per-tenant retention is not possible.

The `snapshot:` block configures the level-9 compaction monitor, and there is
**one monitor per process** (`system=store`), shared by every database:

```
msg="starting L0 retention monitor" system=store interval=15s retention=5m0s
msg="starting compaction monitor"   system=store level=9 interval=24h0m0s
```

A top-level `snapshot.interval` **is** honoured. The same block placed **inside
a `dbs:` entry is silently ignored** — the monitor falls back to the 24h
default. So every tenant shares one snapshot cadence and one retention window;
you cannot give tenant X 6 months and tenant Y 30 days from one sidecar.

That is fine for this epic (prod's `deploy/litestream.yml` sets one 24h/4320h
policy for everyone), but it is a hard ceiling, so record it rather than
discover it later.

**Litestream does not reject unknown config keys.** A misspelled key is dropped
without a warning. Any config generator needs its own validation; the YAML
parser will not catch mistakes for you.

### 2. Colliding replica paths cause a SILENT CROSS-TENANT DATA SUBSTITUTION.

Point two databases at the same replica path and Litestream accepts it without
a warning, an error, or a lock. Both stream LTX files into the same prefix.
Restoring **charlie** from that prefix returned **alpha's data** — 50 rows, all
`tenant=alpha` — and `PRAGMA integrity_check` said `ok`.

This is the worst failure mode available to a data-isolation feature: one
tenant's restore silently hands back another tenant's collection, and nothing
anywhere reports a problem. `pd-fof4` must treat "every generated replica path
is distinct" as an assertion with a test behind it, not a code-review note.

Section 15 removes the footgun entirely — see below.

### 3. Blast radius of a missing tenant DB is zero.

A `dbs:` entry whose file does not exist does **not** abort startup and does not
stop the other tenants replicating — no `ERROR` lines at all. Better still, the
database is picked up automatically once the file appears, with no restart. So
config may legitimately run ahead of provisioning.

### 4. `dir:` + `pattern:` + `watch:` — N tenants with NO config generation.

Undocumented in the config we ship, but present and working in 0.5.11:

```yaml
dbs:
  - dir: /data/tenants
    pattern: "*.sqlite"
    watch: true
    replicas:
      - type: s3
        bucket: …
        path: tenants          # per-db path is DERIVED, not written
```

```
msg="found databases in directory" dir=/data/tenants count=3 watch=true
msg="replicating to" … path=tenants/delta.sqlite
msg="added database to replication" dir=/data/tenants path=/data/tenants/echo.sqlite
```

Two properties that matter to this epic:

- **The replica path is derived from the filename.** Distinct by construction —
  constraint 2 becomes unreachable.
- **`watch: true` picks up a database created after the sidecar started.**
  Provisioning a tenant is `create the file`; no config edit, no restart, no
  reload. `echo.sqlite` was created 5s into the run and began replicating
  within ~5s.

This is worth `pd-fof4` reconsidering its own premise: *generating* config for N
tenants may not be work that needs doing.

**The trade-off is restore addressing.** In `dir` mode the config no longer
names database paths, so the `-config` form fails:

```
$ litestream restore -config … -o out.sqlite /data/tenants/echo.sqlite
ERROR … error="database not found in config: /data/tenants/echo.sqlite"
```

Restore must go through the replica URL instead, which works:

```bash
LITESTREAM_ACCESS_KEY_ID=… LITESTREAM_SECRET_ACCESS_KEY=… \
litestream restore -o /restore/echo.sqlite \
  's3://BUCKET/tenants/echo.sqlite?endpoint=…&region=…&force-path-style=true'
```

`pd-v8zf`'s runbook must pick one mode and document its restore form. They are
not interchangeable.

### 5. Litestream 0.5 writes its own tables into every tenant DB.

Each replicated database gains `_litestream_lock` and `_litestream_seq`. This
is pre-existing behaviour on prod today, not new — but it interacts with a
per-tenant model:

`crates/pkdump-db/src/json_backup.rs::user_tables` enumerates tables from
`sqlite_master`, excluding only `sqlite_%` and `refinery_schema_history`. The
two `_litestream_*` tables therefore land inside the `pkdump export --json`
envelope and are replayed by `pkdump import --json` into a fresh database —
carrying one database's replication bookkeeping into another. Filed as its own
bead; not fixed here (this bead does not touch application code).

## Reproducing

```bash
deep-dives/litestream-multi-db/run.sh          # ~2 min, exit 0 = PASS
KEEP=1 deep-dives/litestream-multi-db/run.sh   # leave MinIO + WORK up to poke at
```

Self-contained: its own podman network, its own MinIO, its own temp dir, its
own throwaway SQLite files. Touches no `pkdump-*` unit, no `pkdump-prod-data`,
no application code.

The constraint probes (§12–16) are assertions, not prose — if a future
Litestream changes any of them, the script fails and says which.
