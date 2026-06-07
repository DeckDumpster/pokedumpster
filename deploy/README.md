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
| `restore-litestream.sh [--yes] [--at=<RFC3339>] <inst> [user]` | Restore the collection from the S3 backup (latest or point-in-time) — see [RESTORE.md](RESTORE.md) |
| `backup-check.sh <inst> [user]` | Layer 1 — verify S3 replica freshness, ping the off-box monitor (run by the `pkdump-backup-check@` timer) |
| `diskcheck.sh` | Layer 4 — push a Pushover alert when the disk crosses the threshold (run by `pkdump-diskcheck.timer`) |
| `alert.sh "<title>" ["<msg>"]` | Shared Pushover sender used by every alarming layer (message also accepted on stdin) |
| `mac-setup.sh` / `mac-deploy.sh` / `mac-teardown.sh` | macOS equivalents (no systemd) |

## Systemd timers

`setup.sh` installs these `--user` units alongside the instance:

- `pkdump-refresh@<instance>` — nightly `pkdump data refresh` inside the
  running container (via `podman exec`), 06:00 + jitter.
- `pkdump-backup-check@<instance>` — backup-freshness dead-man's switch
  (Layer 1, every 6h). See [Backup-failure alarming](#backup-failure-alarming).
- `pkdump-diskcheck` — host-wide low-disk alert (Layer 4, daily). Not
  per-instance; enable once.

The `@`-templated units are `%i`-templated, so one copy serves every instance —
the instance name is the part after `@`. Enable per-instance:

```bash
systemctl --user enable --now pkdump-refresh@prod.timer
systemctl --user enable --now pkdump-backup-check@prod.timer   # after arming alerts.env
systemctl --user enable --now pkdump-diskcheck.timer           # host-wide, once
systemctl --user list-timers 'pkdump-*'        # check schedule
```

Backups themselves are **not** a timer — the Litestream sidecar replicates
continuously (see below). `teardown.sh` disables the refresh + backup-check
timers for the instance (the host-wide disk timer is left alone).

## Backup & restore — Litestream → S3

Backups are off-box only (no local disk): the `pkdump-litestream-<inst>` sidecar
continuously replicates `collection.sqlite` to S3 with **6-month point-in-time
recovery**. The shared catalog is not backed up (reproducible via `seed.sh`).
Credentials are assume-role (auto-refresh) via a podman secret.

```bash
# Restore the latest backup onto a live instance:
bash deploy/restore-litestream.sh prod

# Point-in-time restore (within the 6-month window):
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod
```

**Full disaster-recovery procedure: [RESTORE.md](RESTORE.md)** — latest restore,
point-in-time, total-box rebuild, verification, and troubleshooting.

## Backup-failure alarming

Motivated by a Jun 2026 incident where the (then local) backup unit failed every
night for ~11 days with nobody watching, and a key rotation that left the
Litestream sidecar showing systemd `active` while silently *not* replicating.
**Liveness is not freshness** — the monitor verifies that data actually lands in
S3, not just that the service is up. Defense in depth (pokedumpster-ivq):

- **Layer 1 — freshness dead-man's switch (primary).** `backup-check.sh` runs
  every 6h (`pkdump-backup-check@<inst>.timer`), lists the S3 replica's
  snapshots, and pings an **off-box** monitor (healthchecks.io) only when the
  newest snapshot is fresh. A broken-creds / stalled / dead-box / disabled-timer
  state stops the pings → the monitor alerts. This is the layer that catches the
  silent modes. It also writes a `.backup-last-ok` marker for Layer 3.
- **Layer 2 — `OnFailure` push.** The Litestream sidecar, the refresh run, and
  the backup-check itself fire `pkdump-alert@.service` on failure, pushing the
  failed unit's journal tail to Pushover. Catches hard crashes fast; does *not*
  catch never-ran (that's Layer 1).
- **Layer 4 — low-disk alert.** `diskcheck.sh` (daily, host-wide) pushes when the
  disk crosses `PKDUMP_DISK_THRESHOLD` (default 90%).
- **Layer 3 — in-app banner.** The app shows a staleness banner when the
  `.backup-last-ok` marker goes old (`/api/backup-status`). Passive visibility;
  no paging.

### Arming it

Secrets never live in the repo — `setup.sh` scaffolds two env files:

```bash
# Host-wide: Pushover creds + disk threshold (Layers 2 + 4, and L1's detail push)
$EDITOR ~/.config/pkdump/alerts.env          # PUSHOVER_TOKEN, PUSHOVER_USER

# Per-instance: the healthchecks.io ping URL (Layer 1)
$EDITOR ~/.config/pkdump/<inst>/alerts.env   # PKDUMP_BACKUP_PING_URL

# Then enable the timers:
systemctl --user enable --now pkdump-backup-check@<inst>.timer
systemctl --user enable --now pkdump-diskcheck.timer
```

Create a healthchecks.io check (period ~6h, grace ~3h) and wire its Pushover
integration. With `PKDUMP_BACKUP_PING_URL` empty, Layer 1 is a no-op (dev/test
boxes are unaffected). Verify end-to-end: run the check once
(`systemctl --user start pkdump-backup-check@<inst>.service`) and confirm the
monitor goes green, then simulate a failure (e.g. revoke the bootstrap key or
rename the volume) and confirm the alert fires within the grace window.

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
