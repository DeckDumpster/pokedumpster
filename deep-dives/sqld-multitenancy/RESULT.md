# Result — ATTACH-across-namespaces in self-hosted `sqld`

> ## ⚠️ This path was NOT taken
>
> The libSQL/`sqld` + per-tenant-namespaces direction validated below was
> **rejected**. The direction actually chosen is **file-per-tenant local SQLite
> + Litestream multi-DB replication** — epic **pd-gckl** — which keeps the
> current rusqlite stack and its connection-scoped `ATTACH` exactly as it is,
> so the `cat.`-qualification sweep and async ripple costed below never has to
> exist.
>
> These findings are kept because they are *why* that decision could be made.
> Read them as evidence, not as a plan.
>
> **One statement below has been corrected.** The TEMP-VIEW follow-up
> (spike 2) recommended attaching the catalog **once at connection open**.
> Follow-up #2 — the Rust `libsql` client spike (spike 3) — **disproved that**:
> attach must live inside each `transaction()`. Spike 2's recommendation was
> written before spike 3 ran. Both are left standing, with the contradiction
> marked in place; see "Recommended pattern for a libSQL/`sqld` port".

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
| ATTACH is **connection-scoped** (sticks after COMMIT; attach once per conn) | ✅ YES **over raw Hrana only** — ❌ via the Rust `libsql` client (spike 3; see the correction below) |
| `CREATE TEMP VIEW` supported | ❌ NO — *"unsupported statement"* (TEMP objects can't be replicated) |
| `CREATE TEMPORARY VIEW` supported | ❌ NO — same |
| `CREATE VIEW` (permanent) referencing `cat.*` | ❌ NO — SQLite: *"view cannot reference objects in database cat"* |
| Qualified `cat.cards` reference (no view layer) | ✅ YES |

## What this means

**The TEMP-VIEW indirection does NOT port** — sqld rejects TEMP/TEMPORARY views
(they're non-replicable), and core SQLite forbids a *permanent* view from
referencing an attached database. So there is no view layer that lets catalog
tables be referenced unqualified.

**And at this layer the foundation looked solid:** ATTACH measured as
*connection-scoped* — attach the catalog **once** at connection open
(`BEGIN; ATTACH "catalog" AS cat; COMMIT`) and every subsequent query on that
connection saw `cat.*`, even outside a transaction.

> ### ⚠️ Correction — the paragraph above does not hold for the client we'd use
>
> **This spike (spike 2) measured the raw Hrana pipeline**, where a held baton
> *is* the connection, so attach-at-open persisted. **Follow-up #2 below (spike
> 3) re-ran the same question through the Rust `libsql` client** — the client
> `crates/pkdump-db` would actually use — and found its mode A (ATTACH at
> connection open, query in a separate later call) fails with `no such table:
> cat.cards`. The remote `Connection` does not pin one Hrana stream across
> top-level calls, so connection-scoped ATTACH does not survive the client
> abstraction.
>
> **Spike 2 measured the transport; spike 3 measured the client. The client
> governs.** Spike 2's attach-at-open recommendation was written before spike 3
> ran; it is left visible here rather than edited away, because the two results
> are both real and the difference between them is the finding.

## Recommended pattern for a libSQL/`sqld` port *(revised after follow-up #2)*

1. ~~**At connection open:** issue `BEGIN; ATTACH "catalog" AS cat; COMMIT`
   once.~~ **Superseded by spike 3 — attach-at-open does not persist.**
   Instead: **inside every transaction that reads the catalog**, open a
   `transaction()` and issue `ATTACH "catalog" AS cat` as its first statement
   (replaces today's "create TEMP VIEWs at open" step).
2. **In queries:** reference catalog tables **qualified** as `cat.<table>`
   (replaces the unqualified TEMP-VIEW names). *Unaffected by the correction —
   spike 3 confirms this half.*

## Migration-cost note (revises the earlier estimate)

This is a **modest, real refactor** of `crates/pkdump-db`, not a verbatim port:
the TEMP-VIEW setup becomes an ATTACH step, and catalog-table references in the
binder-page query and friends must be `cat.`-qualified. Bounded and mechanical,
but it touches every query that joins the catalog — factor it into the libSQL
decision.

> **Also revised by follow-up #2.** This estimate assumed the ATTACH was a
> one-line addition at connection open. With attach-per-transaction it becomes
> a "with catalog attached" transaction wrapper that every catalog-reading path
> must route through — a larger, structural change to the connection/query
> layer, not a one-liner. See follow-up #2's own migration-cost section.

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

**This is the spike that disproves the TEMP-VIEW spike's recommendation.**
Spike 2 (the TEMP-VIEW follow-up above) concluded "attach once at connection
open"; measured over the raw Hrana pipeline that was true. This spike (spike 3)
measured the same thing through the client PokeDumpster would actually use and
got the opposite answer. **Where they disagree, this one is the operative
result** — it is the layer the app runs at. A correction note is left in place
in spike 2's section rather than rewriting it, so the discrepancy stays legible.

## Pattern (supersedes spike 2's "attach once at open")

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

---

# Follow-up #3: bottomless S3 backup/restore (`run-bottomless.sh`) — pokedumpster-13w

Validates the backup-simplification payoff against a fully local MinIO (no
external creds; nothing leaves the box). Flow: replicate → destroy sqld's local
volume → restart → prove auto-restore.

## Verdict: ✅ PASS

Seeded 5 rows; gracefully stopped sqld; **destroyed its data volume** (total
local loss); started a fresh sqld with an empty volume against the SAME bucket;
all 5 rows returned automatically on startup. Bucket after shutdown:

```
ns-:default-<uuid>/.meta
ns-:default-<uuid>/000000000001-000000000004-<ts>.zstd   (snapshot + WAL, zstd)
```

## How it works

Restore triggers on startup when the local main DB file is empty → bottomless
pulls the newest generation from the bucket. A wiped/replaced disk self-heals on
boot. Config: `--enable-bottomless-replication` plus `LIBSQL_BOTTOMLESS_ENDPOINT`,
`LIBSQL_BOTTOMLESS_BUCKET`, and — **this build requires the prefixed forms** —
`LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID` / `_SECRET_ACCESS_KEY` / `_DEFAULT_REGION`
(generic `AWS_*` alone errors: "LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION was not set").

## Caveats / risks (weigh before relying on it)

- **Durability window.** The bucket was still EMPTY 8s after the writes — the
  generation only appeared on graceful shutdown. bottomless batches; a HARD
  crash between flushes can lose the most recent writes. Fine for a hobby
  collection (a coarse RPO is acceptable), but this is lagged replication, not
  synchronous durability. Measure/tune the flush cadence before trusting it as
  the sole backup; keep nightly `sqlite .backup` snapshots as belt-and-suspenders.
- **Namespaces untested.** Ran in SINGLE-DB mode for a clean proof. The
  generation key `ns-:default-...` is namespace-shaped (encouraging), but
  per-namespace bottomless WITH `--enable-namespaces` (the real multitenant
  config) is NOT yet validated — the top remaining risk for this path.
- **Longevity.** bottomless is the OLD replication path; libsql is moving to
  libsql_wal (`--migrate-bottomless` flag exists). Future uncertain.

## Reproduce

```bash
spikes/sqld-attach-namespaces/run-bottomless.sh          # replicate / lose / restore
KEEP=1 spikes/sqld-attach-namespaces/run-bottomless.sh   # leave MinIO + sqld up
```
Auto-pulls the `minio` + `minio/mc` images.

---

# Follow-up #4: per-namespace JWT auth + per-namespace bottomless (`run-jwt-backup.sh`)

The final spike (decision pokedumpster-181): closes the auth front-door AND the
per-namespace-backup risk in one fully-local run (MinIO).

## PART 1 — Auth scoping: ✅ PASS

Per-namespace `jwt_key` (set via the admin config endpoint) enforces auth
**independently per namespace**. Matrix (Ed25519 tokens, claim `{"a":"rw"}`):

| request | result |
|---|---|
| no token → tenantA | DENY ✓ |
| tokenA → tenantA | ALLOW ✓ |
| tokenA → tenantB | **DENY ✓ (scoped)** |
| tokenB → tenantB | ALLOW ✓ |

Model confirmed: a namespace is **OPEN until its `jwt_key` is set** (no global
`--auth-jwt-key-file` needed); each tenant gets its own key; a token only works
on its own namespace. ⇒ set `jwt_key` on EVERY tenant namespace or it's
unprotected. Token sent as `Authorization: Bearer <jwt>`; key configured as
URL-safe-base64 of the raw Ed25519 public key.

## PART 2 — per-namespace backup/restore: ✅ PASS, with a DR caveat

- bottomless + namespaces **requires `LIBSQL_BOTTOMLESS_DATABASE_ID`** (startup
  errors "bottomless replication with namespaces requires a DB ID" without it;
  single-DB mode auto-generated one).
- Each namespace replicates to its OWN prefix:
  `ns-<dbid>:tenantA-<uuid>/…`, `ns-<dbid>:tenantB-<uuid>/…`. ✓
- After destroying the volume + restart: **the namespace REGISTRY (meta-store)
  is NOT restored by bottomless** — both namespaces were absent. Re-creating them
  via the admin API then triggered restore-on-open of each namespace's data from
  its generation (A=2 rows, B=3 rows, matched by namespace NAME).

### DR-runbook implication (important)

bottomless backs up per-namespace DATA but NOT the namespace list or their
configs (`jwt_key`, `allow_attach`). Disaster recovery =
(1) start sqld with the SAME `LIBSQL_BOTTOMLESS_DATABASE_ID` + bucket,
(2) re-declare every namespace and re-apply its config,
(3) data restores on first open.
So the app/deploy must own the source of truth for "which namespaces exist and
their config." For PokeDumpster that's just the user list + the catalog's
`allow_attach` — fully reconstructable — but it's a real operational requirement,
and extra reason to keep nightly `sqlite .backup` snapshots as belt-and-suspenders.

## Reproduce

```bash
spikes/sqld-attach-namespaces/run-jwt-backup.sh
KEEP=1 spikes/sqld-attach-namespaces/run-jwt-backup.sh
```
Needs PyJWT + cryptography (host) and the `minio`/`minio/mc` images (auto-pulled).

---

# Real-S3 validation (AWS S3, us-west-2) — `run-bottomless-s3.sh`

The MinIO proof (#3) re-run against **actual AWS S3**. ✅ PASS: replicated to the
real bucket, destroyed the local volume, restarted, all 5 rows auto-restored.
Generation layout identical to MinIO: `ns-<dbid>:default-<uuid>/{.meta,<wal>.zstd}`.

## Credentials: assumed-role temp creds WORK

Authenticated via **STS assume-role** (a user that can only `sts:AssumeRole` →
`role/pokedump-data` which holds the S3 policy). bottomless **honors the session
token** when passed as `AWS_SESSION_TOKEN` (+ `LIBSQL_BOTTOMLESS_AWS_SESSION_TOKEN`)
alongside the prefixed key/secret/region. **Long-lived IAM user keys are not
required** — good, since static user keys are discouraged.

## Production credential caveat (resolve during hardening)

STS temp creds **expire** (assume-role default 1h) and bottomless takes STATIC env
creds with no auto-refresh — fine for this <1 min spike, NOT for a long-running
server. For prod, pick one:
- **IAM Roles Anywhere** — keyless for off-EC2/on-prem (X.509 trust anchor); the
  modern "no static keys" answer and the best fit for the self-hosted box.
- A **refresh sidecar/timer** that re-assumes the role and rotates sqld's creds
  before expiry.
- A **narrowly-scoped static key** (S3 on the one bucket only) — simplest; accepts
  a long-lived but low-blast-radius key.

Least-privilege policy used (attached to the role): `s3:ListBucket` +
`s3:GetBucketLocation` on the bucket ARN, `s3:GetObject/PutObject/DeleteObject`
on the `/*` ARN. (`GetBucketLocation` is required — bottomless calls it on startup.)

## Reproduce

```bash
CLEANUP=1 spikes/sqld-attach-namespaces/run-bottomless-s3.sh   # creds from ~/.pkdump-s3-spike.env
```

---

# Credential strategy validation: bottomless behind aws-sigv4-proxy — `run-sigv4-proxy.sh` (pokedumpster-8ch.9)

Validates the chosen credential strategy (decision pokedumpster-8ch.1): an egress
**signing sidecar** so sqld never holds real AWS credentials. ✅ PASS against real S3.

## Architecture proven

```
sqld/bottomless ──path-style S3, DUMMY creds──▶ aws-sigv4-proxy ──assumes role/pokedump-data,
  (LIBSQL_BOTTOMLESS_ENDPOINT=http://proxy:8080)   (--role-arn, auto-refreshing temp creds)
                                                   re-signs SigV4──▶ real S3 (us-west-2)
```
Replicate → destroy local volume → restart → all 5 rows restored, **with sqld holding only
`dummy`/`dummy` credentials.** The proxy owns the real, rotating creds; sqld needs no restart
and no AWS key.

## The gotcha that cost the spike (save the next person)

Initial run: sqld failed at startup with `Bucket checking error: service error`; the proxy log
showed bottomless's `HEAD /bucket/` returning **403** from S3 — even though a plain `curl` HEAD
through the proxy returned 200. Root cause, isolated by header bisection:

- **`X-Amz-User-Agent` must be stripped at the proxy.** The Rust SDK (`aws-sdk-rust`) adds it to
  every request; it is an `x-amz-*` header, so S3 requires it be part of the signature; the proxy
  forwards it **unsigned** → `SignatureDoesNotMatch` (403). Stripping it fixed everything.

Working proxy invocation:
```
aws-sigv4-proxy --name s3 --region us-west-2 --host s3.us-west-2.amazonaws.com \
  --role-arn arn:aws:iam::ACCT:role/pokedump-data --unsigned-payload \
  -s Authorization -s X-Amz-Date -s X-Amz-Content-Sha256 -s X-Amz-User-Agent
```
(The first three strips remove bottomless's *dummy* SigV4 headers so the proxy signs clean;
`--unsigned-payload` avoids hashing streamed PUT bodies.) sqld gets `LIBSQL_BOTTOMLESS_ENDPOINT`
pointed at the proxy and throwaway `LIBSQL_BOTTOMLESS_AWS_*` creds.

## What this confirms / still open

- ✅ The assume-role + sidecar strategy works: no standing AWS key in sqld; the proxy holds and
  auto-refreshes the assumed-role creds via the SDK chain.
- ⚠️ **Not yet observed:** an actual credential *refresh* across the ~1h STS expiry (the spike runs
  in <1 min). The proxy uses the auto-refreshing SDK provider so it *should* roll over without sqld
  restart — confirm in the real deployment (a long-running soak) before fully trusting it.

## Reproduce

```bash
CLEANUP runs automatically; KEEP=1 to leave the proxy + sqld up:
spikes/sqld-attach-namespaces/run-sigv4-proxy.sh
```
Needs `~/.pkdump-s3-spike.env` with `S3_ROLE_ARN` set; auto-pulls the `aws-sigv4-proxy` + `aws-cli` images.

---

# GATE spike (8ch.2.1): is there a lighter backup-first path than the sqld migration? — `run-litestream.sh`

Question: do we actually need the libsql/sqld migration (8ch.2: async ripple +
`cat.`-qualify sweep across 17 modules) to get backup/restore for the
**single-user** phase? Two findings:

## Finding 1 — the literal "embedded libsql + bottomless VFS" path is DEAD

The standalone `bottomless` crate is stuck at **0.1.1**; bottomless is now
maintained only *inside* sqld (the server replicator we spiked). There is no
maintained embedded-VFS build of it. So option (A) as written is not viable.

## Finding 2 — Litestream gives backup-first with ZERO DB-layer change ✅ PASS

`run-litestream.sh`: a **plain SQLite DB** (the current rusqlite stack, untouched)
→ **Litestream v0.5.11** → **real AWS S3** (us-west-2) → simulate total loss →
restore → all 5 rows recovered. Litestream is a sidecar that watches the SQLite
file's WAL; it needs **no libsql, no sqld, no ATTACH/TEMP-VIEW changes, no async**.

- Uploaded one `.ltx` (LTX-format) file; restore reconstructed the DB exactly.
- **Assume-role temp creds (session token) honored directly** — no signing
  sidecar needed (Litestream uses the AWS SDK and reads `AWS_SESSION_TOKEN`).
- Gotcha: **pin `region` in the Litestream config** — otherwise it calls
  `s3:GetBucketLocation` (which the role doesn't grant) and 403s. Pinning region
  skips that lookup and keeps the IAM policy tight (no GetBucketLocation needed).

## Conclusion / gate resolution

For **phase-1 backup-first (single user)**: keep the entire current
rusqlite/SQLite/ATTACH/TEMP-VIEW stack **unchanged** and add **Litestream** to
continuously replicate `collection.sqlite` → S3 (the shared catalog is
reproducible and not backed up). This **eliminates 8ch.2** (no libsql/sqld, no
sweep, no async) and **reshapes 8ch.3/8ch.4** (a Litestream sidecar/timer
instead of a sqld server + bottomless) for phase-1.

The sqld + namespaces + bottomless + sigv4-proxy apparatus (all validated above)
is **not wasted** — it is the **multitenancy** substrate (Litestream is per-file,
single-DB; it does not do per-tenant namespaces). Both paths are now proven; each
has its phase: **Litestream for single-user backup now; sqld/bottomless for the
multitenant future.**

## Reproduce

```bash
spikes/sqld-attach-namespaces/run-litestream.sh   # auto-cleans the S3 prefix
```
Needs `~/.pkdump-s3-spike.env` (with `S3_ROLE_ARN`) and host `sqlite3`; auto-pulls
the `litestream` + `aws-cli` images.

---

# Deploy validation (8ch.7): Litestream sidecar + assume-role creds — `run-litestream-deploy.sh`

Validates the actual deploy artifacts (`deploy/litestream.yml` + the Quadlet unit's
invocation) and the **project credential standard**: assume-role via `~/.aws/config`
(SDK auto-refresh), **never static keys** (user preference; see bd memory
`aws-s3-credential-strategy-...`). ✅ PASS against real AWS S3.

## What it proved

The fixture collection DB (26 rows) replicated to S3 and restored **26/26 rows**
after total deletion — with Litestream invoked exactly as the Quadlet sidecar
would (same image, `replicate -config /etc/litestream.yml`, same mounts/env), and
credentials supplied ONLY as an assume-role profile:

```
~/.aws/config:       [profile pkdump] role_arn=...:role/pokedump-data
                     source_profile=bootstrap  region=us-west-2
~/.aws/credentials:  [bootstrap] <key that may only sts:AssumeRole>
```
Litestream's AWS SDK assumed the role itself (log showed `snapshot complete` +
`ltx file uploaded`, zero credential errors). No `AWS_SESSION_TOKEN`, no static
S3 key — the SDK refreshes the assumed-role creds on its own (1h soak skipped at
this single-user phase, per decision).

## Refinement applied to 8ch.3 (found during validation)

The sidecar unit had `WantedBy=default.target` → it would crash-loop on a fresh,
unconfigured setup. Fixed: `ConditionPathExists=%h/.config/pkdump/<inst>/aws/credentials`
so it **auto-starts on boot only once the operator has provided the bootstrap
credentials**; `setup.sh` no longer writes a `credentials` template (so the gate
is meaningful) — it documents the file the operator must create.

## Residual (not exercised live)

The validation invoked the litestream container directly (mirroring the unit), not
through a full `setup.sh --test` systemd instance — the unit install is mechanical
(sed + Quadlet) and was structurally validated (`bash -n` + sed preview). A full
systemd-instance smoke test (build image, `systemctl --user start` the generated
service) remains optional follow-up; the backup/restore + credential model — the
load-bearing parts — are proven.

## Reproduce

```bash
spikes/sqld-attach-namespaces/run-litestream-deploy.sh   # auto-cleans the S3 prefix
```
