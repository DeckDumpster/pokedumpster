# Spike: ATTACH-across-namespaces in self-hosted `sqld`

> ## ⚠️ This path was NOT taken
>
> These spikes validated a libSQL/`sqld` + per-tenant-namespaces substrate.
> That direction was **rejected**. The direction actually chosen is
> **file-per-tenant local SQLite + Litestream multi-DB replication** — epic
> **pd-gckl** — which keeps today's rusqlite stack and its connection-scoped
> `ATTACH` unchanged, so the `cat.`-qualification sweep and async ripple these
> spikes costed never has to be paid.
>
> The analysis is kept deliberately: it is the evidence for *why* libSQL was
> rejected. Read it as a record, not as a plan.
>
> **Known correction inside `RESULT.md`:** the TEMP-VIEW spike (spike 2)
> recommended attaching the catalog once at connection open; the Rust `libsql`
> client spike (spike 3) disproved it — attach must live inside each
> `transaction()`. Both results stand in the document with the contradiction
> marked in place.

**Issue:** pokedumpster-5jv
**Status:** throwaway spike — NOT a migration commitment; path since rejected
(see the banner above).

> **Where the runnable scripts went.** Only the findings (`RESULT.md`,
> `SUMMARY.md`, this file) live in-tree. The reproduction scripts
> (`run*.sh`, `probe.py`, `jwt_helper.py`, `temp_view_spike.py`,
> `rust-client/`) were removed to keep the repo lean but remain in git
> history. Restore them next to these docs with:
>
> ```bash
> git checkout 72cba6b -- spikes/sqld-attach-namespaces/
> ```
>
> The `spikes/sqld-attach-namespaces/…` paths referenced below resolve once
> you do that.

## The one question this answers

PokeDumpster's storage rests on a shared-catalog design: each user DB `ATTACH`es
the immutable `shared.sqlite` **read-only** and joins catalog tables unqualified
(via TEMP VIEWs). Any move to libSQL/`sqld` for multitenancy ("Path A": one
`sqld`, namespaces on, bottomless to S3) only works if that ATTACH pattern
survives in `sqld` **server mode, across namespaces**.

Turso marks `ATTACH` "deprecated for new users" and says it "doesn't support
Embedded Replicas" — but a self-hosted `sqld` discussion claims server-mode
ATTACH across same-instance namespaces works. The docs don't settle it. So we
test it empirically.

**Pass:** from a per-tenant namespace we can `ATTACH` a separate read-only
catalog namespace and get **joined rows** back in a single SQL statement.
**Fail:** ATTACH is rejected / unsupported → the shared-catalog architecture
would need rework before any libSQL move.

## What `run.sh` does

1. Starts `ghcr.io/tursodatabase/libsql-server` with `--enable-namespaces` + admin API.
2. Creates two namespaces: `catalog` (a `cards` table) and `tenant1` (a `collection` table).
3. Seeds both; sanity-selects each in isolation.
4. **The test:** from `tenant1`, runs `BEGIN; ATTACH catalog AS cat; SELECT … JOIN cat.cards …; COMMIT`,
   trying several ATTACH syntaxes until one works (or all fail).
5. Prints a VERDICT and tears the container down (`KEEP=1` to leave it running).

All client traffic uses the Hrana HTTP pipeline (`/v3/pipeline`) via `probe.py`
(stdlib only) — SQL semantics are identical to the Rust `libsql` client, so the
verdict is client-agnostic. Namespace routing is by `Host` header.

## Run

```bash
spikes/sqld-attach-namespaces/run.sh          # full run + teardown
KEEP=1 spikes/sqld-attach-namespaces/run.sh   # leave container up for poking
```

See `RESULT.md` for the recorded outcome.
