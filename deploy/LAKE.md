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

## 6. Where the code is

| what | where |
| --- | --- |
| key layout, manifest, stores, config | `crates/pkdump-lake/` |
| the one place a response becomes bytes | `crates/pkdump-ingest/src/landing.rs` |
| flag → landing zone, manifest finalizing | `crates/pkdump-cli/src/landing.rs` |
| the acquisition phase it brackets | `acquire()` in `crates/pkdump-cli/src/{data,setup}.rs` |

Tests:

```bash
cargo test -p pkdump-lake            # key layout, manifest, config refusal
cargo test -p pkdump-ingest --test raw_landing   # the real clients, end to end
```

The `raw_landing` gate drives the real `reqwest` clients against a local
server and asserts the properties that matter: two runs on one date are
disjoint and neither overwrites the other; every recorded SHA-256 matches the
object actually stored; a fetch that fails partway leaves a manifest that
says so; and a landed import writes the *same rows* as an un-landed one —
landing is a tee, not a transform.

Both tiers are hermetic. `PKDUMP_LAKE_DIR` selects a directory-backed store
instead of a bucket, which is what lets the landing zone's behaviour be
asserted without credentials or a network.
