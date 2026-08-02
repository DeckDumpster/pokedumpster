---
name: run-tests
description: Run PokeDumpster's test tiers in order of speed — the tight local dev loop. Stops on the first failure.
user-invocable: true
disable-model-invocation: false
argument-hint: "[tier]   (cargo | lint | frontend | ci | ui | full)"
---

# Run Tests

PokeDumpster's tiered test runner — the tight inner loop. Tiers run fastest
first and **stop on the first failure**. The full local CI loop lives in
`deploy/ci.sh`; this skill is the developer-facing front door to it and to
the faster sub-tiers you want during normal editing.

## Arguments

`$ARGUMENTS` selects a tier. Default (no args) is `cargo` — the tightest loop.

| Arg | Runs | Deps | Time |
|-----|------|------|------|
| `cargo` *(default)* | `cargo test` | none | ~5 s |
| `lint` | `cargo clippy --all-targets -- -D warnings` then `cargo fmt --check` | none | ~15 s |
| `frontend` | `cd frontend && npm test && npm run check && npm run build` | Node | ~15 s |
| `ci` | `bash deploy/ci.sh` — the full local CI loop (Rust gates + frontend + a `--test` container smoke test + intents harness if browsers exist) | Podman | ~3–8 min |
| `ui` | the Playwright intents harness against a running instance | container + browsers + `ANTHROPIC_API_KEY` for vision/generation modes | minutes |
| `full` | `cargo` → `lint` → `frontend` → `ci`, in order | Podman | ~10 min |

## How to run

### 1. Parse the argument

Pick the tier from `$ARGUMENTS`; default `cargo`.

### 2. Pre-flight

- `ci` / `full`: confirm `podman` is on PATH. If not, report and stop.
- `ui`: confirm an instance is running — `podman container exists systemd-pkdump-<instance>` (default instance `ci` or `integration-test`). If none, tell the user to `bash deploy/setup.sh <name> --test && systemctl --user start pkdump-<name>` and stop.

### 3. Run

Run from the repo root. Fast tiers (`cargo`, `lint`, `frontend`) run in the
foreground — they finish in seconds. Slow tiers (`ci`, `ui`, `full`) run in
the **background** so you can monitor:

```bash
bash deploy/ci.sh 2>&1          # tier: ci  — run_in_background: true
```

Never pipe a backgrounded command through `tail`/`grep` — it buffers and
hides progress. Read the output file directly at intervals.

### 4. Monitor slow tiers

While `ci`/`ui` run, read the background output file every ~30 s and report
at natural milestones — when a `==>` step header appears, when the intents
harness starts emitting per-scenario results, or when a failure appears.

### 5. Report

On completion report: which tier, pass/fail counts, wall-clock time, and —
on failure — the exact failing command and error lines. If a tier fails,
**stop**; do not run later tiers (unless `full` was requested, which already
stops internally on first failure). For a failing `ci` run, suggest
`/fix-ci`.

## Key rules

- Default to the `cargo` tier — keep the loop tight; don't spin up a
  container unless asked.
- `cargo test` regenerates the ts-rs TypeScript types in
  `frontend/src/lib/types/`; a dirty git tree there after a run is expected,
  not a failure.
- Stop on first failure. Quote the actual failing line.
- `deploy/ci.sh` is the single source of truth for "does it pass" — `ci`
  and `full` defer to it rather than re-implementing the gates.
