---
name: fix-ci
description: Diagnose and fix a failing CI run. CI failures are your responsibility to root-cause — never a flake.
user-invocable: true
disable-model-invocation: false
argument-hint: "[output-file | nothing — re-runs deploy/ci.sh]"
---

# Fix CI

## Read this first — the mindset

PokeDumpster's CI is the **local loop**: `bash deploy/ci.sh`, run on this
machine (a future GitHub workflow will be a thin wrapper that calls the same
script — the diagnosis below applies identically once that exists).

- **Every CI failure is your responsibility.** Local state you — or an
  earlier session — created causes failures: a full disk, leftover
  containers, occupied ports, a stale `pkdump-ci` instance, a wedged
  systemd-user unit.
- **There are no flakes here.** A timeout, an OOM, a "network" error — on a
  local runner that is almost always a bad local state.
- **Never re-run hoping it passes.** Re-running before diagnosing is
  forbidden; the same broken state yields the same failure.
- **Diagnose deeply, then act.** Quote the actual failing log line, trace it
  to a system-level cause, fix that.
- If you catch yourself typing "transient", "flake", or "let's just retry" —
  stop. You're wrong. Go diagnose.

## Arguments

`$ARGUMENTS` — optional path to a saved `deploy/ci.sh` output file. If
absent, you will produce a fresh failure by running `deploy/ci.sh` once
(this is the one allowed run — to *capture* the failure, not to "retry").

## Phase 1 — Get the failure

If given an output file, read it. Otherwise run `bash deploy/ci.sh`
(background, no pipes) and read its output file once it exits non-zero.

Identify which `==>` step failed: a Rust gate (`cargo test` / `cargo clippy`
/ `cargo fmt`), the frontend gate, the container gate, or the intents
harness.

## Phase 2 — Read the failure carefully

Quote the **specific error line(s)** before doing anything. Common patterns
and what they actually mean:

| Error pattern | Real cause (almost always) |
|---|---|
| `error[E…]` / `error: …` from `cargo` | A genuine code failure — fix the code, not the system |
| `cargo clippy` warning denied by `-D warnings` | A real lint — fix it |
| `cargo fmt --check` diff | Run `cargo fmt` |
| `npm ci` `ETIMEDOUT` / `ENOSPC` | Disk full or thrashing, not network |
| `no space left on device`, `io: …closed pipe` mid `podman build` | Disk full |
| `cannot allocate memory`, `OOMKilled`, killed at random | Memory pressure |
| `address already in use`, `bind: address in use` | Stale container/process holding the port |
| `container with name … already exists` | A previous `pkdump-ci` instance not torn down |
| `database is locked` | A leftover instance holding the SQLite DB |
| server "failed to start within timeout" | Quadlet unit broken, image bad, or stale systemd-user state |
| Playwright "browser not found" | Not a failure — `ci.sh` skips the harness when browsers are absent; if it errored, investigate |

Write down which row the failure matches.

## Phase 3 — Inspect local state

Run these one at a time (never `&&`-chained):

```bash
df -h /home /tmp                                   # disk
free -h                                            # memory + swap
podman ps -a                                       # containers, incl. stopped
podman volume ls                                   # volumes
systemctl --user list-units 'pkdump-*' --all       # quadlet units
ss -ltnp | grep -E '8080'                          # ports
```

When disk is tight (>85%), check PokeDumpster's known accumulation points:

```bash
du -sh /home/ryangantt/pokedumpster/target              # cargo build artifacts — often many GB
du -sh /home/ryangantt/pokedumpster/screenshots         # intents-harness screenshots (gitignored)
du -sh /home/ryangantt/pokedumpster/frontend/node_modules
du -sh /home/ryangantt/pokedumpster/tests/ui/node_modules
du -sh /home/ryangantt/.local/share/containers          # podman storage
du -sh /home/ryangantt/.cargo/registry                  # cargo registry cache
```

Known culprits:
- **`target/`** — cargo artifacts; `cargo clean` reclaims it (costs a rebuild, nothing else).
- **`screenshots/ui/`** — the intents harness leaves a directory per run; gitignored, safe to delete.
- **Stale `pkdump-ci` container/volume** — a CI run that did not tear down. `bash deploy/teardown.sh ci --purge`.
- **podman storage** — `podman system prune` (NOT `-a` without user OK — it removes images they may want).

## Phase 4 — Form a hypothesis, confirm it

Tie Phase 2's error to Phase 3's state in one sentence: "CI failed at step X
with error Y, caused by local condition Z (evidence: …)". If you cannot
connect them, dig deeper — re-read the log, check
`journalctl --user -u pkdump-ci --since '1 hour ago'`. Do not proceed
without a single, evidence-backed root cause.

If the root cause is **a real code/test/lint failure**, that is not a
"system" problem — fix the code. That is still your job.

## Phase 5 — Fix the root cause

Apply the minimal fix. Examples:
- Disk full from screenshots: `rm -rf /home/ryangantt/pokedumpster/screenshots/ui`
- Disk full from `target/`: `cargo clean`
- Stale ci instance / occupied port: `bash deploy/teardown.sh ci --purge`
- Wedged unit: `systemctl --user reset-failed pkdump-ci`
- A real cargo/clippy/fmt failure: fix the code; for fmt, `cargo fmt`.

**Confirm before destructive cleanup beyond gitignored artifacts** —
deleting podman volumes (especially a prod data volume), `podman system
prune`, removing anything you did not create. Gitignored screenshots and
`target/` do not need confirmation.

Verify the fix landed (`df -h`, `podman ps -a`, etc.) before re-running.

## Phase 6 — Re-run and watch

```bash
bash deploy/ci.sh
```

If it fails again, **do not call it a flake**. Return to Phase 2 with the
new log. Iterate until CI is genuinely green.

## Phase 7 — Prevent recurrence

If accumulated state caused it, decide whether the workflow that creates
that state should clean up after itself — `deploy/teardown.sh`, a skill, a
`.gitignore` entry, or a note to the user about a new accumulation point.
Don't fix silently and walk away.

## Hard rules

1. Never re-run CI before diagnosing.
2. Never use the word "flake" — the runner is local; the cause is local.
3. Never stop at "podman crashed" / "network timed out" — find the cause.
4. Never delete outside gitignored artifact dirs without confirming.
5. Always quote the actual failing log line in your report.
6. Always verify the fix before re-running.
7. A real code/test/lint failure is a real bug — fix it, don't explain it away.
8. Stuck after a genuine diagnosis attempt? Ask the user. Don't guess.

## Reporting back

Tell the user: which step failed, the exact line that revealed the cause,
the one-sentence root cause, what you changed, the resulting system state,
whether the re-run is green, and any prevention note.
