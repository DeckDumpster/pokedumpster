# Spike: one Litestream sidecar, N tenant databases

**Issue:** `pd-o98z` — GATE bead for epic `pd-gckl` (per-tenant data model:
file-per-tenant + Litestream multi-DB).
**Verdict:** **PASS** — see [`RESULT.md`](RESULT.md).

## The one question this answers

Epic `pd-gckl` proposes one SQLite file per tenant, the shared catalog still
`ATTACH`ed as today, and **the single Litestream sidecar already running in
production** replicating every tenant database.

That whole plan rests on one claim that had never been tested: that Litestream
can replicate N separate databases from one process and restore any **one** of
them to a chosen point in time, independently. The only evidence for it was
that `deploy/litestream.yml` has a `dbs:` *list* rather than a single `db:`
key — a reading of a config file, not a demonstration.

If the claim were false, the rejected libSQL/`sqld` path would regain its
strongest argument (namespace-native backup) and the epic's answer would be
outcome 2, abandon.

**Pass:** two-plus tenant databases replicate from one sidecar, and tenant #2
restores to an earlier point in time while the others stay current and
untouched.
**Fail:** any of that is impossible → the epic rests on a false premise.

## What `run.sh` does

1. Stands up a throwaway MinIO on its own podman network, and a bucket.
2. Creates **three** tenant databases — three so that "restore the second one"
   is a real middle element, not just "the other one".
3. Writes ONE config with a `dbs:` list, starts ONE `litestream replicate`.
4. Drives 50 rounds of **interleaved concurrent** writes — every round forks a
   writer per tenant and `wait`s, so their transactions overlap. A marker
   timestamp is taken between phase 1 and phase 2.
5. Restores tenant #2 to the marker (out-of-place), restores every tenant to
   latest, then runs a real in-place DR drill: delete tenant #2's live file and
   restore over it, asserting the other two are byte-identical (sha256) before
   and after.
6. §12–16 probe the constraints downstream beads have to build against —
   global vs per-database schedules, replica-path collisions, missing-database
   blast radius, `dir`/`pattern`/`watch` mode, and Litestream's own tables.
   These are assertions too, so a future Litestream that changes them fails
   loudly rather than silently.

Exit 0 = PASS. `KEEP=1` leaves MinIO and the work dir up for poking.

## Run

```bash
deep-dives/litestream-multi-db/run.sh          # ~2 min, full run + teardown
KEEP=1 deep-dives/litestream-multi-db/run.sh   # leave containers + WORK in place
```

Self-contained and prod-safe: its own network, MinIO, temp dir and SQLite
files. It touches no `pkdump-*` unit, not `pkdump-prod-data`, not the live
`collection.sqlite`, and no application code.

## Headline findings

Beyond the PASS itself, two things change what downstream beads should build:

- **`dir:` + `pattern:` + `watch: true`** is a native N-database mode in 0.5.
  Per-tenant replica paths are derived from the filename, and a database
  created *after* the sidecar started is picked up with no restart. Generating
  per-tenant config (`pd-fof4`) may be work that does not need doing. The
  trade-off is that restore must then go by replica URL, not `-config DB_PATH`.
- **Colliding replica paths are accepted silently and leak across tenants** —
  restoring tenant C from a collided prefix returned tenant A's data with
  `integrity_check` = `ok`. In `dir` mode this is unreachable by construction;
  in explicit-list mode it needs a test.

`RESULT.md` has the evidence and the rest of the constraints.
