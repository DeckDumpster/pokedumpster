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

> **This file is about the CATALOG zone** — `raw/` and the `lake/` warehouse
> beside it: cross-tenant, shared, retained indefinitely, reached with the
> broad catalog credentials. The same bucket also holds the **tenant zone**
> under `tenant/`, which is a different object under different governance:
> always tenant-keyed, 90-day retention, its own credentials, plain Parquet.
> Its runbook is [`TENANT_ZONE.md`](TENANT_ZONE.md). Nothing in this file
> applies to it, and that separation is the reason "tenant data never enters
> the lake" still holds.

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

**One carve-out, and it is not a softening of that rule** (pd-nons): a
pokemontcg.io tail that fails no longer ends the acquisition — the run carries
the error and goes on to fetch TCGCSV, which is the half a night cannot get
back. Every dataset after the tail therefore *did* run to its end, so
`finalize` is called with no run-level error and each manifest is judged on its
own failures. `sets` (and `cards`, if it got that far) read incomplete because
`fetch_bytes` recorded their failure; `groups`, `products` and `prices` read
complete because they are. The conservatism above still applies to everything
that genuinely cuts a run short — a TCGCSV failure still stops the acquisition
and still marks the whole invocation incomplete.

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

That drop-in sets the variable in the *unit's* environment, and the unit runs
`deploy/refresh.sh`, which forwards it into the container along with the
bucket, region and profile from `lake.env` and mounts the instance's
`~/.config/pkdump/<instance>/aws/config` beside the
`pkdump-<instance>-s3-bootstrap` secret. Nothing else is needed: everything
landing requires comes from that one file plus the credentials the Litestream
sidecar already uses.

Verify it reached the process — the run says where it is landing, before the
first fetch:

```bash
systemctl --user start pkdump-refresh@<instance>.service
journalctl --user -u pkdump-refresh@<instance>.service | grep 'Landing raw'
#   Landing raw upstream responses in s3://<bucket> (ingest_date=YYYY-MM-DD)
```

#### Why the refresh runs in its own container

Until Aug 2026 the unit was `podman exec systemd-pkdump-<instance> pkdump data
refresh`, reaching into the already-running server. **`podman exec` does not
forward the calling process's environment** — the exec'd process gets the
container's env plus explicit `-e` flags and nothing else — so the drop-in
above reached the unit and never reached the refresh (`pd-vk22`):

```console
$ PKDUMP_LAND_RAW=1 podman exec systemd-pkdump-mutant sh -c 'echo ${PKDUMP_LAND_RAW:-<unset>}'
<unset>
```

The result was a green nightly timer that landed nothing, with no error
anywhere — the one state §3 says must not exist.

`-e` would have fixed half of it. The app container mounts the data volume and
nothing else (`pd-8gjd`): no AWS config, no bootstrap secret, no lake settings.
`podman exec` cannot add a mount to a running container, and mounting the
lake's credentials on the app container would hand the always-on web server
ambient write access to the lake bucket — the coupling `pkdump-lake` is
offline-only to prevent. It would also make the bootstrap secret a hard start
dependency of the *server*, so every instance without one would stop serving.

So the refresh runs in its own container from the same image over the same
volume, which is what `deploy/derive.sh` and `deploy/value-snapshots.sh`
already do. The landing half was the last job still borrowing the server's
container to get its work done.

Four refusals keep the silent no-op from coming back, and
`tests/deploy/run.sh` §13 drives all of them:

| you did | it does |
| --- | --- |
| asked to land with no `lake.env` on the box | refuses before the first fetch, naming the file to write |
| asked to land with a `lake.env` that does not set `PKDUMP_LAKE_S3_BUCKET` / `_REGION` | refuses before a container starts, naming the **host** file and both variables |
| asked to land into S3 with no credentials mounted | says so by name, then fails at the first PUT |
| asked to land, and the run never opened a landing zone | **fails the unit** — the catalog is fine, the wiring is not |

The second one is checked on the host rather than left to the binary
deliberately. The binary refuses too, but from inside the container, where
`$HOME` is `/root` — so its message names `/root/.config/pkdump/lake.env`, a
path that exists on neither side and says nothing about the file the operator
has to fix. The wrapper is the process that read the real file, so it is the
one that can name it. This is `pd-ub8n`'s exact failure: the file on the box was
written from the design note as `PKDUMP_LAKE_BUCKET` / `_REGION` /
`_RAW_PREFIX`, which nothing reads.

It stays a refusal rather than an alias table. Teaching the code to accept both
spellings is the fallback logic this project's No-Fallback convention forbids,
and a half-configured lake that half-works is worse than one that stops.

A refresh nobody asked to land is untouched by any of it, on a box with no
lake configuration at all.

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
fetch that cuts the acquisition short marks every dataset it touched incomplete
even where every fetch succeeded. That is the flag working as designed — it
says "do not assume this is the whole day", not "these bytes are bad". Read the
manifest's `failures[]`, and if the failures are all in other datasets,
`--allow-incomplete` is the right answer and the snapshot will say that is what
you did.

A **failed pokemontcg.io tail** is the one case that no longer does this
(pd-nons, §1): it does not cut the acquisition short, so `prices` reads
complete and only `sets`/`cards` read incomplete. Building the price table from
such a day needs no flag at all.

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

### It is on a timer (`pd-up36`)

`pkdump-prices@<instance>.timer`, whose service runs `deploy/prices.sh` — the
invocation above with the instance's network, image and lake credentials
resolved from where they actually live.

```bash
systemctl --user enable --now pkdump-prices@prod.timer
bash deploy/prices.sh prod                            # or run one now
bash deploy/prices.sh prod --ingest-date 2026-08-09   # rebuild an older day
```

It sits in the middle of the nightly chain — **land → derive → prices → ship
→ transform** — and each link is a declared ordering dependency, never an
inference from several timers sharing 07:00. Without it, the transform tier (§7)
valued every tenant's collection from whatever day someone last built by hand:
correct arithmetic over stale prices, advancing every night, with nothing
anywhere saying the numbers had stopped moving.

Three things about the scheduling are decisions:

**The nightly build passes `--allow-incomplete`.** The refusal above is right
for a hand run and wrong for a timer: `complete` is conservative across
datasets, so a pokemontcg.io tail that died marks the *prices* manifest
incomplete on a night when every price fetch succeeded — the normal shape of a
flaky night. A unit that failed there would page most nights, and a pager that
cries wolf gets ignored (this project has already paid a day for that shape,
`pd-me6h`). So the day is built and the snapshot records
`pkdump.raw-complete=false`, which is the difference between not knowing and
knowing-and-having-written-it-down.

**The alarm is on AGE instead**, and that is what `pkdump-lake-prices-age`
answers: how far behind today the newest `catalog.prices` partition is. More
than two days behind (`PKDUMP_LAKE_PRICES_MAX_AGE_DAYS`) pages; a table nothing
has ever built is the same verdict. It reads partition *metadata*, not data
files, so the cost does not grow with the lake. It runs on **every** run, not
only after a failed build: a check wired to the failure path fires on almost no
night and nobody would notice it had broken — and on the success path it is
also the only thing that asks the table rather than believing the build's
report of itself.

**0, 2 and 1 are three different answers.** 0 is today's partition built over a
fresh table. 2 is a build that produced nothing today over a table still inside
the window — one missed night, which tomorrow's build fills in; the unit's
`SuccessExitStatus=2` keeps it out of `failed` while the wrapper prints
`MISSED` and pushes a warning. 1 is a stale table, or an age that could not be
established at all: not being able to ask is not the same answer as "fine".

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

**Where the holdings come from: the tenant zone, and nowhere else.** The
copies it values are read from `zone_holdings`, which `pkdump-ship holdings`
materialises out of the tenant zone (`pd-szh2`). The online read of each
tenant's live `collection` table shipped beside it for exactly one item and
was then deleted (`pd-i08u`), so there is no `--holdings` flag to choose with
and no fallback when the zone has not been read back — a tenant whose
`zone_holdings` is missing, or is behind what the shipper has put in the zone,
is **skipped naming the command that fixes it**. `bash deploy/ship.sh
<instance>` is what runs both halves. See `TENANT_ZONE.md` §7.

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
consequence of the split, and it is why the job is on a timer rather than
remembered by hand (`pd-8m5c`). A day it does not run is a gap in every tenant's
chart — recoverable, because `--date` reconstructs any day still in the lake, but
not self-healing.

```bash
systemctl --user enable --now pkdump-value-snapshots@<instance>.timer
systemctl --user list-timers 'pkdump-*'
bash deploy/value-snapshots.sh <instance>                  # run one now
bash deploy/value-snapshots.sh <instance> --date 2026-08-09 # backfill a day
```

`deploy/value-snapshots.sh` is what the unit runs: it is the podman invocation
above with the instance's lake network, data volume and credentials resolved from
where they actually live. Installed for every instance by `deploy/setup.sh` and
re-rendered by every `deploy/deploy.sh`, enabled for none — the lakehouse is
opt-in per box, and the unit's `ConditionPathExists` on `lake.env` keeps an
enabled timer inert until `deploy/setup-lake.sh` has run.

**The ordering is declared, not timed.** The transform values a collection from
`catalog.prices`, which is built from what the nightly refresh lands, so the two
must never run beside each other. `After=pkdump-refresh@%i.service` is what
guarantees that: if the refresh still holds a job when this timer fires, this
unit's start job waits for it. (Not `Wants=` — the refresh is a oneshot without
`RemainAfterExit`, so pulling it in would re-run the whole catalog fetch a second
time every night.) The same line is there for the derive and the price build.

Since `pd-i08u` there is a fourth, and it is a *data* dependency rather than a
mutual-exclusion one: `After=pkdump-ship@%i.service`, because that unit's
second half is the only thing that writes the `zone_holdings` this job values.
The two swapped places — the shipment used to run last, after the transform —
so the chain is now

    land → derive → prices → **ship (+ read back)** → transform

and the timer's `OnCalendar=07:30` is *derived* from the shipment unit's own
declared bounds (`07:00` + `TimeoutStartSec=1800`) rather than guessed at.
`tests/deploy/run.sh` §10 recomputes that arithmetic — and asserts the old
`After=` is gone from the other file, because two units each ordered after the
other is a dependency cycle systemd resolves by dropping a job rather than by
complaining.

**0, 2 and 1 are three different answers, and the unit knows it.** Exit 2 —
completed, some tenant skipped because their database is absent or someone holds
a write lock on it — is a normal partial run, not a failure: a tenant mid-import
or a restore in flight produces it. `SuccessExitStatus=2` keeps the unit out of
`failed` so it does not page and does not leave the timer's last run sitting red,
while the wrapper prints a `PARTIAL` line naming the skipped tenants and pushes a
Pushover *warning* — so a half-completed run is neither silent nor an alarm. Exit
1 is a real failure and `OnFailure=pkdump-alert@%n.service` pages for it.

**`catalog.prices` is built by the unit before this one** (§6, `pd-up36`).
`pkdump-prices@%i` is ordered `After=` the derive and this unit is ordered
`After=` it, so the night runs land → derive → prices → ship → transform in sequence
however long each takes. Until that unit existed this job valued each night's
collection from the newest partition the lake happened to hold — correct
arithmetic over stale prices, advancing nightly. Arming this timer without
arming that one reintroduces exactly that, which is why `deploy/setup-lake.sh`
now names both.

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

## 8. The offline catalog derive: `shared.sqlite` from `raw/`

The transform tier above produces a *tenant* artifact. This one produces the
**catalog** — the 1.5 GB, 20-table `shared.sqlite` every instance serves — from
one `raw/` partition, replaying the responses that partition holds rather than
fetching anything.

```bash
# Rebuild the catalog from a specific day's raw. There is no default date.
podman run --rm -v pkdump-prod-data:/data:Z \
    -e PKDUMP_LAKE_S3_BUCKET=... -e PKDUMP_LAKE_S3_REGION=... \
    --entrypoint pkdump-lake-derive localhost/pkdump:prod \
    shared --ingest-date 2026-08-11 --db /data/shared.sqlite --data-dir /data

# Or the way the timer runs it, which resolves all of that from the box:
bash deploy/derive.sh prod                            # today (UTC)
bash deploy/derive.sh prod --ingest-date 2026-08-09   # rebuild an older day

# Compare two catalogs row by row — the acceptance instrument, shipped:
podman run --rm -v /some/dir:/c:Z --entrypoint pkdump-lake-derive \
    localhost/pkdump:prod diff --left /c/a.sqlite --right /c/b.sqlite \
    --exclude raw_derivation
```

### The rule this is shaped by

> Only lakehouse code reads `raw/`. The shared and tenant databases are derived
> from that, so whatever produces them is **also** lakehouse code.

That is why this is a separate binary in a separate crate and not a
`--from-raw` flag on `pkdump data refresh`: a flag would put a raw reader
inside `pkdump-cli`, on the **online** side, which is exactly the coupling the
rule exists to break. `pkdump-lakehouse` is bin-only, so no online target can
link it even by accident.

It is not a second derivation either. The pipeline it runs is
`pkdump_derive::derive` — the same function the online refresh calls, moved out
of the CLI unchanged. Only where the bytes come from differs.

### Two units, and the trap they are arranged against

| unit | what it does |
| --- | --- |
| `pkdump-refresh@<instance>` | **lands**: fetches the upstreams, and with `--land-raw` puts every response in the bucket |
| `pkdump-derive@<instance>` | **derives**: rebuilds `shared.sqlite` from one partition |
| `pkdump-ship@<instance>` | **ships and reads back**: the outbox into the tenant zone, and the zone into each tenant's `zone_holdings` |
| `pkdump-value-snapshots@<instance>` | **transforms**: values every tenant's `zone_holdings` from `catalog.prices` |

Separate units are what let a derive run against yesterday's raw on a night the
fetch failed. The trap on the other side of that is *yesterday's raw silently
deriving today's catalog and looking current*, and four things close it:

- **`--ingest-date` never defaults from the clock.** Rebuilding an older day is
  the same operation, and a job that reads the clock has two behaviours where
  it should have one. `deploy/derive.sh` names today's UTC date explicitly —
  the scheduler is the component allowed to know what day it is.
- **The partition it was asked for must EXIST and be COMPLETE.** No fallback to
  the newest available. A date that landed nothing refuses; a date whose only
  run died partway refuses, because nothing in the landing zone can say whether
  an incomplete run's parts add up to a day, and a catalog that is quietly
  smaller reads as *cards that do not exist*.
- **Re-deriving a date replaces it.** Twice equals once.
- **Provenance**: `shared.raw_derivation` records which run ULIDs produced the
  catalog, how many parts each held, and when the derive ran — so a rerun is
  *identifiable*, not merely tolerated.

```sql
-- which bytes is this catalog actually made of?
SELECT ingest_date, source, dataset, run_id, parts, observed_at, derived_at
  FROM raw_derivation ORDER BY ingest_date DESC, source, dataset;
```

`observed_at` is the run's clock **day** and is deliberately not the same column
as `ingest_date`. They agree for almost every run and differ for exactly the one
that crossed UTC midnight — the run where taking the partition for the
observation date would file yesterday's prices under today.

### The clock

Row-identity needs the derive to reproduce timestamps, not approximate them.
`Manifest.started_at` is where the landing run wrote down the instant it read
once; the derive reads it back and stamps the same values into
`sets.ptcgio_fetched_at`, `tcgcsv_products.fetched_at`, `prices.observed_at` and
`printings.deprecated_at`. A partition landed before that field existed is
**refused by name**, not defaulted — see `crates/pkdump-derive/src/clock.rs`.

Runs that disagree about it are refused too. A derive reproduces *one fetch*,
and rows built from one run's bytes and another run's clock are neither run's
output. It happens when one date was landed by two invocations (a `setup` and a
`refresh` the same day); derive a date one run landed, or re-land it.

### A URL the partition does not hold is a REFUSAL (item 4)

A URL the partition has no record of means raw coverage has regressed: the
derivation grew an input the landing zone does not capture, or an upstream's
origin moved. The run stops, exit 1, naming the URL and telling you to re-land
the date:

```
raw/ has no record of https://tcgcsv.com/tcgplayer/3/9/prices.
The landing zone no longer covers this derivation's inputs: either an endpoint
was added without landing it, or the upstream's origin moved. Re-land the date
(pkdump data refresh --land-raw) and derive again.
```

**What to do about it.** Re-land the date — `systemctl --user start
pkdump-refresh@<instance>.service`, the landing half of the pair, or check that
it ran at all — then derive again.
Deriving a *different*, complete date is the other option — the catalog is a
day behind rather than wrong. `pkdump-derive@` has no `SuccessExitStatus=`, so
this pages through `OnFailure=`, which is intended: a catalog that quietly
skipped a price feed reads downstream as cards that do not exist.

Item 2 of the epic shipped a temporary fallback here — reach the live upstream,
print `!! raw coverage has REGRESSED`, finish anyway — with
`--no-upstream-fallback` as the opt-out. **Both are gone** (pd-6yql), removed
once pd-vves proved row-identity against the real bucket. The flag is rejected
by name rather than ignored, so an old invocation carrying it fails loudly
instead of appearing to work. `deploy/derive.sh` no longer reads the job's
output and pushes no coverage warning of its own.

The reason it could not stay: a run that needed the fallback was **correct but
not reproducible from the lake**. Every gate passed, every row looked right, and
the failure would have surfaced on the day an upstream was down — the day the
lake was bought for. Loudness mitigated that; it depended on somebody reading
the log.

**Set symbols are not an exception to this.** `symbols::normalize_all_symbols`
still fetches PNGs from `images.pokemontcg.io` on the offline path, and always
did: images are deliberately outside `raw/` (§1), so the phase never entered the
replay layer at all — it has no `Wire` in its signature and builds its own HTTP
client. A symbol fetch is therefore not a replay miss, and a derive on a box
with no egress logs `WARN: symbol normalize <set>: …`, counts it, keeps the
set's upstream URL (which still renders) and **succeeds**. See `pd-5w4n`, and
`row_identical.rs::a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`
— the gate that holds the two apart, on a catalog whose symbols have never been
normalised.

### Proving it, on real data (pd-aer9)

Three gates, and each proves something the other two cannot:

| gate | upstream | what only it proves |
| --- | --- | --- |
| `crates/pkdump-lakehouse/tests/row_identical.rs` | a fixture on loopback | row-identity, idempotence, every refusal, the loud fallback — hermetic, in CI |
| `tests/lake/derive.sh` | none: an `--internal` network | the shipped image needs **no network** |
| `tests/lake/real_upstream_derive.sh` | **the real tcgcsv.com and api.pokemontcg.io** | row-identity on **real payloads at real scale** |

The third is not a CI gate and must not become one: it fetches ~1,350 real
responses and takes a few minutes of somebody's evening. It exists because the
first two have never seen a real byte, and a replay that got the 450-group
Japanese catalog, set discovery or a real pagination edge subtly wrong would
pass both.

```bash
cargo build --release -p pkdump-cli -p pkdump-lakehouse
PKDUMP_REAL_DERIVE=1 bash tests/lake/real_upstream_derive.sh
```

It builds a baseline catalog, copies it to both sides, runs `pkdump data
refresh --land-raw` into a landing zone **on local disk**, rebuilds the same
date from that partition with `--no-upstream-fallback`, and diffs the two row
by row. It writes nothing to any bucket and touches no instance.

**The run of 2026-08-13** (`ingest_date=2026-08-13`, `run=01KZWST5KCAXBK5JTGKNJ811AS`):
1,345 parts landed, 85.3 MB uncompressed / 11 MB stored; the online refresh took
**132s**, the offline rebuild from raw **35s**, and every one of the 1,345 URLs
was answered from `raw/` (`raw coverage: complete`). All 21 tables compared
equal, `raw_derivation` excluded and named:

| table | rows | table | rows |
| --- | --- | --- | --- |
| `cards` | 47,640 | `printings` | 75,627 |
| `sets` | 630 | `prices` | 289,255 |
| `tcgcsv_products` | 57,716 | `latest_prices` | 289,255 |
| `sealed_products` | 5,191 | `sealed_prices` | 4,136 |
| `tcgplayer_groups` | 671 | `variants` | 290 |

The clock was recovered from the manifests
(`2026-08-13T05:34:09.516217300+00:00`) rather than read, which is what makes
those `observed_at` and `fetched_at` columns compare equal at all.

Two honest caveats, both visible in the run's own output:

- **The symbol phase distinguished nothing here** — `0 processed, 0 cached, 175
  overrides, 0 failed` on both sides. A warm catalog's `sets.symbol_url` values
  already name `/sym/<set>.png`, so the http-prefix gate skips them. The gap in
  the section below is real; this run does not exercise it.
- **`pokemontcgio/cards` landed nothing**, because no set was new that night.
  That is the ordinary case (the dataset is `Optional` for exactly this
  reason), and it means the replay of *that* dataset is still only proven
  against the fixture.

### What is still NOT proven: prod's own `raw/`

The lake holds exactly one partition today, `ingest_date=2026-08-11`, landed by
hand by pd-fet2. **The derive refuses it**, correctly:

```
$ pkdump-lake-derive shared --ingest-date 2026-08-11 --no-upstream-fallback
Deriving ingest_date=2026-08-11 from s3://pkdump-lake-…/
  tcgcsv/products 2026-08-11: 01KZRWPR39WMHMARZ9WK5S0ND7 (671 part(s))
  …
Error: …: the run's manifest records no started_at, so the clock its rows were
stamped with cannot be recovered.
```

It predates `Manifest.started_at`. And the nights since have landed nothing at
all — `--ingest-date 2026-08-12` against the real bucket answers `no runs
landed … never falls back`, which is `pd-kncd` (the nightly's env never reaches
the process) seen from the reading end.

So the sequence, and it cannot be short-circuited: **pd-kncd lands** → one
night's raw lands with a clock in its manifests → derive *that* date from the
bucket and diff it against the catalog the same refresh built. Until then the
strongest available statement is the one above: row-identical on real upstream
payloads at real catalog scale, over a partition landed by the same writer prod
uses.

### Enabling it

Installed for every instance and enabled for none:

```bash
# On the deployment box, as the pkdump user:
systemctl --user enable --now pkdump-derive@prod.timer

# What it will run, and what to watch the first morning after:
systemctl --user list-timers 'pkdump-*'
journalctl --user -u pkdump-derive@prod.service -n 100
sqlite3 ~/.local/share/containers/storage/volumes/pkdump-prod-data/_data/shared.sqlite \
  'SELECT ingest_date, source, dataset, run_id, parts, derived_at FROM raw_derivation'
```

Do not arm it before `pkdump-refresh@prod` is provably landing raw (pd-kncd):
a derive with nothing to read is a refusal every night, and the unit has **no**
`SuccessExitStatus=`, so it will page.

Think before you do, even then. Today `pkdump data refresh` still derives the
catalog inline, so an enabled derive timer rebuilds it a second time from raw —
correct and idempotent, but redundant. It becomes the only builder in item 6 of
the epic. What is worth having now is the mechanism, the provenance, and the
proof.

### The one phase a replay cannot supply

`symbols::normalize_all_symbols` fetches PNGs from `images.pokemontcg.io`, and
images are deliberately **not** landed — the retention arithmetic that justifies
keeping `raw/` forever is for JSON only. On a box with no egress every symbol
fetch fails, is counted, and the affected sets keep their upstream symbol URL
(which still renders). The derive says so out loud rather than leaving it to be
discovered from a row count. Filed as **pd-5w4n**.

---

## 9. Where the code is

| what | where |
| --- | --- |
| key layout, manifest, stores, config | `crates/pkdump-lake/` |
| the one place a response becomes bytes | `crates/pkdump-ingest/src/landing.rs` |
| flag → landing zone (the WRITING half only) | `crates/pkdump-cli/src/landing.rs` |
| the acquisition phase it brackets | `acquire()` in `crates/pkdump-derive/src/lib.rs` and `crates/pkdump-cli/src/setup.rs` |
| reading `raw/` back — runs, manifests, payloads | `lake/src/pkdump_lake/raw.py` |
| `catalog.prices`, and only that | `lake/src/pkdump_lake/prices.py` |
| the check against `shared.sqlite` | `lake/src/pkdump_lake/verify.py` |
| per-tenant value snapshots (the transform tier) | `lake/src/pkdump_lake/value_snapshots.py` |
| what the timer runs it as | `deploy/value-snapshots.sh` + `deploy/pkdump-value-snapshots.{service,timer}` |
| reading `raw/` back, in Rust — the twin of `raw.py` | `crates/pkdump-lake/src/reader.rs` |
| the catalog derivation itself, both callers share it | `crates/pkdump-derive/` |
| the offline derive: partition choice, replay, the diff | `crates/pkdump-lakehouse/` |
| what its timer runs it as | `deploy/derive.sh` + `deploy/pkdump-derive.{service,timer}` |
| the provenance table | `crates/pkdump-db/src/raw_derivation.rs` |
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
cargo test -p pkdump-lakehouse       # partition choice, replay, the comparator —
                                     #   and tests/row_identical.rs, the whole
                                     #   acceptance matrix against the shipped
                                     #   binary as a subprocess
bash tests/lake/prices.sh            # the build job, on a network with no upstream
bash tests/lake/derive.sh            # the whole CATALOG from raw/, in the shipped
                                     #   image, with egress provably blocked
bash tests/lake/value_snapshots.sh   # the transform, for every tenant — and §10
                                     #   the SHIPPED wrapper the timer runs
bash tests/deploy/run.sh             # §10 the transform's scheduling — ordering
                                     #   after the refresh, 0/2/1, the derived
                                     #   calendar; §11 the derive's, plus the
                                     #   shipped wrapper its timer runs
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

`derive.sh` makes the same argument one level up, about the whole catalog
rather than one table. `pkdump-lake-derive` runs in the **shipped image** on an
`--internal` network — §2 tries to reach 1.1.1.1 from that image and requires
the attempt to fail — and its output is compared row by row against the
`shared.sqlite` the online refresh built from the same responses in the same
pass. **Row-identical, never byte-identical**: SQLite files differ on page
layout and vacuum state for identical content, so a file hash would fail for
reasons that mean nothing. The comparator is the shipped `pkdump-lake-derive
diff`, and every table it skips has to be named on its command line.

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

## 10. The first real run, and what it cost

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
