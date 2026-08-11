# The raw landing zone

Every byte PokeDumpster fetches from an upstream can be landed in S3,
immutably, **before anything parses it**.

Today's refresh fetches, parses into `shared.sqlite`, and throws the bytes
away. When a parser turns out to be wrong the only recovery is to re-fetch
whatever upstream serves *today* — yesterday's catalog is gone. This project
shipped two parser defects in one day (`pd-0o5m`: `basep` mapped 0 of 53
printings; `pd-v0oi`: 26 more sets in the same class), so "the parser was
wrong" is the normal case, not the exceptional one. The landing zone is the
insurance policy: fix the parser, rebuild from history.

This is step 1 of the offline-lakehouse design
(`brain/wiki/projects/pokedumpster/designs/lakehouse-2026-08-10.md`). It
lands raw only. Building Iceberg tables from it is `pd-1ojt`; the per-tenant
transform tier is `pd-hkbc`.

---

## 1. The key layout

```
raw/source=<tcgcsv|pokemontcgio|pokemon-tcg-data>/
    dataset=<groups|products|prices|sets|cards|bulk>/
    ingest_date=YYYY-MM-DD/
    run=<ULID>/
        part-0000.json.zst
        part-0001.json.zst
        …
        _manifest.json
```

Three things about it are load-bearing.

**`run=<ULID>`, not a timestamp.** A ULID sorts chronologically *and*
disambiguates two runs on the same date, so a retry after a partial failure
lands *beside* the first attempt's objects and can never land *on* them. One
ULID covers a whole invocation, so every prefix a single refresh touches
carries the same `run=`.

**One `_manifest.json` per run and dataset**, sitting alongside the parts it
describes, recording per part: the upstream URL actually requested (query
string included), the HTTP status, the byte count *before* compression, and
the SHA-256 of those same uncompressed bytes. "Did we actually get
everything" is answerable without re-fetching a byte.

It also records whether the run finished:

| you see | it means |
| --- | --- |
| `"complete": true` | every fetch this prefix was going to get arrived |
| `"complete": false` + `error` + `failures[]` | the run stopped early, and where |
| no `_manifest.json` at all | the process died before it could even say that |

A manifest that looks whole while the prefix is short is the one thing this
file must never be. A failed fetch flushes its manifest immediately rather
than waiting for the run to tidy up, because a failing upstream is exactly
when the process is likeliest to be killed.

`complete` is deliberately **conservative across datasets**: if any part of
an invocation fails, every dataset that invocation touched is marked
incomplete, even one whose own fetches all succeeded. It has to be. A run
that died at group 200 of 450 leaves a `products` manifest with 200 parts,
no failure of its own, and no way to know it was owed 450 — the sink never
learns how many parts a dataset was supposed to get. Erring the other way
would let exactly that prefix read as whole. So `complete: true` means "this
dataset is all of that date's data"; `complete: false` means "do not assume
so, read `error` and `failures`".

**Nothing in `raw/` is parsed.** The bytes stored are the bytes received,
zstd-compressed and otherwise untouched. `_manifest.json` itself is
uncompressed — it is the file you open when a refresh looks wrong, and
needing a decompressor first is friction at the wrong moment.

### What is deliberately *not* landed

`images.pokemontcg.io` — set symbols and card art. The retention arithmetic
below is for JSON only; landing card art would change it completely. That is
its own decision, not something that creeps in under "everything we fetch".

`pkdump data normalize-symbols` and the symbols phase of a refresh therefore
fetch without landing, on purpose.

---

## 2. Retention: there is deliberately no lifecycle rule

**`raw/` is kept indefinitely, and the absence of a lifecycle rule on it is a
decision, not an oversight.**

It is measured rather than assumed. The first figures here were an estimate
over the price payloads alone; these are what one real refresh actually landed
(`pd-fet2`, 2026-08-11, English + Japanese):

| dataset | parts | uncompressed | in the bucket |
| --- | ---: | ---: | ---: |
| `tcgcsv/products` | 671 | 73.9 MB | **6.12 MB** |
| `tcgcsv/prices` | 671 | 11.2 MB | **1.26 MB** |
| `tcgcsv/groups` | 2 | 135 kB | 21 kB |
| `pokemontcgio/sets` | 1 | 58.8 kB | 6.6 kB |
| **one night** | **1,345** | **85.3 MB** | **7.40 MB** |

**~7.4 MB/night, ~2.7 GB/year, ~27 GB over ten years** — still roughly
$0.03/month in year one, so the decision is unchanged, but note two things the
estimate got wrong. `products` is landed every night and is **five times the
size of `prices`**, so "prices only; cards and sets are near-static" was the
wrong simplification: a group's product list is re-fetched whole each run
whether or not it moved. And the bill is not really storage — 1,349 objects a
night is ~492k PUTs/year, about $2.46, which is more than the bytes cost.

If you are here because you found an unmanaged prefix and were about to tidy
it up: **this is the one that is meant to be unmanaged.**

---

## 3. The bucket

**A separate bucket from the Litestream backup bucket.** Same AWS account,
same `AWS_PROFILE=pkdump` role path, so there is no new credential story —
the SDK assumes the role and refreshes it, and nothing here ever holds a
long-lived key.

The reason for the separation is not lifecycle policy but what the two
buckets hold. The Litestream bucket contains the **only irreplaceable data in
the system** — tenant databases and the registry. Everything in the lake is
reproducible by construction. Keeping them apart means a lifecycle rule
written for the lake can never reach the backups, which is exactly the class
of mistake that stays silent until a restore (`pd-1717`).

The bucket name is **host configuration** — a fact about this AWS account,
not a repo constant — and lives in `~/.config/pkdump/lake.env`, alongside
`alerts.env`, `litestream.env` and `store.env`:

```sh
PKDUMP_LAKE_S3_BUCKET=<bucket>
PKDUMP_LAKE_S3_REGION=us-west-2
#PKDUMP_LAKE_S3_PREFIX=          # optional key prefix; unset = raw/ at the root
#PKDUMP_LAKE_S3_ENDPOINT=        # optional; how a MinIO stands in for S3
```

`deploy/setup.sh` scaffolds the file commented out and never clobbers it. An
explicit environment variable beats the file, the `store.env` precedent.

**There is no default bucket and there will not be one.** With landing asked
for and nothing configured, the command refuses at startup and names the file
to write. It does not guess a bucket, and it does not quietly skip the
landing step and report success — the whole value of the landing zone is that
the bytes are there afterwards, so "misconfigured" and "landed nothing" must
not look alike.

---

## 4. Turning it on

Landing is **opt-in**, and off by default. With the flag absent, `lake.env`
is never read, no S3 client is built, and the fetch path behaves exactly as
it did before the landing zone existed — which is what keeps every offline
gate and container test offline.

```bash
# One run, ad hoc:
pkdump data refresh --land-raw
pkdump setup --land-raw

# Or by environment, which is how a fixed command line turns it on:
PKDUMP_LAND_RAW=1 pkdump data refresh
```

The destination is resolved **before the first fetch**, so a lake that was
asked for and is not configured stops the run at the start rather than after
an hour of requests whose bytes then have nowhere to go. A typo'd bucket or a
role that cannot write surfaces on the first PUT instead — which follows the
first response by milliseconds, and costs no bucket permission beyond the
`PutObject` the job actually needs.

### For the nightly refresh

`pkdump-refresh@<instance>.service` runs a fixed command line, so it opts in
by environment. It is **not** enabled by default — do this only once the
bucket exists and `lake.env` names it:

```bash
mkdir -p ~/.config/systemd/user/pkdump-refresh@<instance>.service.d
cat > ~/.config/systemd/user/pkdump-refresh@<instance>.service.d/lake.conf <<'EOF'
[Service]
Environment=PKDUMP_LAND_RAW=1
EOF
systemctl --user daemon-reload
```

The refresh runs inside the container, so the container also needs the AWS
config and the lake settings — the same mount pattern the Litestream sidecar
already uses for `~/.config/pkdump/<instance>/aws/`.

---

## 5. Reading a run back

Everything a reader needs is in the manifest; no listing is required.

```bash
# What did last night's TCGCSV price sweep land?
aws s3 ls --recursive "s3://${PKDUMP_LAKE_S3_BUCKET}/raw/source=tcgcsv/dataset=prices/ingest_date=$(date -u +%F)/"

# Did it finish?
aws s3 cp "s3://${PKDUMP_LAKE_S3_BUCKET}/raw/source=tcgcsv/dataset=prices/ingest_date=YYYY-MM-DD/run=<ULID>/_manifest.json" - \
  | jq '{complete, parts: (.parts | length), failures}'

# Verify a part against what the manifest claims.
aws s3 cp "s3://${PKDUMP_LAKE_S3_BUCKET}/<part key>" - | zstd -d | sha256sum
```

A run that reports `"complete": false` is not garbage — its parts are real
bytes and are as valid as any other. It simply is not *all* of that
dataset for that date, and the next run's prefix is where the rest is.

---

## 6. Building a table from it

`raw/` is only worth keeping if a table can be built from it and nothing else.
That is a claim, so it is a test — see §7 — and the first table to make it is
`catalog.prices` (`pd-1ojt`).

```bash
# One day, from raw, into the Iceberg catalog:
podman run --rm --network pkdump-lake-<inst> \
  -e PKDUMP_LAKE_NESSIE_URI=http://pkdump-nessie-<inst>:19120/iceberg/ \
  -e PKDUMP_LAKE_S3_BUCKET=<bucket> -e PKDUMP_LAKE_S3_REGION=us-west-2 \
  localhost/pkdump-lake:<inst> \
  pkdump-lake-build-prices --ingest-date 2026-08-11
```

`--ingest-date` is **required and never defaulted from the clock**: rebuilding
1 August is the same operation as building today, and a job that reads the
clock has two behaviours where it should have one. The build reads
`raw/source=tcgcsv/dataset=prices/ingest_date=<date>/`, writes one row per
price actually quoted at grain `(tcgplayer_product_id, sub_type_name,
price_type, observed_date)`, and replaces that `observed_date` partition in a
single commit — so re-running is a replacement, not a doubling, and no other
day is in the filter's reach.

Three things about it are decisions rather than details.

**A day can hold more than one run, and the newest *complete* one wins.**
`run=<ULID>` means a retry lands beside the first attempt, so "rebuild this
date" has more than one answer. `complete: true` means every fetch that prefix
was going to get arrived, which makes that run the whole day by definition.
With **no** complete run the build **refuses** and prints what it found:
nothing in the landing zone can say whether two partial runs together cover a
day — the writer never learns how many parts it was owed — so stitching them
silently would produce a table that looks like a day and is not one.
`--allow-incomplete` builds it anyway and records
`pkdump.raw-complete=false` in the snapshot, along with the run ids and part
count it used.

Expect that refusal to fire on a night when something *else* failed. `complete`
is conservative across datasets (§1): one invocation carries one `run=`, so a
pokemontcg.io tail that died marks the `prices` manifest incomplete even though
every price fetch succeeded. That is the flag working as designed — it says "do
not assume this is the whole day", not "these bytes are bad". Read the
manifest's `failures[]`, and if the failures are all in other datasets,
`--allow-incomplete` is the right answer and the snapshot will say that is what
you did.

**Every product's prices are in the table, sealed and single alike.** A TCGCSV
price payload does not say which kind of product it describes; that is a fact
about the catalog, and joining it in would make this table wrong whenever the
other dataset was. `shared.sqlite` splits the same bytes at import time into
`prices` (single cards, narrow) and `sealed_prices` (sealed, wide, and
`UNIQUE(product, observed_at)`), so a comparison has to account for the split.

**`price` is a double, not a decimal.** TCGCSV quotes JSON numbers and
`shared.sqlite` stores `REAL`; a double is the value that round-trips both. A
decimal would round, and "the sampled prices match" would stop meaning what it
says.

Checking the table against the catalog we already have:

```bash
pkdump-lake-verify-prices --sqlite /path/to/shared.sqlite --observed-date 2026-08-11
```

It asserts that, restricted to single cards, the two agree **exactly** — same
rows, same values — and that every sealed price SQLite kept is present in the
lake. The lake legitimately holds *more* sealed rows: `sealed_prices` has no
`sub_type_name` column, so a sealed product quoted under two sub-types loses
one of them in SQLite. The verifier prints that as a note rather than a
failure, because it is SQLite dropping data rather than the lake inventing it.

---

## 7. The transform tier: per-tenant value snapshots

`pd-ruwh`. The first job that *reads* the lake rather than filling it:
`pkdump-lake-value-snapshots` values every registered tenant's collection from
`catalog.prices` and writes `collection_value_snapshot` back into that
tenant's own database.

```bash
podman run --rm --network pkdump-lake-<inst> \
  -e PKDUMP_LAKE_NESSIE_URI=http://pkdump-nessie-<inst>:19120/iceberg/ \
  -e PKDUMP_LAKE_S3_BUCKET=<bucket> -e PKDUMP_LAKE_S3_REGION=us-west-2 \
  -v /path/to/pkdump/data:/data \
  localhost/pkdump-lake:<inst> \
  pkdump-lake-value-snapshots --date 2026-08-11 --data-dir /data
```

**What it replaces, and why.** `pkdump data refresh` used to end with a step 7
calling `value_history::snapshot_today` on the one collection `$PKDUMP_USER`
resolves to. There was no loop. Every *other* registered tenant got no value
history, ever, and the run reported success (`pd-s5yn`) — latent only because
prod has one tenant. So the unit of work here is the **registry**: active
users, in order, each valued against their own collection.

Looping inside the refresh was considered and rejected. The catalog refresh is
not the component that knows about tenants — it opens `shared.sqlite` and
should have no business opening anything else — and a refresh that half-writes
N collections fails worse than one that writes none. Step 7 is therefore
**deleted** rather than fixed in place (`pd-hkbc`), and
`tests/refresh/tenant_bytes.sh` holds it deleted: a real refresh over a data
directory with two provisioned tenants must leave every tenant database
byte-identical.

**Nothing snapshots today's value until this job runs.** That is the operational
consequence of the split, and it is why `pkdump-lake-value-snapshots` belongs on
a timer beside `pkdump-refresh@<instance>.timer` rather than being remembered by
hand. A day the job does not run is a gap in every tenant's chart — recoverable,
because `--date` reconstructs any day still in the lake, but not self-healing.

**Tenant data never enters the lake.** Prices come out of Iceberg; the
collection is read from, and the snapshot written to, the tenant's SQLite
file. Nothing keyed by a tenant is ever written to a lake table — `§9` of
`tests/lake/value_snapshots.sh` asserts the catalog still holds
`catalog.prices` and nothing else after a run.

**A failing tenant is skipped and the run continues.** A missing database, one
another process holds a write lock on, a schema too old to carry the
provenance table — each is logged, the loop moves on, and the process exits
**2**. Exit 0 means every tenant was snapshotted; exit 1 means the run never
started (no registry, no catalog, an empty lake). A run that half-completes
and reports success is exactly the failure mode of the missing loop this job
replaces.

**The date is required, and pinning is automatic.** `--date` is never
defaulted from the clock, for the reason `--ingest-date` is not: pointing the
job at an older date **is** the backfill. Prices are the newest quote per
(product, sub_type) at or *before* that date — the same "latest price we know"
rule `latest_prices` implements — so an older date reconstructs what the
collection was worth then, not now. The Nessie ref is resolved to a single
commit once per run and recorded in `collection_value_snapshot_run`
(`main@<hash>`), so every tenant in one run is valued from one catalog state
and the value can be traced back to it later.

**The app never blocks on any of this.** Nothing here is on the serving path;
`GET /api/collection/value-history` reads an empty table as an empty chart.
The job being absent, late or half-done is not an outage.

Useful flags: `--tenant <handle>` (repeatable) for a one-off repair,
`--dry-run` to compute and write nothing, `--ref main@<hash>` to pin the
catalog yourself.

---

## 8. Where the code is

| what | where |
| --- | --- |
| key layout, manifest, stores, config | `crates/pkdump-lake/` |
| the one place a response becomes bytes | `crates/pkdump-ingest/src/landing.rs` |
| flag → landing zone, manifest finalizing | `crates/pkdump-cli/src/landing.rs` |
| the acquisition phase it brackets | `acquire()` in `crates/pkdump-cli/src/{data,setup}.rs` |
| reading `raw/` back — runs, manifests, payloads | `lake/src/pkdump_lake/raw.py` |
| `catalog.prices`, and only that | `lake/src/pkdump_lake/prices.py` |
| the check against `shared.sqlite` | `lake/src/pkdump_lake/verify.py` |
| per-tenant value snapshots (the transform tier) | `lake/src/pkdump_lake/value_snapshots.py` |
| the aggregate it must reproduce | `crates/pkdump-db/src/value_history.rs` |
| the test-tier upstream override | `crates/pkdump-ingest/src/upstream.rs` |

The Rust half writes `raw/` and never reads it; the Python half reads it and
never writes. They share nothing but the key layout and the manifest shape,
which is why both spell those out rather than importing them from each other
— and why `tests/lake/prices.sh` builds its input with the *real* writer, so a
change on one side breaks something on the other loudly.

Tests:

```bash
cargo test -p pkdump-lake            # key layout, manifest, config refusal
cargo test -p pkdump-ingest --test raw_landing     # the real clients, end to end
cargo test -p pkdump-ingest --test prices_fixture  # a landing zone + the catalog
                                                   #   built from the same bytes
cargo test -p pkdump-ingest --test value_snapshot_fixture
                                     # a whole data directory — catalog, registry,
                                     #   two collections — and the snapshot rows
                                     #   Rust computes from it
bash tests/lake/prices.sh            # the build job, on a network with no upstream
bash tests/lake/value_snapshots.sh   # the transform, for every tenant
bash tests/refresh/tenant_bytes.sh   # and the refresh writing no tenant at all
```

The `raw_landing` gate drives the real `reqwest` clients against a local
server and asserts the properties that matter: two runs on one date are
disjoint and neither overwrites the other; every recorded SHA-256 matches the
object actually stored; a fetch that fails partway leaves a manifest that
says so; and a landed import writes the *same rows* as an un-landed one —
landing is a tee, not a transform.

`prices.sh` is the one that answers "is `raw/` sufficient, or decorative?".
Its podman network is **`--internal`**, and §2 proves that by trying to reach
the internet from the job image and requiring the attempt to fail — a build
that needed an upstream could not hide. What it builds is then compared row
for row against a `shared.sqlite` produced from the same upstream responses in
the same pass, so "the lake and SQLite disagree" and "the upstream answered
differently" can never be confused for each other.

`value_snapshots.sh` is the one that answers "does *every* tenant get a
snapshot, and are the numbers still the old ones?". Its fixture is a whole data
directory built by the real code — the catalog imported from landed bytes, the
registry and tenant files provisioned by the real `pkdump tenant create` — and
the expectation it diffs against is what Rust's `snapshot_today` computed over
that same fixture. So the transform is held to being *observably a no-op*
before anything else is asked of it, while a second tenant who has never had a
snapshot row must come out with his own, and a third whose database is missing
must be skipped without taking the run down.

`tenant_bytes.sh` is the mirror of that: it answers "and does the refresh still
touch anybody?". A real `pkdump data refresh` runs through the shipped image
over a data directory holding two tenants provisioned by the real `pkdump tenant
create` — one of them `collection`, the handle `$PKDUMP_USER` defaults to and
therefore the exact database the deleted step 7 wrote — and every tenant file,
WAL and shared-memory sidecar included, has to come out byte-identical. Its
upstream is `tests/refresh/upstream.py`, which publishes no sets and no groups,
so the refresh completes in seconds without depending on anyone's uptime; §5
asserts the derivation phases really ran, because a refresh that no-ops
everything would also leave the tenants alone.

Both Rust tiers are hermetic. `PKDUMP_LAKE_DIR` selects a directory-backed
store instead of a bucket, which is what lets the landing zone's behaviour be
asserted without credentials or a network; the Python reader honours the same
variable for the same reason.

---

## 8. The first real run, and what it cost

Everything above shipped against fixtures, a directory-backed store, and a
MinIO. `pd-fet2` ran it against the real bucket for the first time on
2026-08-11. It worked — but three things are worth knowing before you run it
yourself, and none of them were visible from a hermetic gate.

### `lake.env` must spell the keys `PKDUMP_LAKE_S3_*`

The file on the box had been written from the design note as
`PKDUMP_LAKE_BUCKET` / `PKDUMP_LAKE_REGION` / `PKDUMP_LAKE_RAW_PREFIX`. Nothing
reads those names. `--land-raw` refused at startup — correctly, and naming the
file — but "the lake is configured" and "the lake is configured with the names
the code reads" are not the same statement, and only §3's spelling is the
second one. `grep PKDUMP_LAKE_S3_BUCKET ~/.config/pkdump/lake.env` before a
first run.

`PKDUMP_LAKE_S3_PREFIX` is a prefix *in front of* `raw/`. Setting it to `raw`
lands everything at `raw/raw/…`. Leave it unset unless you mean it.

### A flaky pokemontcg.io takes the whole night with it

`acquire()` fetches the pokemontcg.io tail **first**, and no client here
retries — errors propagate, by design. api.pokemontcg.io was answering 500/502
to roughly 45% of requests that day, so most attempts died in their first
second with nothing landed but a 728-byte manifest saying so. TCGCSV was never
reached, so no prices landed at all; it took seven attempts to get one clean
run.

The manifest behaved exactly as designed. The ordering is the problem: prices
are the dataset that cannot be re-fetched later, and they are behind the
upstream most likely to be down. Tracked as `pd-nons`.

### What one night is

1,349 objects, 7.40 MB in the bucket, 85.3 MB uncompressed — the table in §2.
The acquisition phase took **3m38s** (1,345 fetches, each landed before it was
parsed); the whole `pkdump data refresh --land-raw`, derivation included, took
**5m5s** on the deployment box. Landing costs about a PUT per fetch and does
not measurably lengthen the run.

Rebuilding `catalog.prices` for that day from those objects took **29s** and
produced 305,648 rows, which reconcile exactly against the `shared.sqlite`
written from the same responses: 289,327 single-card rows identical value for
value, and every one of the 15,993 sealed prices SQLite kept present in the
lake (the lake holds 328 more, being the sub-types `sealed_prices` cannot
represent — §6). `shared.latest_prices` was 296,697 rows, of which exactly
289,327 carry that day's `observed_at`; the remaining 7,370 are the most
recent quote for products TCGCSV no longer lists.
