# Deep dives

Durable write-ups of exploratory work — research spikes, architecture
evaluations, validation drills. Distinct from:

- `RESEARCH.md` / `PLAN.md` — the frozen v1 design record.
- `architecture/` — design notes for *current* shipped code.

These are kept in-tree (not history-only) when the findings inform work we
intend to pursue, so picking that work up later means **as little rework as
possible**. Throwaway *scripts* that produced the findings usually stay in git
history; each dive notes the recovery commit.

| Dive | What it answers | Status |
|---|---|---|
| [`sqld-multitenancy/`](sqld-multitenancy/) | Can PokeDumpster's shared-catalog (ATTACH read-only) survive a move to libSQL/`sqld` with per-tenant namespaces + S3 backup? Plus JWT auth, bottomless, sigv4-proxy validation. | Validated, then **path NOT taken** — multitenancy went to file-per-tenant SQLite + Litestream (epic `pd-gckl`). Kept as the record of why libSQL was rejected. |
| [`litestream-multi-db/`](litestream-multi-db/) | Can ONE Litestream sidecar replicate N tenant SQLite databases, and restore any one of them to a chosen point in time without disturbing the others? | **PASS** — GATE bead `pd-o98z` for epic `pd-gckl`; `run.sh` is live and re-runnable |
| [`attach-concurrency/`](attach-concurrency/) | What happens when N tenants read the shared catalog through `ATTACH` at once — does anything serialise, and does the catalog still have exactly one writer? | **No contention** — bead `pd-jgd4` for epic `pd-gckl`; `run.sh` is live and re-runnable |
