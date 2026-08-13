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
tenant/database_id=<id>/dataset=<holdings|valuations>/as_of=YYYY-MM-DD/part-NNNN.parquet
```

Built by `pkdump_lake::tenant` — `tenant_prefix`, `partition_prefix`,
`part_key` — which is the one place these strings are constructed.

**`database_id` is the first partition, above `dataset=`.** That is what
makes deleting a tenant one prefix drop covering their holdings *and* their
valuations. Derived artifacts inherit the deletion obligation, so they must
not need a second sweep to find; a layout that put `dataset=` on top would
mean every future dataset is another thing a deletion has to remember.
`tenant_prefix(id)` is that prefix and it is the unit item 8 drops.

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
*mechanical*.

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
# Apply the 90-day retention, then verify what actually landed
bash deploy/setup-tenant-zone.sh --apply

# Verify only — safe to run any time, exits non-zero if the rule is wrong
bash deploy/setup-tenant-zone.sh --check

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
| §2 | the retention check goes **red** three ways: whole-bucket rule, 365 days, a second rule reaching `raw/` — then green again |
| §3/§4 | the two identities carry the rendered documents, and each **can** reach its own zone (so a denial below is a denial, not an absence) |
| §5 | **the boundary, both directions** — catalog cannot read/list/write `tenant/`; tenant cannot read/list/write `raw/` or `lake/` |
| §6 | **the same assertion functions go red** when a credential is replaced by a whole-bucket grant — then green again |
| §6b | a whole-bucket grant added *beside* either policy still cannot cross: the Deny statements work |
| §7 | the zone is **empty** of tenant data, and the catalog is untouched |
| §8 | the prefixes and the window have not drifted across their three homes |

§6 is the point of the file. A boundary check that has only ever been seen
passing is not known to check anything, so it runs the *identical functions*
§5 ran against a deliberately broken configuration.

§8 exists because the prefixes and the retention live in three places that
**cannot** share code — Rust (the shipper reads them at runtime), the policy
documents (AWS reads them) and the bash script. A prefix changed in one and
not the others does not fail loudly; it silently widens a policy.

---

## 6. What is deliberately not here yet

This item builds the **governance**, not the data flow. The zone is empty and
meant to stay that way until the shipper exists — §7 asserts it.

- the **outbox** and its writer — item 1 (`pd-5m54`)
- **key custody**: master key, HKDF-derived per-tenant keys, tombstones, and
  backup vs. destruction as deliberately different paths — item 3 (`pd-ulds`).
  This item defines *where* data lives and *who* can reach it; item 3 defines
  *how* it is encrypted
- the **shipper**: outbox → zone, resumable, gap-detecting — item 4
- **deletion end to end**: drop the partition, destroy the key, verify
  unreadable — item 8

See `pd-8lw7` for the epic.
