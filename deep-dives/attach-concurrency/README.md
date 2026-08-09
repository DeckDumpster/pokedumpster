# Measurement: N tenants reading the shared catalog through `ATTACH`

**Issue:** `pd-jgd4` — epic `pd-gckl` (per-tenant data model: file-per-tenant
+ Litestream multi-DB).
**Result:** see [`RESULT.md`](RESULT.md).

## The one question this answers

Epic `pd-gckl` keeps the card catalog as **one** SQLite file, `ATTACH`ed
read-only by every tenant's connection exactly as it is today. The alternative
— a copy of the catalog inside every tenant database — is explicitly rejected
by the epic as a data-model regression.

That rests on an assumption nobody had tested: that `ATTACH` stays cheap when
several tenants read the catalog at once. Today exactly one process reads it.
"Several friends browsing simultaneously" is more concurrent catalog access
than this app has ever served, and SQLite in WAL mode *should* handle it
comfortably — but "should" is not a measurement.

This is a **measurement**, not a gate. Contention found here does not stop the
epic; it gets recorded and filed. What would matter is discovering that
sharing one catalog file is materially worse than not sharing it, because the
whole design leans on it not being.

## How it is measured

The workload is the real read path — `pkdump_db::binder::get_binder_page`,
the heaviest catalog-joining query in the app — driven from a Rust harness at
[`crates/pkdump-db/examples/attach_concurrency.rs`](../../crates/pkdump-db/examples/attach_concurrency.rs).
It lives in the crate rather than here so that `cargo clippy --all-targets`
compiles it: a benchmark that transcribes the query into standalone SQL drifts
away from the code it is supposed to be defending, and then measures nothing.

The harness copies the server's concurrency structure rather than inventing
one. `pkdump_server::tenant::Tenants` holds **one connection per tenant, each
behind its own mutex, for the life of the process**, and `blocking()` hands
work to `spawn_blocking` — so the harness runs one long-lived connection and
one thread per tenant.

Four arms, run back to back at each reader count and repeated:

| Arm | What it is |
|---|---|
| `shared` | N tenants, ONE catalog file. What the epic proposes. |
| `private` | N tenants, each `ATTACH`ing its OWN copy of the catalog. **The control.** |
| `same_tenant` | N workers contending for ONE tenant's connection. |
| `refresh` | N readers plus a writer committing to the catalog, i.e. the nightly `pkdump data refresh` running while the server serves. |

The `private` arm is what makes the result mean anything. On a four-core box,
sixteen readers cannot run without latency rising, and that rise is CPU
saturation — nothing to do with SQLite. Sharing one catalog is free exactly to
the extent that `shared` matches `private` **at the same reader count**.
Without the control arm the numbers cannot tell those two causes apart, and
the honest answer would be "we don't know".

`same_tenant` exists for the opposite reason: it is the case that genuinely
*does* serialise, by construction, so it gives the claim "tenants do not
serialise against each other" a counter-example to stand against. A run where
nothing serialises anywhere is more likely to be a broken harness than a
lucky architecture.

Two more things keep the arms comparable:

- **Both arms start warm.** Every file a scenario touches is read into the
  page cache first. The control arm holds N copies of a ~110 MiB catalog and
  the shared arm holds one; without pre-warming, the comparison would mostly
  measure which arm's working set the kernel happened to be caching.
- **Arms are adjacent in time and repeated.** One binder page costs ~100 ms at
  this catalog scale, so a single pass yields only a hundred-odd samples. The
  sweep repeats and pools latencies, and the four arms at a given N run back
  to back, so a slow patch of the machine cannot land on one arm alone and
  read as a difference between them.

The catalog is **generated, not borrowed** — ~200 sets, ~43.6k cards, ~87k
printings, ~110 MiB, deterministic from a fixed seed. That is the order of the
real catalog once the Japanese half is counted, and generating it is what lets
this run on any box, in CI, with no prod data anywhere near it.

## Run

```bash
deep-dives/attach-concurrency/run.sh                       # ~8 min from cold
SECONDS_PER=20 REPS=5 LEVELS=1,2,4,8,16,32 ...run.sh       # longer / wider
WORK=/mnt/big/pd-attach KEEP=1 ...run.sh                   # keep the fixtures
```

The fixtures are large — a prod-scale catalog plus one private copy per reader
slot — so point `WORK` (or `TMPDIR`) at a filesystem with room. `run.sh`
refuses to start without it rather than dying halfway through.

Prod-safe and self-contained: everything happens inside `WORK`, against a
catalog the harness generates. It touches no `pkdump-*` unit, not
`pkdump-prod-data`, no live `collection.sqlite`, and no `$PKDUMP_HOME`.

## Headline findings

See [`RESULT.md`](RESULT.md) for the numbers. In short:

- **Sharing the catalog is free.** N tenants on one `ATTACH`ed file cost the
  same as N tenants on N private copies — every shared÷private ratio straddles
  1.00 with no trend in n — and no scenario ever produced a single
  `SQLITE_BUSY`. The epic's premise holds.
- **The real ceiling is CPU per binder page, not the catalog.** One page costs
  ~95 ms because `get_binder_page` scans *every* printing in the catalog and
  builds an automatic index on each call. Pre-existing and single-tenant;
  multi-tenancy multiplies it rather than causing it. Filed as `pd-qce0`.
- **A nightly refresh grows the catalog WAL without bound while readers are
  active** — 4 MiB with no readers, 914 MiB with one. A checkpoint cannot
  reset the WAL while anyone is reading. Filed as `pd-t50h`.
- **The catalog has a second read-write opener**: `pkdump serve` at startup,
  which can overlap the nightly refresh. The request path is provably
  read-only. Filed as `pd-dzu5`.
