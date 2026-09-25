# What replaces MinIO as pokedumpster's local S3 fake

> **Verdict: ADOPT PGSTY SILO — the maintained AGPL fork of the MinIO server —
> and `pgsty/mc` beside it. It is a byte-for-byte drop-in: the same
> `server /data` invocation, the same `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD`,
> the same `/minio/health/live`, the same `mc admin` IAM, the same root uid.
> The migration is the two image constants in `tests/lib/minio.sh`, plus the
> one regex in `tests/lib/objects_test.sh` that guards them, and nothing else.
> Measured: 17/17 requirements, and `tests/lake/run.sh`,
> `tests/litestream/run.sh` and — the hardest gate in the suite —
> `tests/lake/tenant_zone.sh` all green with no harness edit.**
>
> Spike bead `db-hny6`. Written 2026-09-25. Proof-of-concept branch
> `spike/db-hny6-poc` (this worktree) — `spike-poc/` holds `probe.sh`,
> `memprobe.sh`, the Rust SDK probe and every raw run log. **Unmerged.**
> Sources preserved verbatim under [`sources/`](sources/).
>
> The runner-up is **RustFS** (Apache-2.0, 33.9k stars, 1.0 GA nine days ago),
> which passed the identical 17 requirements but is *not* a drop-in: it runs as
> a non-root uid, which breaks four harnesses that do not `chmod 777` their data
> directory. Measured, not predicted — `tests/litestream/run.sh` died with
> `[FATAL] Server runtime failed: Io error: Permission denied (os error 13)`.

**Where this lives.** `docs/spikes/<topic>/RESULT.md`, matching
`docs/spikes/logout-across-deployments/`. The landing gate for a spike branch
refuses a commit touching anything outside `docs/spikes`.

---

## 1. The question

`tests/lib/minio.sh` names two container images that thirteen files in this
repository depend on. On 2026-09-15 MinIO withdrew its public images from
Docker Hub, and from quay.io by 2026-09-25; `github.com/minio/minio` is
archived — *"This repository was archived by the owner on Apr 25, 2026. It is
now read-only. … THIS REPOSITORY IS NO LONGER MAINTAINED."*
([`sources/minio-github-archived.txt`](sources/minio-github-archived.txt)).

CI is not broken: db-n3a6 mirrored the pinned tags to
`ghcr.io/deckdumpster/{minio,mc}` and they serve anonymously today. But that
mirror is the last copy of software nobody will ever patch.

**What should stand in for S3 in pokedumpster's test suite?** An answer is a
costed recommendation naming one thing, with the requirement list it was chosen
against and what adopting it costs. Real AWS S3 was considered and rejected by
the operator (db-8wy4/db-ht2v/db-m0h1, closed 2026-09-25) and is not re-proposed
here; it appears in §5 as the baseline that was turned down.

---

## 2. What the suite actually needs

This is half the answer, and it was established from the code before any
candidate was looked at.

### 2.1 The thirteen files

`git grep -lE 'MINIO_IMAGE|PKDUMP_MINIO' -- tests` returns 13. They are not 13
harnesses:

| file | what it is | needs a store? |
|---|---|---|
| `tests/lib/minio.sh` | the one definition of the two image names | — (it *is* the constant) |
| `tests/lib/objects_test.sh` | lint tier; greps the **tree** for a hardcoded image | **no** |
| `tests/lake/run.sh` | Iceberg + Nessie round trip, time travel | yes |
| `tests/lake/prices.sh` | the nightly price build into Iceberg | yes |
| `tests/lake/phase3.sh` | valuing a collection from the tenant zone | yes |
| `tests/lake/value_snapshots.sh` | the transform tier, every tenant | yes |
| `tests/lake/shipper.sh` | the outbox → tenant zone, under the real IAM policy | yes |
| `tests/lake/tenant_zone.sh` | the credential boundary + 90-day retention | yes |
| `tests/lake/deletion.sh` | deletion proof on a **versioned** bucket | yes |
| `tests/litestream/run.sh` | the shipped sidecar config, every tenant | yes |
| `tests/litestream/drill.sh` | the restore drill | yes |
| `tests/litestream/recreate.sh` | replica survives a recreate | yes |
| `tests/alarming/run.sh` | backup-check correspondence + paging | yes |

So **11 container gates stand a server up**, one lint-tier test needs nothing,
and one file is the constant itself.

**Every one of the 11 launches the server identically** —
`grep -rh 'MINIO_IMAGE" server /data' tests/ | wc -l` returns 11, one per gate:

```bash
podman run -d --name "$MINIO_CTR" --network "$NET" \
    -p "127.0.0.1:$PORT:9000" \
    -e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET" \
    -v "$WORK/minio:/data:Z" \
    "$MINIO_IMAGE" server /data
```

That uniformity is what makes a drop-in replacement a one-line change and
anything else an eleven-file change. Two further details that decide candidates:

* **8 of the 11 health-check `http://…/minio/health/live`** (`alarming`,
  `lake/{run,shipper,tenant_zone,deletion}`, `litestream/{run,drill,recreate}`).
* **7 of the 11 `chmod 777` the mounted data dir; 4 do not** — `alarming/run.sh`,
  `litestream/{run,drill,recreate}.sh`. A server image that runs as a non-root
  uid fails on those four. Observed on one of them (§4.3), deduced for the
  other three, which create the directory identically.

### 2.2 The two clients, and which is stricter

* **The Rust AWS SDK** (`aws-sdk-s3 1.141` per `Cargo.lock`), through
  `crates/pkdump-lake/src/store.rs`, with `PKDUMP_LAKE_S3_ENDPOINT` and
  `force_path_style(true)` when an endpoint is set. The operations it actually
  performs are five, and only five: `put`, `get`, `list_keys` (paginated
  `ListObjectsV2`, no delimiter), `child_dirs` (`ListObjectsV2` with
  `delimiter=/`, reading `CommonPrefixes`) and `delete`. The traits in
  `store.rs` are deliberately narrow — `ObjectStore` has no read, `ObjectSource`
  no write, `ObjectPurge` neither — which is why the S3 surface this app needs
  is so much smaller than the surface its *test policies* need.
* **Litestream 0.5.17** (`deploy/litestream-lib.sh`), in `dir:` mode with
  `watch: true`, pinned region, explicit `endpoint`. **This is the stricter
  client and the more brittle contract**, because `deploy/backup-check.sh`
  parses one specific log line out of it — `msg="replica sync" db=… replica=s3
  txid.replica=… txid.db=…` — and pd-pfxf is the scar from that line moving.
  A fake that satisfies the Rust SDK but not Litestream fails 4 of the 11 gates.
* **`mc`** (`minio/mc`), in **9** of the 11, and **`aws-cli`** (unpinned
  `docker.io/amazon/aws-cli:latest`) in 3 — `lake/tenant_zone.sh`,
  `litestream/{drill,recreate}.sh`.

### 2.3 Does anything depend on **mc-specific** behaviour?

Yes, and it is the single hardest requirement in the list. `mc` is used 26
times as **`mc admin`** across three gates:

```
tests/lake/tenant_zone.sh   admin user add / policy create / policy attach / policy detach
tests/lake/shipper.sh       admin user add / policy create / policy attach (+ swap in §6)
tests/lake/deletion.sh      admin user add / policy create / policy attach
```

`mc admin` is the **MinIO admin API**, which the AWS CLI has no equivalent for —
there is no `aws iam` against a bucket-only server. The other 47 `mc` calls
(`ls` ×24, `mb` ×9, `mirror` ×4, `rm` ×3, `pipe` ×3, `cat` ×3, `version` ×1)
are ordinary S3 and the AWS CLI could cover them. **So a candidate without the
MinIO admin API does not cost "an mc equivalent"; it costs rewriting the
identity setup of the three gates whose whole subject is identity.**

What those three gates install is not a toy policy. It is
`deploy/policies/tenant-zone/{catalog,tenant}-credentials.json`, **rendered by
the real `deploy/setup-tenant-zone.sh --render`** so that, in the gate's own
words, *"this gate cannot pass against a policy nobody deploys"*. Those
documents need a server that evaluates:

* resource ARNs at object-prefix granularity (`arn:aws:s3:::B/tenant/*`);
* **explicit `Deny` beating `Allow`** — four `Deny` statements, and
  `tests/lake/tenant_zone.sh` §6b re-runs the probes against a credential given
  a whole-bucket grant *beside* the policy and requires the Deny to still win;
* **`Condition` → `StringLike` → `s3:prefix`** on `s3:ListBucket`, in both
  directions: the tenant must be refused `raw/` and still able to list
  `tenant/`.

Nothing in the fake landscape except MinIO and its forks does all three.

### 2.4 Which S3 features are genuinely exercised

| feature | exercised? | the harness that proves it |
|---|---|---|
| bucket create / put / get / list / delete | yes | all 11 |
| delimiter listing (`CommonPrefixes`) | yes | `pkdump-lake` `ObjectSource::child_dirs`, used by the derive |
| **IAM users + policy documents** | yes | `tenant_zone.sh` §3, `shipper.sh` §0, `deletion.sh` |
| **explicit `Deny` over `Allow`** | yes | `tenant_zone.sh` §6b (seen red), `shipper.sh` §6 |
| **`s3:prefix` `Condition` on ListBucket** | yes | `tenant_zone.sh` §4–§6 |
| **bucket versioning + noncurrent version read by `--version-id`** | yes | `deletion.sh` §6 |
| **delete markers** | yes | `deletion.sh` (the drop leaves a marker + noncurrent version) |
| **lifecycle put / get / delete** | yes | `tenant_zone.sh` §2 (via `aws s3api`), incl. the `NoSuchLifecycleConfiguration` path that `setup-tenant-zone.sh` exit 3 depends on |
| Litestream LTX replication + `ltx` listing | yes | `litestream/*`, `alarming/run.sh` |
| listing pagination | **weakly** — every fixture is well under one page | — |
| multipart upload | **no** — `grep multipart` finds nothing in `pkdump-lake`/`pkdump-ship`/`lake/`; every fixture is kilobytes, below the SDK's 8 MiB threshold | — |
| presigned URLs | **no** — `grep -rn presign crates/ lake/ deploy/ tests/` returns nothing outside `.md` | — |
| conditional writes (If-Match / If-None-Match) | **not established** — nothing in this repo asks for them; whatever Litestream 0.5.17 does internally was satisfied by every candidate that passed R8 | — |

### 2.5 How many harnesses need no fake at all?

The bead asks this because "the cheapest fix is to not need a fake". The honest
number is **one, and it already needs none**: `tests/lib/objects_test.sh` is
lint-tier, hermetic, and only greps the tree.

Of the 11 container gates, **zero can drop the store without deleting the
coverage they exist for**:

* `PKDUMP_LAKE_DIR` (`Backend::Dir`) is real and is honoured by `open`,
  `open_reader`, `open_tenant_zone`, `open_tenant_zone_reader` and
  `open_tenant_zone_purge` — so in principle `shipper.sh` and `deletion.sh`
  *could* run directory-backed. But those two gates exist to prove the IAM
  boundary and the encryption over a **real bucket under the real tenant
  policy**; a `DirStore` has no credentials to be denied by. The hermetic Rust
  tiers (`cargo test -p pkdump-ship`, `-p pkdump-erase`) already cover the
  `DirStore` half, and `tests/lake/derive.sh` and
  `tests/refresh/tenant_bytes.sh` already run `PKDUMP_LAKE_DIR` and are
  correctly *not* in the thirteen.
* The four Litestream gates replicate to `type: s3`, which is what production
  runs, through the shipped `deploy/litestream.yml` — the one file every
  sidecar in deploy and in every gate mounts. Pointing them at a non-S3 replica
  would mean a second config, which is precisely the drift that file exists to
  prevent.
* `lake/{run,prices,phase3,value_snapshots}.sh` use an Iceberg warehouse at
  `s3://`; PyIceberg would take a `file://` warehouse, but that deletes the only
  coverage of `PKDUMP_LAKE_S3_ENDPOINT` + `force_path_style` there is.

**So the answer is: no, we cannot avoid needing a fake.**

---

## 3. How the candidates were measured

A feature table is not evidence, so every candidate was run through one script —
`spike-poc/probe.sh` on branch `spike/db-hny6-poc` — that exercises the
requirement list above with the **real clients**: containerised `mc`, the real
`amazon/aws-cli`, real **Litestream 0.5.17** with the shipped
`logging: level: debug` and `dir:`+`watch:` shape, and a **Rust binary built
against `aws-sdk-s3` on the `rust:1.94-slim-bookworm` toolchain the
`Containerfile` names**, calling exactly the five operations
`crates/pkdump-lake/src/store.rs` calls.

Seventeen checks, R1–R9. **The probe was validated against the incumbent
first**: MinIO `RELEASE.2025-09-07T16-13-09Z` from the DeckDumpster mirror
scores 17/17. A probe whose baseline fails is measuring itself; the first two
runs scored 14/16 and both failures were probe bugs (one of them a
`| grep -q` inversion under `pipefail` — the repo's own
`law-no-grep-q-under-pipefail`, met in the wild while writing the tool that
tests for it).

All measurements were taken on the deployment box, which at the time had
**1 CPU and 3958 MiB RAM** with `/tmp` a 2 GiB tmpfs — *not* the 6144 MiB
ephemeral runner. Timings are therefore an upper bound and memory is directly
comparable between candidates but not to a two-core runner.

---

## 4. What was found

### 4.1 The probe matrix

| | MinIO (mirrored) | **Silo** | RustFS | SeaweedFS | Adobe S3Mock | Garage |
|---|---|---|---|---|---|---|
| version measured | RELEASE.2025-09-07 | RELEASE.2026-09-16 | 1.0.0 | 4.47 (`latest`) | `latest` | v2.1.0 |
| licence | AGPL-3.0 (archived) | AGPL-3.0 | Apache-2.0 | Apache-2.0 | Apache-2.0 | AGPL-3.0 |
| R1/R2 mc basics | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ (setup) |
| **R3 IAM user+policy+attach** | ✅ | ✅ | ✅ | ❌ *attach refused* | ❌ none | ❌ none |
| **R4 explicit Deny > Allow** | ✅ | ✅ | ✅ | — | — | — |
| **R5 `s3:prefix` Condition** | ✅ | ✅ | ✅ | — | — | — |
| **R6 versioning + noncurrent read** | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ *not implemented* |
| **R7 lifecycle put/get/del + absent** | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |
| **R8 Litestream 0.5.17 + TXIDs** | ✅ | ✅ | ✅ | ✅ | ✗ n/e | ✗ n/e |
| **R9 Rust `aws-sdk-s3`** (5 ops) | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |
| **score** | **17/17** | **17/17** | **17/17** | 11 pass / 2 fail / 2 skip | 2 / 8 / 3 | 2 / 9 / 2 |
| boot to first HTTP answer (warm) | 2.0–5.4 s | **1.0–2.9 s** | 1.2–2.5 s | 6.4 s | 32.9 s | 1.3–5.5 s |
| memory: idle / peak under 300×64 KiB | 59 MB / 170 MiB | 60 MB / 176 MiB | **244 MB / 260 MiB** | — | — | — |
| image size | 176 MB | 163 MB | 288 MB | 533 MB | 135 MB | **26.6 MB** |
| runs as | root | root | **non-root (`rustfs`)** | root | non-root | non-root |
| drop-in for the 11 launch sites | — | **yes, verbatim** | no (uid) | no | no | no |

Every candidate was probed twice: once as it was pulled, and once more at the
end of the session with every image already local, captured verbatim in
`spike-poc/logs/probe-runs.txt`. **No check changed verdict between the two
runs**; the only differences are the boot times (the first run of each includes
its pull — SeaweedFS measured 39.8 s cold against 6.4 s warm, S3Mock 61.2 s
against 32.9 s, RustFS 20.0 s against 1.2 s) and SeaweedFS's, Garage's and
S3Mock's R9, which was SKIPped in the first round because the Rust probe binary
was still compiling. The boot ranges above are the two measurements; on a
single-core box shared with the rest of this session they are noisy, and only
the ranking is durable: five native servers in the low single-digit seconds,
then a JVM an order of magnitude behind.

`✗ n/e` = not established: those candidates failed earlier checks so the later
ones could not be reached honestly. For Garage and S3Mock the R8 failure
reported a *local* condition (`db not ready: page size not initialized`) rather
than a store error, so it is recorded as not established rather than as a
Litestream incompatibility.

Garage's R1/R2/R3 failures are partly my harness: Garage has no root credential
and needs keys minted through its own admin API first, which the probe does not
do. Its disqualifying facts are documented rather than probed — see §4.5.

### 4.2 Silo is a literal drop-in, and that is the whole finding

`pgsty/silo` is a fork of `minio/minio` (`forked from minio/minio`,
[`sources/silo-github.txt`](sources/silo-github.txt)), so every interface the
suite touches is the same object:

* same entrypoint shape — `[/usr/bin/docker-entrypoint.sh] | [silo]` against
  MinIO's `[…docker-entrypoint.sh] | [minio]`, and `server /data` is accepted
  verbatim;
* same `MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD`;
* same `/minio/health/live`;
* same `mc admin` API surface — the probe drove Silo with the **mirrored
  MinIO `mc`** and every admin call worked;
* `USER` is empty (root), so the four harnesses that do not `chmod 777` are
  unaffected.

Real harnesses were run against it with **no edit to any harness**, only
`MINIO_IMAGE` (and, where noted, `MC_IMAGE`) in the environment. Against the
MinIO baseline, measured on the same box in the same session:

| gate | MinIO (mirrored) | **Silo** |
|---|---|---|
| `tests/litestream/run.sh` | PASSED, 39.0 s | **61 passed, 0 failed, 38.2 s** |
| `tests/lake/run.sh` | PASSED, 1 m 39.3 s | **PASSED, 1 m 36.8 s** |
| `tests/lake/tenant_zone.sh` | (not re-run) | **PASSED, 1 m 37.7 s** — with `MC_IMAGE=docker.io/pgsty/mc:latest` too |
| `tests/lake/deletion.sh` | — | §0 green on a **versioned** bucket, then died in its own Rust build (§8) |

Three things about that table are the evidence, not the timings:

* The Litestream run includes **§3b**, the section pd-pfxf exists for: the
  running sidecar must emit `msg="replica sync"` carrying both TXIDs for
  `deploy/backup-check.sh` to parse. It does.
* `tests/lake/tenant_zone.sh` is **the hardest gate in the suite** and it passed
  with both halves of the client stack on the fork. Its §3 installs the
  rendered production policy documents verbatim; §5 asserts the boundary in
  both directions; **§6 re-runs those same assertions against a deliberately
  broken credential and requires them to fail**; §6b proves the explicit `Deny`
  statements are load-bearing; §2/§2b drive the whole lifecycle read-back
  including the `CANNOT VERIFY` (exit 4) path against a credential refused by a
  real policy. All 21 `ok`/`red` lines green.
* `tests/lake/run.sh` took **less** time on Silo than on MinIO, on a
  single-core box, with the lake job image layer-cached for both.

The client half is maintained too: **`pgsty/mc`** is the same organisation's
fork of `minio/mc`, released on the same cadence
([`sources/silo-mc-github.txt`](sources/silo-mc-github.txt),
[`sources/silo-mc-releases-api.json`](sources/silo-mc-releases-api.json) —
`RELEASE.2026-09-16`, `2026-09-13`, `2026-09-03`, `2026-09-01`, `2026-08-26`).
`docker.io/pgsty/mc:latest` resolves; `docker.io/minio/mc:latest` and
`quay.io/minio/mc:latest` both 404 (`podman manifest inspect`, 2026-09-25).
**Driving Silo with `pgsty/mc` scores 17/17 as well** — including all four
`mc admin` operations the IAM gates use.

**Maintenance signal** (`sources/silo-repo-api.json`, fetched 2026-09-25):
3,522 stars, 17 open issues, `pushed_at` 2026-09-25 — *today*. Server releases
2026-04-17, 06-18, 08-04, 08-06, 09-03, 09-16: roughly monthly, accelerating.
Every release ships multi-arch images with GPG signatures, SHA-256 checksums,
SBOMs and build provenance ([`sources/silo-pgsty.txt`](sources/silo-pgsty.txt)).

### 4.3 RustFS passes every requirement and is still not a drop-in

RustFS scored 17/17 — identical to MinIO, including the three IAM checks that
kill everything else. `tests/lake/run.sh` passed against it unmodified
(`rc=0`, 2 m 13.9 s — not comparable to the 1 m 36–39 s figures above, because
that run also pulled the image and built the PyIceberg job image the later runs
found in the layer cache), which is a strong result: PyIceberg, Nessie, a catalog
restart and a prefix assertion all over RustFS.

It also answers `/minio/health/live` with 200, accepts `server /data`, and reads
`MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` — it advertises MinIO compatibility and
means it.

**But `tests/litestream/run.sh` failed against it**, and the reason is not
about S3 at all:

```
=== 1. throwaway MinIO ===
!! FAILED  tests/litestream/run.sh:242
!!   command : curl -fsS "http://127.0.0.1:$MINIO_PORT/minio/health/live"
!!   status  : 7
```

with the container's own last line:

```
[FATAL] Server runtime failed: Io error: Permission denied (os error 13)
```

`podman inspect -f '{{.Config.User}}' docker.io/rustfs/rustfs:latest` → `rustfs`.
The image runs as a non-root uid; rootless podman maps it to a subuid that
cannot write a data directory created under a `mktemp -d` (0700) by the
invoking user. The 7 gates that `chmod 777` are fine; the 4 that do not are
not. **Observed on `litestream/run.sh`**; the other three —
`alarming/run.sh:268`, `litestream/drill.sh:494`,
`litestream/recreate.sh:118` — are `mkdir -p "$WORK/minio"` under the same
`mktemp -d` with no `chmod`, so they fail for the same reason, which I did not
separately run.

**Adopting RustFS therefore costs 4 harness edits** (one `chmod 777` line each)
or one `--user 0:0` added to 11 launch sites — small, but it is not zero, and
it is exactly the kind of difference that is invisible until a gate goes red.

Maintenance signal is excellent on paper — 33,898 stars, Apache-2.0,
`pushed_at` 2026-09-25, 51 open issues, and a release every day or two
(`1.0.1-preview.11` 09-24, `.10` 09-22, `.9`/`.8` 09-21, `.7` 09-20 …). The
caution is that **1.0 GA was 2026-09-16, nine days before this was written**,
and the IAM implementation this suite would lean on has a recent history:
`GHSA-xgr5-qc6w-vcg9` / `CVE-2026-22043` is a *deny-only short-circuit
privilege escalation* in the IAM evaluator
([`sources/rustfs-advisory-iam-deny-only.txt`](sources/rustfs-advisory-iam-deny-only.txt)),
affecting `alpha.13`–`alpha.78` and patched before GA. Open issues #3279
(*"default-deny not enforced"*) and #7989 (versioned `GetObject` denied unless
the policy also grants `s3:GetObjectVersion`) are in the same area. For a test
fake, an IAM bug is not a security incident — but it *is* a gate that goes red
or, worse, green for the wrong reason, in the three gates whose entire subject
is a Deny.

### 4.4 SeaweedFS: mature, and the wrong shape

SeaweedFS is the most battle-tested candidate — since 2012, releases weekly
(4.47 on 2026-09-14, 4.46 09-08, 4.45 08-31 …). It passed the basics, lifecycle
and Litestream.

It is also, after the three MinIO-lineage servers, the **only** candidate whose
S3 surface the Rust SDK, the AWS CLI and Litestream were all happy with — R9
green on all five operations, R7 green, R8 green.

It fails the one thing that matters. `mc admin user add` and `mc admin policy
create` both succeed; **`mc admin policy attach` returns
`The specified method is not allowed against this resource`**. SeaweedFS's
identity model is its own `s3.configure` JSON — identities with coarse
`actions` (`Read`, `Write`, `List`, `Admin`, `Tagging`) — not AWS IAM policy
documents. Adopting it means the three boundary gates stop installing
`deploy/policies/tenant-zone/*.json` verbatim and start installing a
*translation* of them, which destroys the property those gates were written for.
Versioning also did not yield a readable noncurrent version through `mc`.

There is corroborating recent trouble in exactly these two areas:
issue #8516 (2026-03-05, v4.14) — *IAM policies created and attached, S3 calls
still AccessDenied* — and #8754 (v4.15) — lifecycle configuration cannot be
applied ([`sources/seaweedfs-issue-8516-iam.txt`](sources/seaweedfs-issue-8516-iam.txt),
[`sources/seaweedfs-issue-8754-lifecycle.txt`](sources/seaweedfs-issue-8754-lifecycle.txt)).
Its 533 MB image — three times Silo's — is the least of it.

### 4.5 The rest

* **Adobe S3Mock** — 2/7/4, and disqualified by design rather than by bug. Its
  own README's *Important Limitations* say *"Presigned URLs: Accepted but not
  validated (expiration, signature, HTTP verb not checked)"* and *"Not for
  production: S3Mock is a testing tool and lacks the security features required
  for production use"*
  ([`sources/adobe-s3mock-github.txt`](sources/adobe-s3mock-github.txt)). It
  does not authorize anything, and three gates exist to prove an authorization
  boundary. The probe never reached those: `mc admin user add` failed with
  *"The specified bucket does not exist"*, because there is no admin API to
  fail more informatively. It is also the slowest thing measured — a JVM, first
  answer at 61.2 s including the pull, 190 MB at boot — in a suite that stands
  one of these up per gate.
* **Garage** — smallest and fastest (26.6 MB image, 1.3 s boot, 31 MB), and its
  own compatibility table settles it:
  *"Garage does not (yet) support object versioning"*, `PutBucketVersioning`
  **Missing**, `PutBucketPolicy`/`GetBucketPolicy` **Missing**,
  `GetBucketVersioning` a **stub**, `PutBucketLifecycleConfiguration`
  **partially implemented** — *"The only actions supported are …"***
  ([`sources/garage-s3-compatibility.txt`](sources/garage-s3-compatibility.txt)).
  No bucket policy and no versioning kills `tenant_zone.sh`, `shipper.sh` and
  `deletion.sh` outright.
* **LocalStack (community)** — not probed, and the reason is structural rather
  than technical: *"LocalStack Community edition does not enforce IAM policy
  evaluation by default — all API calls succeed regardless of the IAM role or
  policy attached"*
  ([`sources/localstack-iam-enforcement.txt`](sources/localstack-iam-enforcement.txt)).
  A fake that grants everything turns `tenant_zone.sh` §4–§6 into a gate that
  passes because nothing is enforced — the worst available outcome for a
  boundary test. It is also the candidate most exposed to the failure mode this
  bead exists for: **from March 2026 LocalStack requires an account to run**
  ([`sources/localstack-road-ahead.txt`](sources/localstack-road-ahead.txt)),
  i.e. a credential in CI, which the operator has already rejected in another
  form.
* **Zenko CloudServer** — not probed. Its IAM lives in Scality Vault, which is
  not part of the open-source CloudServer distribution, so it lands in the same
  place as SeaweedFS with less maintenance behind it.
* **versitygw** (Versity S3 gateway, Apache-2.0) and **s3proxy** (Apache-2.0) —
  images resolve and both are alive, but both are *gateways over a filesystem*
  whose access control is their own (IAM plugins / single credential set), not
  AWS policy documents. Same disqualification, so neither was probed.
* **OpenMaxIO** — the other MinIO fork. It forked the *console*, not the server,
  and has been dormant since roughly its first commit. Not a candidate.

---

## 5. The options, with cost and risk

Costs below are **engineer-hours to adopt** plus **runner seconds/MiB per CI
run**, measured on this box unless stated.

### Option A — Keep the mirrored MinIO and revisit when it breaks (status quo)

* **Cost to adopt: 0 hours.** It is what is running.
* **Ongoing cost: 0 seconds, 0 MiB** — 176 MB image, 2.0 s boot, ~170 MB
  resident, the numbers CI already pays.
* **Risk.** The mirror is a GitHub Container Registry namespace holding two tags
  nobody will ever update, for software whose upstream is archived and read-only
  since 2026-04-25. Three failure modes, none of them loud:
  1. **The mirror goes away** — GHCR policy change, org rename, quota. CI stops
     being able to pull and 11 gates die at once, on somebody else's schedule.
     This is exactly the 2026-09-15 event, replayed with no second mirror to
     move to.
  2. **A CVE lands in `RELEASE.2025-09-07T16-13-09Z`.** Nothing will patch it.
     The blast radius is a test container on a private box, which is genuinely
     small — but it is unbounded in time.
  3. **Client drift.** `mc` is the same problem and the same mirror.
* **The cost when it breaks is on the record, in this repo.** `tests/lib/minio.sh`
  describes the 2026-09-15 event in its own comment: the first ephemeral CI run
  after the withdrawal *"pulled nothing, and the gates did not fail cleanly —
  they carried on without an object store and one of them sat in a wait until
  the job hit its 90-minute cap."* So the honest cost of A is not "do Option B
  later"; it is **one 90-minute CI job that reports the wrong thing, plus the
  diagnosis, plus Option B under time pressure** — and the failure mode is a
  gate carrying on *without a store*, which is the false-green direction
  `tests/lib/objects.sh` exists to close.
* **What it is actually buying: time, at the price of choosing the moment.**
  The bead's own framing — *"on our own timetable rather than on the timetable
  of whatever breaks it"* — is the argument against A, and it is a good one.
  But A is not irrational, and it should not be dismissed: nothing is broken
  today, the mirror is under our own org rather than a stranger's, and Option B
  is cheap enough that it will still be cheap in six months. **A is the right
  answer if and only if you expect to be less busy later than now.**

### Option B — **Silo + `pgsty/mc`** (recommended)

* **Cost to adopt: two constants, plus one regex, plus prose.** Concretely:
  1. the four lines of `tests/lib/minio.sh` — two image names, two versions;
  2. that file's comment block, whose registry argument is currently *"the
     DeckDumpster mirror … is the only source"* and would become *"the
     maintained fork, pinned"*;
  3. **`tests/lib/objects_test.sh:270`**, which greps the tree for
     `(docker\.io/)?minio/(minio|mc):` — after the switch that pattern no
     longer catches a hardcoded `pgsty/silo:` literal, so the ratchet that
     keeps one spelling in one place would silently stop ratcheting. This is
     the one thing a careless migration would miss.

  **Zero harness edits** — proven by running three real gates, one of them
  `tenant_zone.sh`, with only `MINIO_IMAGE`/`MC_IMAGE` overridden.
  Realistically **2–3 hours** including a full `deploy/ci.sh`.
* **Ongoing cost: a wash, slightly favourable.** 163 MB image (−13 MB), 1.0 s
  boot (−1.0 s). Memory is level with MinIO's — under the identical
  300 × 64 KiB workload, 59.97 MB idle / 176 MiB peak against MinIO's
  59.36 MB / 170 MiB. Both real harness runs were marginally *faster*
  (1 m 36.8 s vs 1 m 39.3 s; 38.2 s vs 39.0 s).
* **Risk.**
  1. **It is one maintainer's fork with 3,522 stars.** If Pigsty stops, we are
     back here — but back here with a *newer* frozen artifact than today's and
     the same three exit routes.
  2. **AGPL-3.0**, same as MinIO was. It is a test-only container we neither
     distribute nor offer over a network, so this is unchanged from today.
  3. **It inherits MinIO's codebase**, including any latent bug — but that is
     the point: the suite's expectations were written against that codebase.
  4. **`latest` moves.** Pin `RELEASE.2026-09-16T00-00-00Z` (digest
     `sha256:39aab3c3…`), exactly as `minio.sh` already argues — and widen the
     `objects_test.sh` scan in the same commit (cost item 3 above) so it goes
     on being the only spelling.

### Option C — RustFS

* **Cost to adopt: Option B plus 4 harness edits.** One `chmod 777` line each
  in `alarming/run.sh` and `litestream/{run,drill,recreate}.sh`, or `--user 0:0`
  at 11 launch sites. Call it 3–4 hours with a full container-tier run to find
  anything else the uid change surfaces.
* **Ongoing cost: +125 MB image; +84 MiB resident per instance.** Under the
  identical `memprobe.sh` workload RustFS peaked at **260 MiB** against Silo's
  176 MiB and MinIO's 170 MiB, and its *idle* figure is the striking one —
  244 MB against 59–60 MB for both Go servers, so it costs the headroom whether
  or not a gate is doing anything. With two gates in flight on a 6144 MiB
  runner that is ~170 MiB given up for nothing the suite needs.
* **Benefit over B: licence and momentum.** Apache-2.0 rather than AGPL, ~10×
  the stars (33,898 vs 3,522), a release every day or two, and a genuinely
  different implementation rather than a frozen fork — so it is more likely to
  still exist in 2029 than a one-maintainer fork is. **Who funds it I could not
  establish**; the repository is a company-shaped org and the project has been
  listed in the Runa Capital ROSS index and NVIDIA's Inception programme, but I
  found no funding disclosure.
* **Risk.** 1.0 GA is **nine days old**. The IAM evaluator — the only part of
  the surface this suite leans on hard — carries `CVE-2026-22043` (fixed
  pre-GA) and two open correctness issues in the same area. A red gate is
  cheap; an IAM bug that makes a **Deny** gate pass for the wrong reason is
  not, and three of the eleven gates are exactly that shape.

### Option D — Real AWS S3

Rejected by the operator on 2026-09-25 (db-8wy4/db-ht2v/db-m0h1): credentials
in CI, per-run spend, network flakiness introduced into tests that have none,
and one IAM mistake between a test suite and prod's data lake. Recorded as the
baseline, not re-proposed. Nothing measured here changes that reasoning; if
anything §2.3 strengthens it, since the gates that would most benefit from real
S3 fidelity are the three that would be granted real IAM to do it.

### Option E — Need no fake (`PKDUMP_LAKE_DIR`)

* **Cost: 0 hours** for the one file that already needs nothing.
* **For the 11 container gates: it is not an option**, per §2.5. Converting them
  is not cheaper, it is a deletion of coverage wearing a cost saving's clothes.

---

## 6. Recommendation

**Adopt Option B: pin `tests/lib/minio.sh` to
`docker.io/pgsty/silo:RELEASE.2026-09-16T00-00-00Z` and
`docker.io/pgsty/mc:RELEASE.2026-09-16T00-00-00Z`.**

It is the only candidate that costs *nothing but the constant* — measured, by
running three real harnesses with no harness edit, one of them the hardest gate
in the suite — while restoring an active maintenance line, signed artefacts and
a client fork on the same cadence. It is smaller and marginally faster than what
it replaces. And because it is the same codebase, the eleven gates' accumulated
expectations about MinIO's behaviour keep holding by construction rather than by
re-testing.

**Silo and RustFS are not equivalent, and the difference is not the probe
score** — both scored 17/17 against every requirement the suite has. The
difference is that one of them needs no harness to change and the other needs
four, and that difference is a proxy for a larger one: with Silo, *anything the
suite has quietly come to assume about MinIO over two years* keeps holding,
because it is MinIO. With RustFS, each such assumption is a coin flip resolved
by a CI run. The uid problem is the first of those to surface; it is not
evidence that it is the last.

**The one load-bearing assumption: that Pigsty keeps publishing.** Everything
else about this recommendation is measured; that is forecast. It is supported —
`pushed_at` today, six server releases since April, a client fork released in
lockstep, signed multi-arch images with SBOMs — but it is one organisation, and
a fork's half-life is not a repository statistic.

That assumption is deliberately cheap to be wrong about. Because Silo is a
drop-in, *moving off* Silo later costs exactly what moving onto it costs: the
same two constants. The decision is reversible for the price of a commit, which
is why the newer, larger, more permissively-licensed RustFS is the runner-up
rather than the pick — its advantages are real but they are advantages for a
*dependency you are stuck with*, and this is not one.

**Adoption steps** (not done here — this spike leaves no code):

1. Change the four constants in `tests/lib/minio.sh` to
   `docker.io/pgsty/silo:RELEASE.2026-09-16T00-00-00Z` and
   `docker.io/pgsty/mc:RELEASE.2026-09-16T00-00-00Z`, and rewrite its comment
   block: the registry decision is no longer "the DeckDumpster mirror is the
   only source" but "the maintained fork, pinned".
2. **Widen `tests/lib/objects_test.sh:270`'s pattern** in the same commit, or
   the ratchet that makes `minio.sh` the only spelling stops seeing the new
   name. A gate that has quietly stopped ratcheting is the failure mode this
   repository has paid for more than once.
3. `bash deploy/ci.sh`, whole. Eight of the eleven gates were not completed
   here (§8) and the three IAM gates are the ones worth watching.
4. Keep `ghcr.io/deckdumpster/{minio,mc}` in place for a release or two. It
   costs nothing and it is the rollback.
5. **Separately**, pin `docker.io/amazon/aws-cli:latest`, which is a moving tag
   in three gates and is the same class of bug as pd-pfxf. Filed as `db-fdct`
   (§8).

---

## 7. The falsifier

The recommendation is wrong if **any** of these turns out to be true. Each is
checkable, and #2 has already been checked.

1. **A gate that was not run here fails against Silo for a reason other than
   the store's identity.** Three of eleven were run end to end
   (`tests/lake/run.sh`, `tests/litestream/run.sh`, `tests/lake/tenant_zone.sh`).
   The eight not completed include two of the three `mc admin` IAM gates —
   whose *primitives* were probed green (R3/R4/R5 in both directions, plus
   versioning and lifecycle), and one of which, `tenant_zone.sh`, does install
   the full rendered policy documents and did pass. **Check:**
   `MINIO_IMAGE=docker.io/pgsty/silo:RELEASE.2026-09-16T00-00-00Z MC_IMAGE=docker.io/pgsty/mc:RELEASE.2026-09-16T00-00-00Z bash deploy/ci.sh`.
   This is the single highest-value thing to do before adopting, and it is one
   command.
2. **Silo's `mc` fork is not interchangeable with `minio/mc` for `mc admin`.**
   **Checked, and it holds**: `MC_IMAGE=docker.io/pgsty/mc:latest bash probe.sh silo`
   → 17/17, and `tests/lake/tenant_zone.sh` PASSED with **both** halves pinned
   to Pigsty. This falsifier is closed; it is kept because it is the thing to
   re-check when either fork bumps a release.
3. **Pigsty stops publishing.** Concretely: no `pgsty/silo` release for
   9 months, or `pushed_at` older than 6 months. Then Option C (RustFS) becomes
   the pick, and by then its 1.0 line will have the track record it lacks today
   — which is the argument for revisiting rather than pre-empting.
4. **The suite grows a requirement Silo does not have.** The likeliest is
   multipart upload, which nothing exercises today (§2.4) — a fixture crossing
   8 MiB would start exercising it silently. Silo inherits MinIO's multipart
   implementation so this is unlikely to bite, but "unlikely" is the word
   doing the work.
5. **The AGPL becomes a problem.** It will not while this is a test container
   on a private box, but if a Silo container were ever shipped *to* anyone or
   offered over a network, re-read the licence. Option C is Apache-2.0
   specifically for that world.

---

## 8. What I could not establish

Marked honestly, because an inferred answer stated confidently is worse than a
named gap.

* **Eight of the eleven gates were not completed against Silo, and why matters
  per gate.** Three were run green end to end (`lake/run.sh`,
  `litestream/run.sh`, `lake/tenant_zone.sh`) — and that last one is the reason
  the residual risk is small rather than merely unmeasured: it is the gate that
  installs the production policy documents and asserts the boundary in both
  directions, seen green and red. Of the rest:
  * `lake/prices.sh` **could not run in this environment** — its §0 runs
    `cargo test` on the *host*, and this box's cargo is 1.93.1 while
    `Cargo.lock` needs 1.94.1, with no `rustup` present. A local toolchain gap,
    nothing to do with any candidate, and it is why
    `spike-poc/logs/prices_silo.log` shows `rc=1` two seconds in.
  * `lake/{deletion,shipper,value_snapshots,phase3}.sh` need
    `pkdump_image_ensure` — a full containerised release build of the
    workspace. **`deletion.sh` was run against Silo and got through §0** —
    a **versioned** bucket created (`mc version enable`), both rendered policy
    documents installed on two identities — and then died in §1, its own image
    build, after 8 m 57 s:

    ```
    rust-lld: terminate called after throwing an instance of 'std::system_error'
                what():  Resource temporarily unavailable
    error running container: reading container state from /usr/bin/crun:
      fork/exec /usr/bin/crun: resource temporarily unavailable
    ```

    That is the linker failing to spawn a thread on a 1-core / 3958 MiB box,
    not anything to do with the object store. It would fail identically against
    MinIO. Not retried, because it would fail identically again.
    `spike-poc/logs/deletion_silo.log`.
  * `alarming/run.sh` and `litestream/{drill,recreate}.sh` are long gates that
    were deprioritised in favour of `tenant_zone.sh`, which exercises strictly
    more of the object store.

  The requirement probe covers all of their *primitives* — the three IAM
  checks, versioning and noncurrent reads, lifecycle, the Rust SDK's five
  operations, Litestream's TXID line — but "every primitive passes" is not
  "the gate passes". §7.1 is the check, and it is one command.
* **Whether Litestream 0.5.17 uses conditional writes** against S3, and
  therefore whether a candidate could pass R8 for the wrong reason. It was not
  instrumented. Since MinIO, Silo, RustFS and SeaweedFS all passed R8 with the
  shipped config, whatever it asks for is widely satisfied.
* **Garage's true score.** Its R1/R2/R3 failures are my probe's — it needs keys
  minted through its own admin API first. Its disqualification (no versioning,
  no bucket policy) comes from its own documentation, not from my run.
* **The 6144 MiB runner's real headroom** with eleven gates going two at a time.
  Everything here was measured on a 1-CPU / 3958 MiB box. The *relative* numbers
  transfer; the absolute ones do not. `podman stats` MemUsage also includes page
  cache — the same MinIO container read 72, 138, 172 and 174 MB across five
  probe runs, which is why §4.1 quotes the controlled `memprobe.sh` figures
  (same workload, same sampling) rather than the probe's incidental one, and why
  even those are a **rank order rather than a budget**.
* **Litestream's R8 result for Garage and S3Mock.** Both reported a *local*
  SQLite condition (`db not ready: page size not initialized`) rather than a
  store error, so the probe could not say anything about the store. Recorded as
  not established.
* **`pgsty/mc` against the other two IAM gates.** `tenant_zone.sh` drives
  `user add`, `policy create`, `policy attach` **and `policy detach`** — the
  full set — and passed. `shipper.sh` and `deletion.sh` use no admin subcommand
  `tenant_zone.sh` does not, so the residual risk is about *those gates*, not
  about the client.

**Adjacent work found and not done** (filed, not fixed):

* `docker.io/amazon/aws-cli:latest` is an unpinned moving tag in
  `tests/lake/tenant_zone.sh`, `tests/litestream/drill.sh` and
  `tests/litestream/recreate.sh` — the pd-pfxf failure mode with a different
  image. → bead `db-fdct`.
* `mc_as()` in `tests/lake/{tenant_zone,shipper}.sh` runs `podman run` **without
  `-i`**, so `printf … | mc_as … pipe …` writes a **zero-byte object**. Harmless
  where it is used (a permission probe only needs the PUT to be attempted) but
  it means `can_put` is not writing the bytes it appears to. → bead
  `db-6wh2`.

---

## 9. Reproducing this

Everything is on branch `spike/db-hny6-poc`, under `spike-poc/`:

| file | what it does |
|---|---|
| `probe.sh` | the 17-check requirement probe. `bash probe.sh {minio,silo,rustfs,seaweedfs,garage,s3mock}` |
| `memprobe.sh` | steady-state memory under 300 × 64 KiB objects |
| `rustprobe/` | the Rust `aws-sdk-s3` probe (§3); built in `rust:1.94-slim-bookworm`, 20 m 03 s cold on one core |
| `logs/` | every real-harness run, verbatim, with its `time` and exit status |
| `logs/probe-runs.txt` | the console output of all seven probe runs |

The real-harness runs need no code at all — this is the whole finding:

```bash
MINIO_IMAGE=docker.io/pgsty/silo:latest bash tests/lake/run.sh
MINIO_IMAGE=docker.io/pgsty/silo:latest bash tests/litestream/run.sh
MINIO_IMAGE=docker.io/pgsty/silo:latest MC_IMAGE=docker.io/pgsty/mc:latest \
  bash tests/lake/tenant_zone.sh
```

### Two numbers worth keeping

**The slowest thing in this session was the Rust probe's cold compile — 20 m 03 s
for `aws-sdk-s3` + `aws-config` on one core** — against roughly 1 m 40 s for a
whole `tests/lake/run.sh`. There is no shared fixture for a standalone AWS-SDK
binary in this repo, so a spike that wants to test an S3 surface against the
real Rust client builds its own; if this comes up again, keep the binary rather
than rebuilding it.

**Second: `tests/lake/prices.sh` could not be run in this session.** Its §0
runs `cargo test -p pkdump-ingest --test prices_fixture` on the **host**, and
this box's system cargo is 1.93.1 while `Cargo.lock` pins an AWS SDK needing
1.94.1, with no `rustup` to switch. That is a property of the aeon's
environment, not of the repository — `rust-toolchain.toml` asks for 1.94 and a
box with `rustup` would fetch it — but it is why one of the eleven gates is
absent from the evidence, and it would equally block the whole `rust` tier of
`deploy/ci.sh` here.
