# The tenant zone

Holdings and valuations, offline, under governance that makes an account
deletion real.

This is a **different object** from the catalog zone described in
[`LAKE.md`](LAKE.md), and the difference is the whole point. They share a
bucket; they share nothing else.

| | catalog zone | tenant zone |
|---|---|---|
| prefixes | `raw/`, `lake/` | `tenant/` |
| keyed by | nothing tenant-shaped, ever | always `database_id` |
| retention | **indefinite**, deliberately unmanaged | **90 days**, lifecycle-enforced |
| credentials | reach `raw/` + `lake/`, denied `tenant/` | reach `tenant/`, denied the rest |
| format | Iceberg tables via Nessie | plain partitioned Parquet |
| deletion | n/a — nothing here is anybody's | a prefix drop |

The standing rule — *tenant data never enters the lake* — is **restated by
this, not broken.** That rule was always about the catalog: cross-tenant,
shared, retained forever. The tenant zone is governed separately, which is
the only reason it can exist at all.

---

## 1. The key layout

```text
tenant/database_id=<id>/dataset=holdings/as_of=YYYY-MM-DD/part-seq-<from>-<to>.parquet.enc
tenant/database_id=<id>/dataset=valuations/as_of=YYYY-MM-DD/part-NNNN.parquet.enc
```

Built by `pkdump_lake::tenant` — `tenant_prefix`, `partition_prefix`,
`part_key`, `range_part_key` — which is the one place these strings are
constructed.

**Every object here is sealed, so every key says `.enc`.** The bytes are
AES-256-GCM under that tenant's derived key (see [`KEYS.md`](KEYS.md)); a key
ending `.parquet` would describe something no reader could open. The envelope
is `pkdump_ship::cipher`.

**Holdings parts are named for the outbox range they carry**, valuations for
their ordinal. The difference is not cosmetic: holdings are shipped
incrementally and at-least-once, so a part has to be addressable by *what is
in it* if a retry is to land on the object it is retrying rather than beside
it. Valuations are recomputed whole for a date and have no such identity.
See §7.

**`database_id` is the first partition, above `dataset=`.** That is what
makes deleting a tenant one prefix drop covering their holdings *and* their
valuations. Derived artifacts inherit the deletion obligation, so they must
not need a second sweep to find; a layout that put `dataset=` on top would
mean every future dataset is another thing a deletion has to remember.
`tenant_prefix(id)` is that prefix and it is the unit `pkdump-erase`
drops — see §7 and [`DELETION.md`](DELETION.md).

**Plain Parquet, not Iceberg.** Iceberg records absolute paths in its
metadata, so moving this zone into its own bucket later would mean rewriting
manifests; plain files keep that a location change. It also gives up
snapshots and time travel *deliberately* — holdings want current state per
tenant, deletable, not history. (The catalog wants the opposite, which is why
the catalog is Iceberg.)

**Hive-style `key=value` components**, matching `raw/`, so DuckDB and pyarrow
recover the partition values from the path without a side table.

---

## 2. Retention is 90 days, and it is a product limit

The catalog's indefinite retention is justified by "we may need to rebuild
any historical price". **No equivalent argument covers holdings.** Two
consequences follow, and both are wanted:

- **90 days IS the backfill window.** A price correction reaches the last 90
  days and no further.
- **A missed deletion has a bounded blast radius.** Anything not explicitly
  deleted ages out within 90 days regardless — which is what makes 90 days
  safer than "indefinite, with a delete button".

It is enforced by an S3 lifecycle rule
([`policies/tenant-zone/lifecycle.json`](policies/tenant-zone/lifecycle.json)),
applied and then read back by `setup-tenant-zone.sh`. Not documented —
*mechanical*. What that read-back may and may not conclude is §4b: "there is no
rule" and "I could not read the rule" are opposite facts and get different
exits, because the repair for one is to apply a rule the other already has.

Retention is measured on **object age**, not on `as_of`. So a tenant whose
state is still current has to be re-materialised inside the window or they
age out of the zone. That is a constraint the shipper inherits, and it is
stated here because this is what makes it true.

> Raising the window is a **decision to file**, not a default to edit. It
> appears in three files that a gate holds together (§5); changing one is a
> failing test, which is the intent.

---

## 3. Separate credentials, from day one

One bucket means the *only* thing separating the two zones is a pair of
credential policies. A prefix boundary is a policy, and a policy is one
misconfiguration from being nothing at all — while looking identical from the
outside.

Two mirror-image documents, applied verbatim:

- [`policies/tenant-zone/catalog-credentials.json`](policies/tenant-zone/catalog-credentials.json)
  — allows `raw/` + `lake/`, **explicitly denies** `tenant/`
- [`policies/tenant-zone/tenant-credentials.json`](policies/tenant-zone/tenant-credentials.json)
  — allows `tenant/`, **explicitly denies** `raw/` + `lake/`

Both also deny `s3:PutLifecycleConfiguration` on the bucket: retention is not
theirs to widen.

The explicit `Deny` statements are not decoration, and this is asserted
rather than assumed (gate §6b): **an explicit Deny beats any Allow**, so a
later broad grant made somewhere else cannot silently widen either zone. That
is the property that makes these two documents safe to live with.

Listing is prefix-scoped via a `StringLike` condition on `s3:prefix`. Note
that `s3:prefix` is only a valid condition key for `s3:ListBucket` — AWS and
MinIO both **reject** a policy that pairs it with
`s3:ListBucketMultipartUploads`, which is why neither document contains that
action.

### Configuration

Host config, in the same `~/.config/pkdump/lake.env` the catalog zone reads —
one file for both halves, so a job can never end up reading one zone's
settings with another zone's credentials:

```sh
PKDUMP_TENANT_AWS_PROFILE=<profile>   # reaches tenant/ and nothing else
```

Resolved by `pkdump_lake::tenant::TenantZoneConfig`, with **no default**, and
two refusals:

- unconfigured → refuses, naming the file. Falling back to the catalog
  profile would erase the boundary silently.
- `PKDUMP_TENANT_AWS_PROFILE == AWS_PROFILE` → refuses. One profile for both
  zones is not a narrow policy that happens to be wide; it is **no boundary
  at all**, and a zone governed by nothing looks exactly like a zone governed
  correctly.

Credentials themselves stay auto-refreshing role assumption, as everywhere
else here. Nothing holds a long-lived key.

---

## 4. Running it

```bash
# Apply the 90-day retention, then verify what actually landed.
# --profile names the identity; §4a says which one it has to be, and why it
# is neither of the two zone credentials.
bash deploy/setup-tenant-zone.sh --apply --profile pkdump

# Verify only — safe to run any time; the exit code is the answer (below)
bash deploy/setup-tenant-zone.sh --check --profile pkdump

# Render the two IAM documents with the bucket substituted, to attach
bash deploy/setup-tenant-zone.sh --render --out /tmp/pkdump-policies
```

The bucket comes from `PKDUMP_LAKE_S3_BUCKET` in `lake.env`, or `--bucket`.

**Retention is applied; IAM is only rendered.** Role ARNs, trust policies and
the assume-role chain are not facts this repo holds, so the documents are
printed for the operator to attach. Bucket configuration it can do; account
configuration it must not guess.

`--apply` always re-reads the rule afterwards and checks it, because a
lifecycle PUT that scoped itself to the wrong prefix succeeds *identically*
to one that did not.

### 4a. Which identity applies retention — and which one may check it

**`role/pokedump-data`, reached as `AWS_PROFILE=pkdump`.** Write that down
before reaching for the zone credentials, because they are the obvious guess
and they are the wrong one *by design*:

> Both `catalog-credentials.json` and `tenant-credentials.json` **explicitly
> deny** `s3:PutLifecycleConfiguration`. Retention is not theirs to widen — a
> zone credential that could rewrite the rule bounding its own blast radius
> would make the 90-day limit advisory. So the identity that applies retention
> is deliberately a *third* one, and on 2026-08-26 it did not exist on the box:
> the permission was granted to `role/pokedump-data`, which is what
> `AWS_PROFILE=pkdump` assumes.

Checking needs `s3:GetLifecycleConfiguration` on the bucket, and that is a
separate grant from the read/write access to either prefix. An identity that
can list `raw/` all day may still be unable to read the bucket's lifecycle.

**Pass `--profile`.** With it omitted the script acts as whatever the
environment holds; on the night this was written that was *another project's
backup user*, pointed at this project's lake bucket. It failed safe only
because that user had no lifecycle permission anywhere. The script now resolves
and prints its identity before it acts —

```
==> Identity: arn:aws:sts::…:assumed-role/pokedump-data/…
    named by --profile pkdump
```

— and names it again in every refusal, because a denial is only diagnosable if
you know who was denied. An endpoint that does not implement
`sts:GetCallerIdentity` (MinIO does not) prints `UNRESOLVED` with the reason
rather than stopping; a governance script that cannot run against a stand-in
bucket is one that cannot be tested.

### 4b. `--check` has three answers, and they are not interchangeable

| exit | verdict | what it means | repair |
|---|---|---|---|
| 0 | correct | the rule is present and is the rule | — |
| 1 | **PRESENT BUT WRONG** | a rule exists and is not the rule: wrong window, or a prefix reaching the catalog. Every configuration refusal below is also 1 | fix the rule |
| 2 | usage | — | — |
| 3 | **ABSENT** | no lifecycle configuration at all, or none of its enabled rules covers `tenant/`. Retention was never applied | `--apply` |
| 4 | **CANNOT VERIFY** | the read itself failed — `AccessDenied` first among them. Says nothing about whether a rule exists | grant `s3:GetLifecycleConfiguration`, re-check |

Exit 3 and exit 4 were **one sentence** until `pd-2hnp`: `--check` printed "no
lifecycle configuration" for a genuinely absent rule *and* for an
`AccessDenied`. "There is no retention rule" and "I am not allowed to look" are
opposite facts.

The direction that hurts is not the one it was found in. Once the rule IS
applied, an operator whose credentials cannot read it is told the rule was
never applied — and the repair for *that* is to apply or widen one, against a
bucket whose retention is already correct. **A governance check that cannot
tell absent from forbidden is worse than no check, because it is trusted.**

So an unrecognised error is classified as 4 as well, never as 3. The one place
this script may not guess is about a deletion rule it could not read.

### What it refuses

Its output is a rule whose job is deleting objects, so every way of pointing
it somewhere unintended is a stop:

- no bucket configured → names `lake.env`, stops
- the **Litestream backup bucket** → stops. That bucket holds the only
  irreplaceable data in the system
- a rule reaching `raw/` or `lake/`, or one with no prefix at all → stops.
  A whole-bucket expiry would quietly start deleting the catalog
- 90 changed to anything else → stops

---

## 5. The gate

`tests/lake/tenant_zone.sh` (container tier, run by `deploy/ci.sh`). MinIO
stands in for the bucket, and the real `setup-tenant-zone.sh` and the real
policy documents are what it exercises — an IAM policy document is the same
dialect for AWS and MinIO, which is the only reason this is testable rather
than trustable.

**Every claim is seen both green and red:**

| § | claim |
|---|---|
| §1/§1b | retention applied by the real script; it refuses an unnamed bucket and the backup bucket |
| §2 | the retention check goes **red** and the **exit code is asserted**: whole-bucket rule, 365 days and a second rule reaching `raw/` each give 1; the rule deleted gives 3 — then green again |
| §2b | the same **correct** rule read by a real credential MinIO really denies gives **4, not 3**, names `s3:GetLifecycleConfiguration`, and never renders the ABSENT verdict — then the rule it could not see is confirmed still applied |
| §3/§4 | the two identities carry the rendered documents, and each **can** reach its own zone (so a denial below is a denial, not an absence) |
| §5 | **the boundary, both directions** — catalog cannot read/list/write `tenant/`; tenant cannot read/list/write `raw/` or `lake/` |
| §6 | **the same assertion functions go red** when a credential is replaced by a whole-bucket grant — then green again |
| §6b | a whole-bucket grant added *beside* either policy still cannot cross: the Deny statements work |
| §7 | that gate's own fixtures leave the zone **empty**, and the catalog is untouched |
| §8 | the prefixes and the window have not drifted across their three homes |

§6 is the point of the file. A boundary check that has only ever been seen
passing is not known to check anything, so it runs the *identical functions*
§5 ran against a deliberately broken configuration.

`tests/deploy/run.sh` §15 is the other half of §2/§2b, and it is hermetic and
sub-second: the classification is pure string work over what the aws CLI says,
so it is pinned there against a stubbed CLI — six answers including the two the
container tier cannot easily stage (an error nobody anticipated, and a
lifecycle document that exists but says nothing about `tenant/`). The container
tier is where a **real** credential is **really** denied by MinIO; this is where
the mapping from what the CLI said to which of the three answers it is has been
seen red for every case.

§8 exists because the prefixes and the retention live in three places that
**cannot** share code — Rust (the shipper reads them at runtime), the policy
documents (AWS reads them) and the bash script. A prefix changed in one and
not the others does not fail loudly; it silently widens a policy.

---

## 6. What fills it: the shipper (`pd-dxn3`)

`pkdump-ship` moves every registered tenant's ownership outbox (`pd-5m54`)
into this zone, encrypted under that tenant's derived key (`pd-ulds`). It is
the only thing in the workspace that writes under `tenant/`.

```bash
# Every registered tenant. What pkdump-ship@<instance>.service runs.
bash deploy/ship.sh <instance>

# One tenant, after a repair.
bash deploy/ship.sh <instance> --tenant alice

# What is unshipped, and every gap ever recorded, per tenant.
podman run --rm -v pkdump-<instance>-data:/data --entrypoint pkdump-ship \
    localhost/pkdump:<instance> status --data-dir /data

# Read one part back. The database_id= component of the key says whose key to
# derive, so there is no flag for it — and a part only opens under the prefix
# it was written to.
podman run --rm -v pkdump-<instance>-data:/data \
    -v ~/.config/pkdump/<instance>/tenant-master.key:/keys/tenant-master.key:ro \
    -e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
    --entrypoint pkdump-ship localhost/pkdump:<instance> \
    decrypt --data-dir /data --key 'tenant/database_id=…' --json
```

**It reads no clock.** `as_of` comes out of each event's own `occurred_at`, so
re-shipping a range on a later day lands in the partition it landed in the
first time. There is no `--date`.

**Delivery is at-least-once and idempotent.** The cursor
(`ownership_outbox_cursor`, in the tenant's own database) is written *after*
the object lands, so a crash in between repeats a part rather than losing one
— and because a part is addressed by its sequence range and sealed
deterministically, the repeat is byte-identical and lands on the same key.

### Four exit statuses, and 3 is the one to know

| exit | means | the unit does |
|---|---|---|
| 0 | every registered tenant shipped | nothing |
| 2 | the run finished; some tenants skipped | `SuccessExitStatus=2`, a Pushover warning naming who |
| 3 | **SEQUENCE GAP** — events were LOST | fails the unit, plus its own alarm naming the tenant and range |
| 1 | the run never started, or shipped nobody | fails the unit |

A **gap** means the outbox's monotonic sequence has a hole. That sequence is
gap-free by construction (`AUTOINCREMENT`, written by a trigger inside the
mutation's transaction, never reissued), so a hole is not a curiosity — it is
proof that an event existed and was lost, and this zone is therefore missing
it. The shipper records the missing range in that collection's
`ownership_outbox_gap` **before** the cursor moves past it, because once past
nothing can detect it again.

It does **not** stop shipping on a gap. The missing rows are already gone;
withholding the rows that survive would be a second loss caused by detecting
the first. To see what has been recorded:

```sql
-- in tenants/<database_id>.sqlite
SELECT from_seq, to_seq, detected_at FROM ownership_outbox_gap ORDER BY from_seq;
```

Nothing clears that table but an operator who has reconciled it. The shipper
never does — reconciling is not the shipper's job.

A **tombstoned** tenant (see [`KEYS.md`](KEYS.md)) is skipped and is *not* an
anomaly: their key refuses to derive, so nothing of theirs can be written, and
the run is still clean. A tenant nobody has run `pkdump keys register` for is
skipped as a **warning** — absence is not permission.

### Scheduling

`pkdump-ship@<instance>.timer`, installed for every instance and **enabled for
none**. It fires at 07:00 with the rest of that wave, `After=` the landing, the
derive and the price build; the transform is the unit that now waits, at 07:30,
derived from this unit's own bounds. The chain is

    land → derive → prices → **ship (+ read back)** → transform

and this unit runs BOTH halves (`deploy/ship.sh`): `pkdump-ship run` out to the
zone, then `pkdump-ship holdings` back into each tenant's `zone_holdings`. Since
`pd-i08u` that second half is the only input the transform has.

```bash
bash deploy/setup-lake.sh <instance> --arm-shipper
```

That is `systemctl --user enable --now pkdump-ship@<instance>.timer` with the
three preconditions **checked first** (`pd-0h2p`), and it refuses, naming which
one, rather than arming:

| 1 | `PKDUMP_TENANT_AWS_PROFILE` is set in `lake.env` and is **not** `AWS_PROFILE` |
| 2 | the master key exists, at mode 600 on the host |
| 3 | every registered tenant has a completed full backfill on record |

Check 3 is the one with no other witness. It is asked of the collections
themselves — `pkdump outbox status --all-tenants --require-backfill`, inside
the instance's own image over its own data volume — and answered by an **exit
status**, never by reading the report's text.

**Do not arm it on a box before the backfill has run.** The shipper ships the
OUTBOX, and an existing collection's outbox starts empty (`pd-whsw`): armed
early it faithfully ships every change made from tonight and nothing anybody
already owns — a zone that looks populated and is not, valued nightly into a
plausible number, with nothing reporting it. `pkdump outbox emit --all
--all-tenants` (`pd-385w`) is what makes the outbox describe the collection
that is already there; arm the timer after it, per instance. `pkdump outbox
status`, and `ownership_emit_log` in the tenant's own database, are how you
read the detail: a `backfill` row naming the range it covered and when it
finished.

**One precondition is stated and not checked, because nothing on the box can
check it**: that the master key has been BACKED UP. A lost key is
indistinguishable from a deleted tenant by design ([`KEYS.md`](KEYS.md) §1),
and from the night it is armed this timer writes data only that key opens.

**prod is armed** (`pd-r130`, 2026-08-26), after confirming exactly that —
`ownership_emit_log` recording a completed `backfill` of seq 1..4814.

**And on a box where the transform timer is already armed, arming this one is
no longer optional.** Until `pd-i08u` the transform valued the live collection
and this unit was purely additive; now the transform has no other source, so a
box running `pkdump-value-snapshots@` without `pkdump-ship@` records no value
history for anybody and says so nightly (exit 1, "nobody was snapshotted at
all"). Enable them together, backfill first.

**A box that has been shipped to ONCE and then left unarmed is worse, and it
is silent.** `zone_holdings` exists, so the transform does not skip anybody —
it values the collection as the zone last saw it, at exit 0. Phase 3 refuses
only the inverse (a materialisation older than the cursor); a zone merely
*behind the outbox* is deliberately not refused, because that normally means
the shipper skipped that tenant and the shipper says so in the same nightly
run — which is an argument that needs a nightly shipper run to exist. `pd-r130`
found prod in exactly that state and reproduced it before arming: one card
added, no shipment, and the transform reported `1 tenant(s) snapshotted, 0
skipped` over `through seq 4814`. Nothing about the run looks wrong; the number
is simply a day out of date, and it drifts further every day.

If you are not sure which state a box is in, ask the tenant's own database —
these three should be one number:

```sql
SELECT (SELECT MAX(seq)       FROM ownership_outbox)        AS outbox_max,
       (SELECT shipped_thru   FROM ownership_outbox_cursor) AS cursor,
       (SELECT max_seq        FROM zone_holdings_run)       AS zone_max;
```

### Gates

`cargo test -p pkdump-ship` is hermetic: part planning and gap detection, the
sealed envelope, the Parquet encoding, and `tests/shipping.rs`, which proves
the four claims end to end over a `DirStore` — a dropped sequence number is
detected and recorded, the same rows shipped twice leave the zone
byte-identical, a crash between the PUT and the cursor re-ships that part and
nothing else, and each tenant's parts open only under that tenant's own key.
It also reduces a shipped part with `pkdump_db::outbox::project` — the one
implementation of the resolution rule — and checks the result against the
tenant's live collection, which is what "the zone is the collection" means and
what a part decoding into the outbox's own `Event` type is for. And it covers
the seam with the backfill: a collection whose holdings predate
the triggers, put through `pkdump outbox emit`, ships as ordinary events —
into the partitions those rows' own timestamps name, not the day the backfill
ran.

`tests/lake/shipper.sh` is the container tier: the shipped image against a
real MinIO under the **rendered** IAM documents, with the catalog role's
inability to read shipped holdings asserted in the failing direction too, a
real process killed mid-run and resumed, and `deploy/ship.sh`'s four exit
statuses.

`tests/lake/tenant_isolation_test.sh` is the source-level boundary (lint tier,
hermetic, pd-cgi9 re-cut by pd-7x83). It is where "the tenant zone is
tenant-keyed and the catalog zone is not" stops being a convention: every key
builder in `crates/pkdump-lake/src/tenant.rs` takes a `database_id`, the zone
resolves no tenant identity of its own, the shipper names no catalog prefix,
entry point or credential, and the online path links neither zone. Its own red
proof is `tests/lake/tenant_isolation_selftest.sh`, which breaks each of those
in a copy of the tree and requires the matching assertion to fail — and
requires the tenant zone's *legitimate* tenant-keying to fire nothing.

---

## 7. What reads it: Phase 3, the valuation (`pd-szh2`, `pd-i08u`)

A collection's value is `catalog.prices` × what that collection holds. Until
`pd-szh2` the second half came from the tenant's live `collection` table — the
write moved offline, the read stayed online, and closing that is what this epic
is for. Phase 3 values a collection from **the zone**, and since `pd-i08u`
there is no other way to value one: the online read is deleted, there is no
`--holdings` flag, and a tenant whose zone has not been read back is skipped
rather than valued from somewhere else.

It is two processes, and the split is the point:

```bash
# 1. The outbox -> the zone -> a staging table in each tenant's own database.
#    Both halves are deploy/ship.sh, which is what the nightly timer runs.
bash deploy/ship.sh <instance>

# …or by hand, which is the same two commands. App image: they need the master
#    key and the tenant profile, which nothing else on the box holds.
podman run --rm -v pkdump-<instance>-data:/data \
    -v ~/.config/pkdump/<instance>/tenant-master.key:/keys/tenant-master.key:ro \
    -e PKDUMP_MASTER_KEY_FILE=/keys/tenant-master.key \
    --entrypoint pkdump-ship localhost/pkdump:<instance> \
    holdings --data-dir /data

# 2. The valuation, out of that table. No flag chooses it — it is the source.
pkdump-lake-value-snapshots --date <date> --data-dir /data
```

**Why two processes.** The zone side is Rust — the envelope, the key
derivation and the resolution rule each have exactly one implementation and it
is `pkdump-ship`. The price side is Python, because `catalog.prices` is
Iceberg and `pyiceberg` is the only client here. Writing either one again in
the other language would produce a *second* implementation of the thing this
item has to prove identical, which is the one mistake that would make the
proof worthless.

So the seam is a table. `pkdump-ship holdings` reduces the zone with
`pkdump_db::outbox::project` and writes `zone_holdings`; the transform's
existing SQL reads that name instead of `collection` and **nothing else
changes**. One token. A difference between the two valuations is therefore a
difference in holdings and cannot be a difference in arithmetic.

`zone_holdings` is created from `collection`'s own `pragma_table_info`, never
declared, so a column added to `collection` reaches it with no change here —
and it carries no triggers, so materialising cannot emit outbox events and a
Phase 3 run cannot feed itself. `zone_holdings_run` beside it records which
parts produced it. All are transport state: none travels in the portable
JSON envelope, and the deletion that drops a tenant drops them.

**One staging table per outbox source (`pd-bbv7`).** The zone has always
carried both holdings tables — `collection` and `sealed_collection` — and the
read-back used to decline the second, on the grounds that valuing sealed
product was a decision rather than a table name. It is now a decision that has
been taken, so `sealed_collection` lands in `zone_sealed_holdings`, built from
its own `pragma_table_info` the same way. Two tables, never one: `row_id` is
unique only *within* a source (`pd-4gop`), so the first single and the first
sealed lot are both 1 and reducing them together would merge two unrelated
holdings into a plausible wrong number. `zone_holdings_run.sealed_rows` counts
the second half beside `rows`.

An event from a source this reader has **no** staging table for is still
declined — and now counted **by name**, per table, on stderr. The single
number it used to print is precisely what let half a tenant's holdings sit
unvalued without anybody being able to see what was being ignored;
`pkdump_ship::zone::every_outbox_source_has_a_staging_table` is the gate that
stops a third source getting that far.

### It shipped alongside the online path, and then replaced it

`pd-szh2` shipped this beside the online read, with an executable proof:

```bash
pkdump-lake-value-snapshots --date <date> --data-dir /data --compare
```

Both computations, every registered tenant, rows diffed, nothing written; exit
4 naming the tenant and the dimension if any pair disagreed. On the gate's
fixture it came back **2 tenants compared, 7 rows, 0 differing** — and the same
gate showed it going red, exit 4, when a collection changed without being
shipped.

`pd-i08u` is the change that acted on that result. **The online read, the
`--holdings` flag and `--compare` itself are all gone**: with one path there is
no second computation to compare against, and a flag left behind is a flag a
runbook or a timer can still reach for. One valuation, one provenance.

What replaced the comparison as the arithmetic check is stronger for being
about the round trip rather than about two runs of the same SQL:
`tests/lake/phase3.sh` §5 and `tests/lake/value_snapshots.sh` §5 both diff the
zone valuation against the rows Rust `value_history::snapshot_today` computed
over the same collection — so a holding lost, duplicated or wrongly resolved
anywhere between the outbox and `zone_holdings` shows up as a changed number.

### Three refusals, and the last two are the ones that matter

- **No staging table** → that tenant is skipped, naming `pkdump-ship
  holdings`. Obvious.
- **A staging table behind the zone** → skipped too. `zone_holdings_run.
  max_seq` against `ownership_outbox_cursor.shipped_thru` is what detects it:
  the reader stopped short of what the shipper has put in the zone, so the
  materialisation predates the last ship. Left to run, it would value today's
  collection at last week's holdings and every number would look reasonable.
  That is the quiet failure this whole item could otherwise become.
- **Staging tables from two different reads** → skipped too (`pd-bbv7`).
  `zone_holdings_run.sealed_rows` against `COUNT(*)` in
  `zone_sealed_holdings`: any build that materialises both writes the two in
  ONE transaction, so a disagreement means they were not written together. It
  is a **rollback's** fingerprint — an older `pkdump-ship holdings` rebuilds
  `zone_holdings`, leaves `zone_sealed_holdings` exactly as it found it, and
  rewrites the run row with no sealed count to put in it. What is left is one
  read's cards beside an earlier read's sealed lots, and nothing else in the
  system can see it.

A tenant whose OUTBOX is ahead of the zone is *not* refused, and that stayed
true when the online path went away. It means the shipper skipped that tenant,
and **the shipper is what says so** — the same nightly run names them and
pushes its own PARTIAL warning. Refusing here would report one shipping
failure twice, and would withhold a valuation of holdings that are genuinely
what the offline side was told. What the transform refuses is the case nothing
else can see: a read-back that never happened, one that happened too early, or
one that happened in two halves.

### What the zone does not carry

Both holdings tables are shipped (`pkdump_db::outbox::SOURCE_TABLES`). The
condition multiplier (`conditions`) and the third arm of the price rule
(`manual_prices` over `user_printings`) are read from the tenant's own
database. They are not holdings and the outbox does not emit them. Phase 3
narrowed which table the copies come from and nothing else.

The sealed half needs neither: nothing prices a sealed lot off its condition
(`/sealed` does not, so the chart must not), and `manual_prices` is keyed by
`printing_id`, which a sealed lot has none of. Its price rule is one arm —
`COALESCE(market_price, mid_price)` off the newest observation, spelled once in
`pkdump_db::prices::sealed_market_price_expr_from!` and spent by the `/sealed`
page and both value-history paths.

**Deleting the online holdings read did not remove the tenant-database
dependency**, and nothing should be written as though it had. The valuation
still opens each tenant's SQLite — to read `conditions`, to read
`manual_prices`, to read `zone_holdings` itself, and to write
`collection_value_snapshot` back. What moved offline is where the *copies*
come from.

### Gates

`cargo test -p pkdump-ship` covers the round trip hermetically: the zone read
back materialises the collection row for row across several parts and two
partitions, a change that has NOT shipped is absent from the staged holdings
(and shipping is what reconciles them), a tenant's key opens only their own
partition, a tenant with nothing in the zone stages an empty table rather than
none, and the staged tables stay out of the JSON envelope.

`tests/lake/phase3.sh` is the container tier and the acceptance bar: both
images, a real MinIO, a real Nessie, `catalog.prices` built from landed bytes,
the real backfill, the real shipper, the real read-back. §5 diffs the result
against Rust's own rows for the same collection, and asserts that neither
`--holdings` nor `--compare` is still reachable. Its **§6 is the section that
matters** — a collection changed without shipping must leave the valuation
UNMOVED, and §6b requires shipping and reading back to move it. A Phase 3 that
quietly read the live table fails the first half; one that is frozen or cached
fails the second; only a real zone read passes both.

`tests/lake/value_snapshots.sh` is the transform tier's own gate and now
stands the same chain up (§4b), because the transform cannot run without a
materialised zone at all. Its §5 is the arithmetic claim pd-ruwh has always
made — the Python transform's numbers are the Rust implementation's numbers —
and the zone round trip is now inside it rather than beside it.

**It is on the timer** (`pd-i08u`). `deploy/ship.sh` runs both halves, so
`pkdump-ship@<instance>` ships *and* reads back, and the chain is

    land → derive → prices → **ship (+ read back)** → transform

with `pkdump-value-snapshots@` ordered `After=pkdump-ship@` and its calendar
entry derived from that unit's own bounds. The two units swapped places: the
shipment used to run last, after the transform, on the narrower ground that
the two must not write one SQLite file at once. That is still true and still
guaranteed — but it is now a data dependency, and it points the other way.

---

## 8. Deleting a tenant out of it

```bash
bash deploy/erase.sh prod delete --tenant alice --yes --reason "account closed"
```

The prefix in §1 is the deletion unit, and `pkdump-erase` (`pd-qbrf`) is what
drops it — after recording a tombstone against the tenant's key, and before
attempting every path by which their holdings or valuations could still be
read and requiring every one to fail. **The bar is proven, not asserted**, and
the same command run one moment earlier reports every one of those paths open.

The runbook, including what survives a drop on a versioned bucket and what to
do about each way the proof can come back incomplete, is
[`DELETION.md`](DELETION.md).

---

## 9. What is deliberately not here yet

- **valuations as a zone dataset**: `Dataset::Valuations` exists and nothing
  writes it. Phase 3 computes the numbers and writes them where the app reads
  them (`collection_value_snapshot`); putting them back in the zone under the
  tenant's key would mean sealing from Python, which is the second cipher
  implementation `pd-szh2` refused to create. It wants a Rust publisher, and
  it belongs with Phase 4 (publish back).

See `pd-8lw7` for the epic.
