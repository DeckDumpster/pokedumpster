# sqld / libSQL storage investigation — decision brief

> ## ⚠️ This path was NOT taken
>
> The libSQL/`sqld` Path A recommended below was **rejected**. The direction
> actually chosen is **file-per-tenant local SQLite + Litestream multi-DB
> replication** — epic **pd-gckl** — which keeps today's rusqlite stack and its
> connection-scoped `ATTACH` unchanged. Kept as the record of why libSQL was
> rejected, not as a plan.

One-screen distillation of five spikes. Full detail in `RESULT.md`; decision of
record is beads **pokedumpster-181** — itself superseded by **pd-gckl**. Branch
`spike/sqld-attach-namespaces`.

## Question

Can we re-architect PokeDumpster's storage onto an object-storage-backed,
per-tenant-namespaced substrate (for multitenant hosting + easy backup) while
keeping a SQLite-compatible query engine? Driven by wanting friends to use the
app over the internet.

## Options considered

| Option | Verdict |
|---|---|
| **SlateDB directly** | ❌ It's a KV/LSM primitive, not a query engine. Every adopter layers their own engine on top. Forfeits SQL/joins — the heart of the app. |
| **ZeroFS** (FS over SlateDB) | Transparent (keeps ATTACH; it's just files), but durability without turnkey PITR, deep correctness-sensitive stack, 1-day-old AGPL. |
| **libSQL embedded replicas** | ❌ µs local reads but ATTACH unsupported → would force denormalizing the catalog. |
| **Turso Database** (Rust rewrite) | Vendor's future, but beta — too sharp for the only-thing-worth-backing-up data. |
| **libSQL/sqld "Path A"** (server + namespaces + bottomless) | ✅ **Recommended target** if/when multitenancy is greenlit. Validated below. |

## What the 5 spikes proved (all ✅, with caveats)

1. **ATTACH across namespaces** works — needs `allow_attach=true` on the catalog
   namespace; `ATTACH "catalog" AS cat` (double-quoted); inside a txn; read-only.
2. **TEMP views don't port** (unsupported + permanent views can't reference an
   attached db) → reference catalog tables **`cat.`-qualified**, drop the view layer.
3. **Rust `libsql` remote client** doesn't pin a stream per Connection → ATTACH
   must live **inside the query's `transaction()`** (attach-at-open doesn't persist).
   No libsql mode gives both persistent-attach AND ATTACH. **This overturns
   spike 2's "attach once at connection open" recommendation**, which held only
   at the raw-Hrana layer; `RESULT.md` marks the contradiction in place.
4. **bottomless backup/restore** works (auto-restores an empty DB on startup).
   Replication is **batched** (durability window; flushed on graceful shutdown).
5. **Per-namespace JWT auth** (per-namespace `jwt_key`) is enforced and **scoped**
   (tokenA denied on tenantB); a namespace is **open until its key is set**.
   **Per-namespace backup** works but needs `LIBSQL_BOTTOMLESS_DATABASE_ID`, and
   **the namespace registry is NOT backed up** — DR must re-declare namespaces,
   then data restores on open.

## Migration cost (honest)

`crates/pkdump-db` needs a **with-catalog-attached transaction wrapper** + `cat.`
qualification across the catalog-joining read path; plus per-tenant namespace +
`jwt_key` provisioning; plus a DR runbook that re-declares namespaces. Bounded
but non-trivial — touches the read path broadly.

## Recommendation

- **Now:** keep status quo (local SQLite + ATTACH + `backup.sh`).
- **Backup first (current plan):** validate bottomless against a *real* S3 bucket
  in **single-DB mode** — that directly hardens backup/restore for today's
  single-user app, no multitenancy needed yet.
- **Then multitenancy:** libSQL/sqld Path A (NOT SlateDB-direct, NOT embedded
  replicas). — **Superseded.** Path A was rejected; multitenancy went to
  file-per-tenant SQLite + Litestream multi-DB (epic **pd-gckl**), whose whole
  argument is that the migration cost priced above buys nothing local SQLite
  doesn't already have.
- **Always:** keep nightly `sqlite .backup` as belt-and-suspenders.
- **Standing risk:** Turso is de-prioritizing libSQL (closed-source rewrite);
  bottomless is the legacy replication path.

## Spike scripts (all self-cleaning; `KEEP=1` to inspect)

| Script | Proves |
|---|---|
| `run.sh` | ATTACH across namespaces |
| `run-temp-view.sh` | TEMP-VIEW vs `cat.`-qualify |
| `run-rust-client.sh` | Rust client per-transaction ATTACH |
| `run-bottomless.sh` | backup/restore vs local MinIO |
| `run-jwt-backup.sh` | per-namespace auth + per-namespace backup |
| `run-bottomless-s3.sh` | backup/restore vs a **real** S3 bucket (creds via env file) |
