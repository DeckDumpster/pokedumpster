# Gate suite timing — warm-cache measurement

**Bead:** db-pag5  
**Date:** 2026-09-21  
**Purpose:** Determine SPIRA_GATE_LOCK_WAIT sizing from a warm-cache run, before adjusting any gate timeouts (operator request, turn 254).

---

## What was measured

`cargo test`, `cargo clippy --all-targets`, and `cargo fmt --check` run in the
same worktree after a cold build had already warmed the cargo cache.  Machine:
same box that runs prod, with two concurrent aeons building. The full measurement
script is `timed.sh` (committed in this dive); the raw output is `timed.out`.

## Results

```
PHASE1 cold (populating cache, NOT the answer)
  cold build                     1s   rc=0    ← cache already warm from earlier build
  cold clippy                    0s   rc=0
  cache size                   8.5G

PHASE2 WARM, nothing changed:
  fmt                            0s   rc=0
  test                          96s   rc=0   ← 411 dominant tests: 39.97s
  clippy                         1s   rc=0
  TOTAL WARM SUITE:            ~97s

PHASE3 second warm run (high system load: 9.96 load avg):
  test --no-run (build)          1s   rc=0
  test (run only)              180s   rc=0   ← same 411 tests: 96.36s
  TOTAL (higher load):         ~181s

done 2026-09-21T02:04:24Z  load 9.96 7.74 7.03
```

The dominant test binary runs 411 tests. Under moderate load it finishes in ~40s;
under 9.96 loadavg (2.5x the CPU count) it takes 96s. Wall-time variance is 97–181s
depending on concurrent workload.

## Gate log context (before cheapgate fix)

From `gate.log` with the full two-tier CI suite as the gate and cold cargo cache:

```
23:12:06Z db-6gob   ran=855s rc=0  pass
23:24:55Z db-5tgs   ran=769s rc=1  branch-red
23:43:43Z db-ae5u   ran=460s rc=76 base-red
23:59:36Z db-5tgs   ran=709s rc=0  pass
00:13:00Z db-ae5u   ran=803s rc=1  branch-red
```

Lock wait was 360s (LAND_GATE_RESERVE=3600 → 3600/10=360). Three rc=75
lock-timeout failures observed: `db-f6f2`, `db-ae5u`, `db-dbb9`.

## Gate log context (after cheapgate fix, pr=128)

Gate changed to `cargo fmt --check` alone:

```
2026-09-21T03:34:05Z  ran=1s  rc=0 pass
2026-09-21T03:34:08Z  ran=1s  rc=0 pass
2026-09-21T03:34:10Z  ran=1s  rc=0 pass
```

No rc=75 seen since. Eight branches certified in one pass.

## Conclusions

| Scenario | Gate time | Lock wait (360s) | Fits? |
|---|---|---|---|
| Current gate: `cargo fmt --check` | 1–2s | 360s | Yes |
| Full suite warm, moderate load | ~97s | 360s | Yes |
| Full suite warm, high load (9.96) | ~181s | 360s | Yes |
| Full suite cold, two concurrent builders | 700–948s | 360s | **No** |

The cold-cache case is the one that caused #171. Warm cache fits comfortably
within the current 360s lock wait even under high system load.

**SPIRA_LAND_GATE_RESERVE does not need to change** while the gate is
`cargo fmt --check`. If the gate is ever expanded back to include `cargo test`,
it will fit within 360s under normal and moderate load, but a cold-cache run
alongside two concurrent builders would still exceed it. The fix for that case
is cargo cache warming (already done in production), not a higher reserve.

**The decoupling fix (#176)** — making `landing.sh` respect an explicitly-set
`SPIRA_GATE_LOCK_WAIT` instead of always computing `RESERVE/10` — remains
worthwhile. It lets the lock wait be sized independently of the pass budget,
which is the right knob for future tuning, without touching MAXSEC.

## Recovery

- `timed.sh` and `timed.out` are at `/home/ryan/.claude/jobs/59b3ad45/tmp/`
- Cold-gate measurements: same dir, `gatewatch.out`
- Post-cheapgate measurements: `cheapgate.out`
