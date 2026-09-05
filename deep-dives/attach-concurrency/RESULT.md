# RESULT: sharing one `ATTACH`ed catalog across N tenants costs nothing

**Issue:** `pd-jgd4` (epic `pd-gckl`). **Not a gate** — a measurement. The
epic is not blocked either way; these are the numbers it should be defended
with, and the three findings in §4 are filed as their own beads.

- **Hardware:** 4 cores, 15 GiB RAM. SQLite via `rusqlite` 0.32.
- **Catalog:** generated, prod-scale — 200 sets, 43,586 cards, 87,210
  printings, 87,210 `latest_prices`, 111 MiB. Deterministic from a fixed seed.
- **Tenants:** one collection database each, provisioned through the real
  `pkdump tenant create` path, 3,000 owned rows apiece.
- **Workload:** the real `pkdump_db::binder::get_binder_page`, default
  `BinderQuery`, rotating across all 200 sets.
- **Evidence:** `run.sh`, 4 repetitions × 10 s per cell, 4 arms × 5 reader
  counts. ~11,000 binder pages rendered.

## The answer

**No contention.** Across every scenario, every reader count and every
repetition — **zero** `SQLITE_BUSY` or `database is locked` outcomes,
including with a writer committing to the catalog throughout.

N tenants reading one shared catalog perform the same as N tenants each
reading their own private copy of it. SQLite in WAL mode does what it says:
readers do not block readers, and a writer does not block readers.

The epic's premise holds. Nothing here argues for denormalising the catalog
into each tenant.

## 1. Shared vs private: the control arm

The comparison that carries the result. `shared` is the epic's design: N
tenant connections, one catalog file. `private` is the same load with the
catalog **not** shared — N tenants, N separate 111 MiB copies. A ratio of
1.00 means sharing costs nothing.

Both arms start with every file pulled through the page cache, run back to
back at each reader count, and pool latencies across repetitions.

| n | p50 ratio | p95 ratio | ops/s ratio |
|---|---|---|---|
| 1 | 1.00 | 0.88 | 1.02 |
| 2 | 1.20 | 1.06 | 0.92 |
| 4 | 1.07 | 1.15 | 0.90 |
| 8 | 0.94 | 0.94 | 1.04 |
| 16 | 1.11 | 1.10 | 0.87 |

Every ratio straddles 1.00, and **none of them trends with n** — which is
what noise looks like, and is not what a serialisation point looks like. A
lock held across the catalog would climb monotonically as readers were added;
p50 goes 1.00, 1.20, 1.07, 0.94, 1.11 instead.

Worth recording because it nearly became a finding: an earlier 3-repetition
sweep showed p95 ratios of 0.94/1.40/1.45/1.12/1.27, which looked like a
consistent tail penalty for sharing. It did not survive more samples. With
four repetitions the same column reads 0.88/1.06/1.15/0.94/1.10. At a few
hundred samples per cell, p95 is resting on a handful of observations —
treat any p95 claim from a short run of this harness with suspicion.

## 2. What the numbers actually are

Absolute latency, pooled across repetitions (`shared` arm):

| n | ops/s | p50 | p95 | p99 | max |
|---|---|---|---|---|---|
| 1 | 10.3 | 95 ms | 115 ms | 130 ms | 169 ms |
| 2 | 14.1 | 132 ms | 224 ms | 281 ms | 421 ms |
| 4 | 18.0 | 200 ms | 344 ms | 631 ms | 729 ms |
| 8 | 19.6 | 397 ms | 559 ms | 684 ms | 845 ms |
| 16 | 17.6 | 872 ms | 1322 ms | 1477 ms | 1639 ms |

Throughput flattens at ~18–20 ops/s from n=4 onward. The box has 4 cores and
this query is pure CPU: past four in-flight pages, requests queue for a core
and latency rises in proportion. The `private` arm rises identically.
**That ceiling is not the catalog.** It is finding A.

## 3. The `same_tenant` control — what serialisation actually looks like

`Tenants` holds one connection per tenant behind one mutex, so two requests
for the *same* tenant serialise by construction. Measuring it gives the "no
contention" claim something to be measured against: a sweep in which nothing
serialises anywhere is more likely a broken harness than a lucky design.

| n | ops/s | p50 | p95 |
|---|---|---|---|
| 1 | 10.3 | 95 ms | 117 ms |
| 2 | 10.2 | 101 ms | 682 ms |
| 4 | 9.9 | 110 ms | 1688 ms |
| 8 | 10.9 | 101 ms | 3819 ms |
| 16 | 11.1 | 116 ms | 6959 ms |

This is the signature: **throughput pinned flat** at one connection's worth
(~10 ops/s) no matter how many workers arrive, p50 unchanged because the work
itself did not get slower, and p95 growing linearly in n as the queue behind
the mutex grows. The `shared` arm shows none of it — it scales to the core
count and stops there. The harness can see serialisation; there is none
between tenants.

It also says something about the deployment worth writing down: **one
tenant's traffic is capped at one connection.** For "several friends", each
on their own database, that is fine and arguably desirable — it bounds what
any one person can do to the box. It would not be fine for several concurrent
sessions belonging to one tenant, but nothing in this epic proposes that.

## 4. FINDINGS

Three. All pre-existing, none caused by multi-tenancy, all filed rather than
fixed — this is a measurement bead.

### A. A binder page costs ~95 ms regardless of set size — `pd-qce0`

`get_binder_page`'s printings query scans **every printing in the catalog**
and builds an index over the result, on every call:

```
|--CO-ROUTINE p
|  `--COMPOUND QUERY
|     |--LEFT-MOST SUBQUERY
|     |  `--SCAN shared.printings          <-- all 87,210 rows
|     `--UNION ALL
|        `--SCAN user_printings
|--SEARCH shared.cards USING INDEX idx_cards_set (set_code=?)
|--SEARCH p USING AUTOMATIC COVERING INDEX (card_id=?)   <-- rebuilt per call
```

The `(printings UNION ALL user_printings)` subquery cannot use
`idx_printings_card`, so SQLite materialises the whole union and builds an
automatic covering index over it. Rendering one 274-card page returns 533
printings and takes 95 ms; the set filter is applied *after* the scan, so
cost tracks total catalog size, not page size.

This is the real ceiling on how many people can browse at once, and it is
there today with one user. It is not an `ATTACH` problem and not a
multi-tenancy problem — but N tenants multiply it, which is why it surfaced
here. Fixing it would also make this harness far more precise: at ~95 ms an
operation, a 10-second cell yields a few hundred samples, which is why §1 has
to hedge about p95.

### B. The catalog WAL grows without bound during a refresh — `pd-t50h`

> **FIXED 2026-09-03 (pd-t50h).** The measurement below stands; what it
> describes no longer happens. Two truncating checkpoints — an opportunistic
> one every 5s from inside variant expansion (100ms wait), and one with a 30s
> wait at the end of the write window — bound the file during a derivation and
> return it afterwards. `journal_size_limit`, the first mitigation this section
> proposed, was measured and does **not** work: it truncates only when a
> checkpoint resets, which is the thing a reader prevents. The writer also
> moved — since pd-lunn `pkdump data refresh` writes no catalog table at all,
> and the nightly writer is `pkdump-lake-derive shared`. See CLAUDE.md, "The
> catalog's WAL, and what gives it back".

`pkdump data refresh` writes to the catalog while the server serves reads
(`deploy/pkdump-refresh.service` runs `podman exec` into the *running*
container). With any reader in flight, the WAL only appends. Same writer,
same commit loop, same duration; only the reader count varies:

| readers | writer commits / 10 s | WAL left behind |
|---|---|---|
| **0 (control)** | 26,518 | **4.0 MiB** |
| 1 | 28,661 | 914.1 MiB |
| 2 | 30,779 | 992.4 MiB |
| 4 | 28,031 | 861.9 MiB |
| 8 | 20,664 | 658.7 MiB |
| 16 | 11,727 | 421.0 MiB |

The control is the finding. With no readers the autocheckpoint restarts the
WAL and it sits at ~4 MiB. **One** reader takes it to ~900 MiB — a ~230×
difference. It is binary rather than proportional: growth is ~31 KB per
commit at every reader count, and the totals fall at high n only because the
writer gets fewer commits in when readers are eating the CPU. A checkpoint
can copy frames out while readers are active but cannot *reset* the WAL until
a moment arrives with nobody reading.

Two caveats on the absolute numbers. This writer commits ~2,900 times a
second, far harder than a real refresh, so the MiB/s does not transfer — the
mechanism and the 0-vs-1 reader contrast do. And the catalog is deliberately
*not* replicated by Litestream, so this is disk space on the data volume, not
a backup problem. What multi-tenancy changes is the likelihood that somebody
is always reading.

**Measurement note, because it nearly produced a false finding.** SQLite
checkpoints and *deletes* the `-wal` when the last connection to a database
closes. The first version of this harness stat'ed the file after joining its
threads, and the writer-only control reported `0 B` — which reads exactly
like "the checkpoint kept up perfectly" and is instead "the file was deleted
on close". The harness now stats the WAL while every connection is still
open. Anyone re-running this should keep that ordering.

### C. The shared catalog has a second writer entry point — `pd-dzu5`

The bead asked to confirm exactly one writer. There is not one.

**The request path is clean.** `connect_user` attaches the catalog
`mode=ro`, `assert_isolated` refuses a connection wired to anything but this
tenant's file plus that one catalog, and
`connection.rs::attach_exposes_catalog_and_enforces_readonly` already asserts
that a write through the attachment fails. No tenant can write the catalog,
at any tenant count.

**But two processes open it read-write, and they can overlap:**

| Writer | Where | When |
|---|---|---|
| `pkdump setup` | `pkdump-cli/src/setup.rs:61` | manual |
| `pkdump data {refresh,expand-only,apply-corrections,normalize-symbols}` | `pkdump-cli/src/data.rs:89,148,187,208` | **nightly timer** |
| `pkdump serve` **startup** | `pkdump-server/src/lib.rs:190` | **every deploy/restart** |
| `pkdump seed-fixture` | `pkdump-cli/src/fixture.rs:55` | dev only |

`pkdump serve` calls `open_shared` read-write before serving — schema DDL,
`add_missing_columns`, five seed reconciles, then `search_meta::reconcile`.
That is deliberate (a binary upgrade can ship a data-only migration) and it
happens once at startup rather than per request, so **it does not scale with
tenant count**. But `deploy/pkdump-refresh.service` fires `pkdump data
refresh` into the running container nightly, and `deploy.sh` can restart the
server at any moment. Nothing serialises the two; the only thing between them
is `busy_timeout(5 s)`, after which a server start fails with "database is
locked" rather than retrying — a failed deploy, not corruption.

Unlikely, and unchanged by this epic. But the bead is right that no test
covers it.

## 5. What this does and does not license

- **Does:** keeping the catalog as one file `ATTACH`ed per tenant, at the
  handful-of-friends scale this epic targets. There is no contention to
  design around, and the shared page cache means one file is if anything
  cheaper than N copies.
- **Does:** treating "several tenants browse at once" as a CPU-provisioning
  question rather than a database-architecture one.
- **Does not:** any claim about how many tenants the *box* serves. On 4 cores
  it is roughly four concurrent binder-page loads before latency goes
  superlinear — and that is finding A's fault, not `ATTACH`'s.
- **Does not:** the absolute latencies. They come from a generated catalog on
  one machine. Re-take them against the real catalog if a number is ever
  load-bearing.
- **Does not:** anything about concurrent *writers* to a tenant database.
  This measured catalog reads. Two sessions writing one tenant's collection
  is the `same_tenant` mutex, not SQLite, and was not the question.
