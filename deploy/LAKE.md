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

It is measured rather than assumed: 295,951 price records/day × ~110 B =
~33 MB/day raw, **~4.1 MB/day compressed, ~1.5 GB/year, ~15 GB over ten
years** — roughly $0.03/month in year one. Cheaper than revisiting the
decision, and far cheaper than losing the ability to rebuild a date. (Prices
only; cards and sets are near-static.)

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

## 7. Where the code is

| what | where |
| --- | --- |
| key layout, manifest, stores, config | `crates/pkdump-lake/` |
| the one place a response becomes bytes | `crates/pkdump-ingest/src/landing.rs` |
| flag → landing zone, manifest finalizing | `crates/pkdump-cli/src/landing.rs` |
| the acquisition phase it brackets | `acquire()` in `crates/pkdump-cli/src/{data,setup}.rs` |
| reading `raw/` back — runs, manifests, payloads | `lake/src/pkdump_lake/raw.py` |
| `catalog.prices`, and only that | `lake/src/pkdump_lake/prices.py` |
| the check against `shared.sqlite` | `lake/src/pkdump_lake/verify.py` |

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
bash tests/lake/prices.sh            # the build job, on a network with no upstream
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

Both Rust tiers are hermetic. `PKDUMP_LAKE_DIR` selects a directory-backed
store instead of a bucket, which is what lets the landing zone's behaviour be
asserted without credentials or a network; the Python reader honours the same
variable for the same reason.
