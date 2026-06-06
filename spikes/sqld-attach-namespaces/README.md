# Spike: ATTACH-across-namespaces in self-hosted `sqld`

**Issue:** pokedumpster-5jv
**Status:** throwaway spike — NOT a migration commitment.

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
