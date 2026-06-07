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
| [`sqld-multitenancy/`](sqld-multitenancy/) | Can PokeDumpster's shared-catalog (ATTACH read-only) survive a move to libSQL/`sqld` with per-tenant namespaces + S3 backup? Plus JWT auth, bottomless, sigv4-proxy validation. | Validated; multitenancy deferred (see `pokedumpster-cz8`, `-181`) |
