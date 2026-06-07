---
name: deploy
description: Set up, deploy, seed, restore, or tear down a PokeDumpster container instance via the deploy/ scripts.
user-invocable: true
disable-model-invocation: false
argument-hint: "<action> <instance> [port]   (action: setup | deploy | seed | restore | teardown | status)"
---

# Deploy

Front door to PokeDumpster's rootless-Podman + systemd deployment. Every
action delegates to a script in `deploy/` — this skill chooses the right
one, runs it, and reports. It never reimplements deploy logic.

An *instance* is a named deployment (`prod`, a feature branch, `ci`). Each
has a Quadlet unit `pkdump-<instance>.container`, a data volume
`pkdump-<instance>-data`, and a systemd-user service `pkdump-<instance>`.

## Arguments

`$ARGUMENTS` is `<action> <instance> [port]`. If the action is omitted,
infer it: no instance dir → `setup`; otherwise `deploy`. Default instance
is `prod`.

| Action | Script | What it does |
|---|---|---|
| `setup` | `deploy/setup.sh <instance> [port]` | Build the image, install the Quadlet unit + timers. Add `--test` to seed the data volume from `tests/ui/fixtures/` (offline), or `--init` to clone the seed volume. |
| `seed` | `deploy/seed.sh <instance>` | Populate the catalog by running `pkdump setup` in a one-off container. `deploy/seed.sh --volume [--force]` builds the reusable seed volume instead. |
| `deploy` | `deploy/deploy.sh <instance>` | Rebuild the image and restart the instance (delegates to `setup.sh` if the instance does not exist). |
| `restore` | `deploy/restore-litestream.sh <instance>` | Restore the collection from the S3 backup (latest, or `--at=<RFC3339>` for point-in-time). Stops, restores, restarts, verifies. See `deploy/RESTORE.md`. |
| `teardown` | `deploy/teardown.sh <instance> [--purge]` | Stop and remove the instance. `--purge` also deletes the data volume. |
| `status` | — | Report instance health (see below). |

## How to run

### 1. Parse `<action> <instance> [port]`

### 2. Pre-flight
- `podman` must be on PATH. If not: `sudo apt install podman` — report and stop.
- For `setup`/`deploy`: you must be inside the repo clone (the scripts build from `Containerfile`).
- Warn if linger is off (`loginctl show-user "$USER" -p Linger`) — services stop at logout without `loginctl enable-linger $USER`.

### 3. Run the script

Image builds take minutes — run `setup`/`deploy` in the **background** and
monitor the output file. Quick actions (`teardown`, `restore`, `status`) run
in the foreground.

### 4. After `setup`

A fresh instance has no catalog. Tell the user the next steps the script
prints: `deploy seed <instance>` (or it was a `--test`/`--init` setup, which
is already seeded), then `systemctl --user start pkdump-<instance>`.

### 5. `status`

Report, for the instance:

```bash
systemctl --user status pkdump-<instance> --no-pager
podman port systemd-pkdump-<instance> 8080/tcp     # the host port
podman ps --filter name=pkdump-<instance>
```

Then curl the port to confirm the server answers.

### 6. Report

State the action taken, the instance, its host port and health, and the
single most useful next command (start it, seed it, view logs with
`journalctl --user -u pkdump-<instance> -f`).

## Key rules

- **`--purge` and `restore` are destructive** — they delete or overwrite a
  data volume (a real collection). Confirm with the user before purging or
  restoring anything other than a throwaway `ci`/feature instance.
- Never build for `prod` from a dirty or non-default branch without saying so.
- `deploy/ci.sh` is its own thing — for the test loop use `/run-tests`, not
  this skill.
- The `pkdump-refresh` timer (nightly catalog refresh) is installed by
  `setup.sh`; enable it with
  `systemctl --user enable --now pkdump-refresh@<instance>.timer`. Backups are
  the Litestream sidecar (S3), not a timer — recovery via `deploy/RESTORE.md`.
