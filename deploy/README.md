# PokeDumpster Deployment

Rootless Podman + systemd. No GitHub Actions — all build / test / deploy
logic lives in these scripts. No sudo required after the one-time Podman
install. Each instance is a separate repo clone with its own image, data
volume, and host port.

The intent is a **super tight local CI loop now, expanding to GitHub
later** — see [Expanding to GitHub](#expanding-to-github-later).

## Prerequisites

### Linux (one-time, needs sudo)

```bash
sudo apt install podman sqlite3
loginctl enable-linger "$USER"     # keeps --user services alive after logout
```

### macOS (one-time)

```bash
brew install podman
podman machine init --memory 4096 --cpus 4
podman machine start               # also needed after each reboot
```

The Podman machine (a lightweight Linux VM) persists across reboots but must
be started after each reboot. macOS has no systemd, so use the `deploy/mac-*.sh`
scripts instead of `setup.sh` / `deploy.sh` / `teardown.sh`.

## Conventions

| Thing | Value |
|---|---|
| Service / unit name | `pkdump-<instance>` |
| Container name | `systemd-pkdump-<instance>` (Linux), `pkdump-<instance>` (macOS) |
| Image tag | `pkdump:<instance>` (alias of `pkdump:latest`) |
| Data volume | `pkdump-<instance>-data` |
| Container port | 8080 (host port auto-assigned unless given) |
| Data dir in container | `/data` (`PKDUMP_HOME=/data`) |
| Default instance | `prod` |

Two SQLite databases live on each data volume:

- `shared.sqlite` — the immutable card catalog. Fully reproducible from
  upstream via `pkdump setup`; **not** backed up.
- `collection.sqlite` — the per-user collection. The only thing worth
  backing up.

## Local CI loop

`deploy/ci.sh` is the inner dev cycle. It runs everything a CI service
would, as a plain re-runnable script:

```bash
bash deploy/ci.sh
```

Steps, in order, exiting non-zero on the first failure:

1. Tear down any stale `ci` instance.
2. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
3. Frontend: `npm ci && npm run check && npm run build`.
4. Build and start a `--test` container, wait for the server to answer on
   its port, then tear it down.
5. Intents UI harness (`tests/ui`, Playwright) — run if browsers are
   installed, otherwise skipped with a message. Some scenarios need
   `ANTHROPIC_API_KEY`; without it, Vision-mode scenarios may skip.

Install Playwright browsers once to enable step 5:

```bash
( cd tests/ui && npx playwright install chromium )
```

## Seed volume (one-time, speeds up future instances)

Build a reusable `pkdump-seed-data` volume so `setup.sh --init` clones it in
seconds instead of re-downloading the catalog:

```bash
bash deploy/seed.sh --volume            # runs `pkdump setup` once
bash deploy/seed.sh --volume --force    # recreate after schema changes
```

## Instances

### Stable deployment (`prod`)

```bash
git clone <repo-url> /opt/pokedumpster-prod
cd /opt/pokedumpster-prod
bash deploy/setup.sh prod 8080
bash deploy/seed.sh prod                # populate the catalog
systemctl --user start pkdump-prod
```

### Feature / test instances

Each instance runs from its own checkout on any branch. Host port is
auto-assigned if omitted.

```bash
git clone <repo-url> ~/workspace/pkdump-feature-xyz
cd ~/workspace/pkdump-feature-xyz
git checkout feature-xyz

# Fast: seed the data volume from the committed fixture (offline, ~seconds)
bash deploy/setup.sh feature-xyz --test
systemctl --user start pkdump-feature-xyz

# Or clone the pre-built seed volume (run `seed.sh --volume` first)
bash deploy/setup.sh feature-xyz --init
systemctl --user start pkdump-feature-xyz

# Rebuild + restart after code changes
bash deploy/deploy.sh feature-xyz

# Clean up
bash deploy/teardown.sh feature-xyz             # keeps data volume
bash deploy/teardown.sh feature-xyz --purge     # removes everything
```

## Scripts

| Script | Purpose |
|---|---|
| `ci.sh` | Local CI loop — Rust + frontend gates, test container, intents harness |
| `seed.sh <instance>` | Populate one instance's catalog in place |
| `seed.sh --volume [--force]` | Build the reusable `pkdump-seed-data` volume |
| `setup.sh <name> [port] [--init] [--test]` | Create an instance. `--test` seeds from the committed fixture; `--init` clones the seed volume |
| `deploy.sh <name>` | Rebuild image and restart one instance |
| `teardown.sh <name> [--purge]` | Stop and remove an instance; `--purge` deletes the data volume |
| `backup.sh [instance] [user]` | Online `sqlite3 .backup` snapshot of `<user>.sqlite` |
| `restore.sh [--yes] <file> [instance] [user]` | Restore a `<user>.sqlite` snapshot |
| `mac-setup.sh` / `mac-deploy.sh` / `mac-teardown.sh` | macOS equivalents (no systemd) |

## Systemd timers

`setup.sh` installs two templated `--user` units alongside the instance:

- `pkdump-backup@<instance>` — nightly `sqlite3 .backup` of the per-user
  collection DB (via `backup.sh`), 02:00 + jitter.
- `pkdump-refresh@<instance>` — nightly `pkdump data refresh` inside the
  running container (via `podman exec`), 06:00 + jitter.

The units are `%i`-templated, so one copy serves every instance. The
instance name is the part after `@`. Enable them per-instance:

```bash
systemctl --user enable --now pkdump-backup@prod.timer
systemctl --user enable --now pkdump-refresh@prod.timer

systemctl --user list-timers 'pkdump-*'        # check schedule
journalctl --user -u pkdump-backup@prod.service # check last run
```

`backup.sh` resolves its repo clone from the unit's `WorkingDirectory`,
which defaults to `~/pokedumpster-<instance>`. If your `prod` clone lives
elsewhere (e.g. `/opt/pokedumpster-prod`), edit the `WorkingDirectory` /
`ExecStart` paths in `pkdump-backup.service` before enabling the timer, or
drop in a `systemctl --user edit pkdump-backup@prod` override.

`teardown.sh` disables and removes the per-instance timers.

## Backup & restore

```bash
# Manual backup (the nightly timer runs this automatically)
bash deploy/backup.sh prod

# Restore a snapshot
bash deploy/restore.sh ~/pkdump-backups/prod/daily/pkdump-prod-20260519-020000.sqlite prod
```

Backups land under `~/pkdump-backups/<instance>/` (override with
`PKDUMP_BACKUP_DIR`) with `daily/` (7), `weekly/` (8), `monthly/` (12)
retention tiers. Only `collection.sqlite` is backed up — the shared catalog
is reproducible with `pkdump setup`.

## Expanding to GitHub later

There is intentionally **no `.github/workflows/`** directory. When CI moves
to GitHub, the workflows should be thin wrappers that call these scripts on
a self-hosted runner — all real logic stays here so the laptop loop and CI
behave identically:

```yaml
# .github/workflows/ci.yml  (sketch — not committed)
jobs:
  ci:
    runs-on: [self-hosted, linux]
    steps:
      - uses: actions/checkout@v4
      - run: bash deploy/ci.sh

# .github/workflows/deploy.yml  (sketch — not committed)
jobs:
  deploy:
    runs-on: [self-hosted, linux]
    steps:
      - run: git -C /opt/pokedumpster-prod pull --ff-only
      - run: bash /opt/pokedumpster-prod/deploy/deploy.sh prod
```

The `ci` instance name and `--purge`-on-exit behavior in `ci.sh` already
make it safe to run repeatedly on a shared runner.

## Troubleshooting

```bash
systemctl --user status pkdump-<name>
journalctl --user -u pkdump-<name> -f
podman port systemd-pkdump-<name> 8080/tcp
podman exec -it systemd-pkdump-<name> sh
podman volume inspect pkdump-<name>-data
```
