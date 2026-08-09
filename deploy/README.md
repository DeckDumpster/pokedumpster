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

Three SQLite databases live on each data volume:

- `shared.sqlite` — the immutable card catalog. Fully reproducible from
  upstream via `pkdump setup`; **not** backed up. One copy, `ATTACH`ed by
  every tenant.
- `tenants/<database_id>.sqlite` — one collection per tenant, named by an
  opaque ULID and never by the handle of the person whose collection it is
  (`collection` is the original single user, and the ids say nothing about
  that). See [TENANTS.md](TENANTS.md) for provisioning, for the operator step
  that answers **which file is whose**, and for the two migrations — out of the
  old flat `collection.sqlite` layout, and then off handle-named files onto ids.
- `registry.sqlite` — the user registry: handle → `database_id`. At the data
  root, deliberately outside `tenants/` so that directory keeps meaning "one
  file per tenant" exactly.

The last two are the irreplaceable set, and both are replicated by the one
Litestream sidecar. The registry is not an afterthought in that set: without it
the tenant files are present but anonymous. [RESTORE.md](RESTORE.md) restores it
**first** for exactly that reason.

## Local CI loop

`deploy/ci.sh` is the inner dev cycle. It runs everything a CI service
would, as a plain re-runnable script:

```bash
bash deploy/ci.sh
```

Steps, in order, exiting non-zero on the first failure:

0. Pick the container store (see [Container storage](#container-storage)) and
   refuse to start if either disk is under the floor.
1. Tear down any stale `ci` instance.
2. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
   Then `tests/deploy/run.sh` — the store resolution and the low-disk guard.
3. Frontend: `npm ci && npm test && npm run check && npm run build`.
4. Build and start a `--test` container and wait for the server to answer on
   its port.
5. Backup gate (`tests/litestream/run.sh`): four tenant databases replicated
   through the shipped Litestream config to a throwaway MinIO, and a
   non-first one restored.
6. DR drill (`tests/litestream/drill.sh`): `deploy/RESTORE.md`'s procedure
   executed with the shipped scripts — one tenant restored in place while the
   others keep exactly their own data.
7. Visual regression (`tests/visual/`): every route at 1440 and 768, diffed
   against the committed baselines.

The intents UI harness (`tests/ui`, Playwright) is deliberately not in the
loop — it needs `ANTHROPIC_API_KEY` for Vision mode. Run it on its own:

```bash
( cd tests/ui && npx playwright install chromium && npx playwright test )
```

## Container storage

Rootless Podman keeps images, layers and volumes under `$HOME`. On the box that
runs `prod`, `$HOME` is on the 98G LVM root that `prod` itself runs from, while
the 938G disk holding the checkouts sits nearly empty — so every throwaway
`--test` instance and every CI image build eats the disk prod depends on. At
100% full a `cargo` link died with `ld terminated with signal 7 [Bus error]`,
which reads as a toolchain bug and not a disk problem.

**Non-prod storage is opt-in-relocatable; prod's never moves.**

```bash
# Build this instance's image and volume into an alternate store:
PKDUMP_STORE_ROOT=/big/disk/pkdump-store bash deploy/setup.sh scratch --test

# Podman's default store (what prod uses, and the default everywhere else):
bash deploy/setup.sh prod 8090
```

- **Which disk is host config, not a repo constant.** Uncomment
  `PKDUMP_STORE_ROOT` in `~/.config/pkdump/store.env` — the same directory
  `alerts.env` and `litestream.env` live in — and `deploy/ci.sh` builds there.
  `setup.sh` scaffolds the file commented out, so the knob is visible on a new
  box without changing anything. An explicit `PKDUMP_STORE_ROOT` in the
  environment wins over the file, and an explicit `PKDUMP_STORE_ROOT=` (empty)
  is how one run opts back out on a box that opts in.
  The store is never *inferred* from the box's disk layout: a rule like "the
  checkout is on a different filesystem from `$HOME`" describes one machine, and
  on any other it quietly starts a container store at the top of whatever
  external drive or network mount the checkout happens to sit on.
- Only `ci.sh` reads `store.env`. `setup.sh` — which is also how prod is
  installed — honours the environment and nothing else, so a host that opts in
  cannot relocate a prod deploy.
- `setup.sh`, `deploy.sh`, `seed.sh` and `teardown.sh` all agree on one store per
  instance: the generated Quadlet unit records it in a `GlobalArgs=` key, and
  `teardown.sh` reads it back, so a bare `deploy/teardown.sh <instance>` removes
  from the store the instance was *created* in.
- Buildah's `--mount=type=cache` contents (the `Containerfile` caches the cargo
  registry and `target/` that way) move with it — that was 6.7G on the prod disk.
- **`prod` never sets the variable**, so prod's generated unit is byte-identical
  to the pre-existing one and prod's volumes never move. `tests/deploy/run.sh`
  asserts exactly that.
- Not covered: `pkdump-refresh@` and `pkdump-backup-check@` are one `%i` template
  shared by every instance, so they cannot carry per-instance store flags. An
  instance in an alternate store is a throwaway — do not enable those timers for
  it.

Mechanism and rationale: [`deploy/store-lib.sh`](store-lib.sh).

#### Deleting a store — never `podman system reset`

Everything above teaches you to aim `--root`/`--runroot` at a second store.
`podman system reset` is the one subcommand that ignores them — it resets podman
storage "back to default state", and on 4.9.3 that included
`/run/user/$UID/libpod`, the rootless SHM lock, and the buildah cache at the
ambient `TMPDIR`, none of which any flag pointed at. Run against a throwaway
probe store, it took `prod` down: HTTP 000 on 8090 and podman answering
`container state improper` while `pkdump serve` was still alive, with other
instances stuck in state `Created` — serving but unmanageable. Data survived and
Litestream never stopped replicating; the damage was runtime state, repaired by
`systemctl --user restart pkdump-<instance>` per affected instance.

Remove a store by removing what it owns and then the directories, all of which
*are* scoped:

```bash
export PKDUMP_STORE_ROOT=/big/disk/pkdump-store
. deploy/store-lib.sh && pkdump_store_activate   # puts the flags on every call
podman stop -a && podman rm -af
podman volume rm -af && podman rmi -af && podman network prune -f
rm -rf "$PKDUMP_STORE_ROOT" "${PKDUMP_STORE_GLOBAL_ARGS##*--runroot=}"
```

The runroot comes off `PKDUMP_STORE_GLOBAL_ARGS` rather than a glob, so this
deletes the runroot belonging to *this* graph root and not another store's. The
`podman` shim lives inside the store root, so the last line takes it with it —
start a new shell rather than trusting the one whose `PATH` now points at
nothing.

`tests/deploy/run.sh` §6 greps `deploy/` and `tests/` and fails on a scripted
reset, so a future store-teardown command has to use this recipe.

### Low-disk guard

`deploy/diskcheck.sh` has two modes off one threshold source:

```bash
bash deploy/diskcheck.sh                    # alert mode — Layer 4 timer, always exits 0
bash deploy/diskcheck.sh --floor /some/path # gate mode — exits 1 under the floor
```

Gate mode is what `ci.sh` runs before it builds anything, on both `$HOME` and the
store root. `PKDUMP_DISK_FLOOR_GB` (default 10) sets the floor. It exists because
running out of room mid-build does not announce itself as a disk problem.

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
| `deploy.sh <name>` | Rebuild image, reinstall the unit files from this checkout, and restart one instance |
| `teardown.sh <name> [--purge]` | Stop and remove an instance; `--purge` deletes the data volume |
| `restore-litestream.sh [--yes] [--at=<RFC3339>] [--unattributed] <inst> [database-id]` | Restore ONE collection from the S3 backup (latest or point-in-time). Addressed by the database's file stem, not by a handle — `pkdump tenant list` says which is whose. **Refuses a database the registry cannot name** (restore `--registry` first; `--unattributed` for a purged one). See [RESTORE.md](RESTORE.md) |
| `backup-check.sh <inst> [user]` | Layer 1 — verify S3 replica freshness, ping the off-box monitor (run by the `pkdump-backup-check@` timer). The verification always runs; the ping URL controls only the ping |
| `alarm-status.sh <inst> [--verify]` | Is alarming actually ARMED on this instance? Exit 0 = yes. `--verify` fires it for real |
| `diskcheck.sh` | Layer 4 — push a Pushover alert when the disk crosses the threshold (run by `pkdump-diskcheck.timer`) |
| `diskcheck.sh --floor [path...]` | Gate — exit non-zero under `PKDUMP_DISK_FLOOR_GB` free; run by `ci.sh` before it builds |
| `store-lib.sh` | Sourced — resolves which Podman store an instance's image and volume live in (`PKDUMP_STORE_ROOT`) |
| `units-lib.sh` | Sourced — renders every unit template this checkout ships into `~/.config`, preserving the instance's published port. Shared by `setup.sh` and `deploy.sh` so a deploy cannot ship a binary and leave the units behind (pd-2t6u) |
| `alert.sh "<title>" ["<msg>"]` | Shared Pushover sender used by every alarming layer (message also accepted on stdin) |
| `mac-setup.sh` / `mac-deploy.sh` / `mac-teardown.sh` | macOS equivalents (no systemd) |

## Systemd timers

`setup.sh` installs these `--user` units alongside the instance, and `deploy.sh`
re-installs them on every deploy — the files under `deploy/` are templates, so
what an instance runs is a copy, and a copy only tracks the repo if something
rewrites it. Until Aug 2026 nothing did on the deploy path: prod's Litestream
sidecar was still the pre-multi-tenant template, missing the `OnFailure=`
alerting the repo had carried since Jun 2026, so the sidecar that silently
stopped replicating paged nobody (pd-2t6u). A deploy now names the units it
changed.

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
continuously replicates **every** `tenants/*.sqlite` **and `registry.sqlite`** to
S3 with **6-month point-in-time recovery**. One sidecar covers all of it — it
watches the `tenants/` directory and derives each tenant's S3 prefix from its
filename, so `pkdump tenant create` is the whole of "add a tenant to backups": no
config edit, no restart. The registry is named explicitly instead, on its own
prefix beside the tenants one. The shared catalog is not backed up (reproducible
via `seed.sh`). Credentials are assume-role (auto-refresh) via a podman secret.

Upgrading an instance whose `litestream.env` predates the registry: re-run
`deploy/setup.sh <inst>` to backfill the two new keys, then restart the sidecar.
Until you do, the sidecar refuses to start — deliberately, so the registry cannot
be silently left out of the replicated set.

```bash
# Restore the latest backup onto a live instance. The argument is the database's
# file stem (an opaque `database_id`), not a person — `pkdump tenant list` maps
# handles to ids. Only that one collection is touched; it defaults to `collection`:
bash deploy/restore-litestream.sh prod
bash deploy/restore-litestream.sh prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC

# The user registry — restore this FIRST after a total loss (RESTORE.md). Not a
# suggestion: a tenant restore refuses until the registry can say whose file it is.
bash deploy/restore-litestream.sh --registry prod

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

### Is it armed?

```bash
bash deploy/alarm-status.sh prod            # read-only; exit 0 = ARMED, 1 = NOT
bash deploy/alarm-status.sh prod --verify   # …then FIRE it: real monitor ping + real push
```

This is the only trustworthy answer to "are the backups alarmed?", and it exists
because every other signal lied. Installed units, present config files and
scripts exiting 0 described a system where **nothing had ever fired**. So the
gates are deliberately strict: a `CHANGE_ME` placeholder is not configured, an
enabled timer that has never completed a run is not armed, and a checker whose
last confirmation is older than the staleness window is not armed. Anything less
than every gate green prints `NOT ARMED`, the reasons, and the commands to fix
it.

`--verify` is the last step of arming rather than part of the check: it runs the
real checker (pinging the real monitor) and sends a real Pushover push, so
"should reach me" becomes "did reach me".

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

# And confirm it end-to-end (sends a real ping and a real push):
bash deploy/alarm-status.sh <inst> --verify
```

Create a healthchecks.io check (period ~6h, grace ~3h) and wire its Pushover
integration. Verify end-to-end: run the check once
(`systemctl --user start pkdump-backup-check@<inst>.service`) and confirm the
monitor goes green, then simulate a failure (e.g. revoke the bootstrap key or
rename the volume) and confirm the alert fires within the grace window.

**There is no "unconfigured" pass.** `backup-check.sh` used to print `skipping`
and exit 0 when `PKDUMP_BACKUP_PING_URL` was empty — a green unit, a green
journal, and no monitor, having asked S3 nothing at all (pd-1717). The skip is
gone: with that variable empty the freshness check still runs and a stale
replica still fails (`pd-7f46`). What is missing is only the off-box dead-man,
so a dead box or a dead timer goes unnoticed — which is a question about
*arming*, and `alarm-status.sh` is what answers it (NOT ARMED, exit non-zero).
`alert.sh` refuses to pass the same way: asked to alert with no credentials, it
exits non-zero rather than dropping the alert quietly.

### Proving it fires

`tests/alarming/run.sh` (run by `ci.sh`) stands up a throwaway instance, a
throwaway MinIO and a local HTTP recorder in place of healthchecks.io and
Pushover, then **makes every layer fire** and asserts on the requests that
arrive: the green heartbeat, the `/fail` trip, the Pushover push and its
journal tail, the low-disk push, and the freshness marker. It also mutates the
ping URL in both directions to hold the pd-1717 fix in place. Nothing it does
touches `pkdump-*@prod`: its units live under their own name prefix, and both
external endpoints resolve to `127.0.0.1`.

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
