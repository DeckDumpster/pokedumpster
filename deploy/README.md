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

Steps, in order, exiting non-zero on the first failure of a sequential step
(the container gates below run in parallel — see
[The container gates run in parallel](#the-container-gates-run-in-parallel)):

0. Pick the container store (see [Container storage](#container-storage)) and
   refuse to start if either disk is under the floor.
1. Tear down any stale `ci` instance.
2. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
   Then `tests/deploy/run.sh` — the store resolution and the low-disk guard.
3. Frontend: `npm ci && npm test && npm run check && npm run build`.
   Then the shipped image, built **once** — five gates below need it and each
   wants its own tag, so they tag this one (see
   [The image, once](#the-image-once)).
4. Start a `--test` container and wait for the server to answer on its port.
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

### The container gates run in parallel

Eleven of those steps — litestream, drill, alarming, recreate, upgrade,
tenant-header, schema-version, the three lake gates and refresh — stand up
their own containers and share nothing. Every name each of them uses (network,
container, volume, image tag, unit prefix, temp dir) is already derived from
its own prefix plus a hash of the checkout path, because concurrent polecats
have run whole CI suites beside each other for months. That isolation is what
makes running them at the same time a scheduling change and not a correctness
one.

So they do not run where they are written. Each **queues** itself under its own
tier guard and the last step runs the queue, two at a time
(`deploy/ci-parallel.sh`).

```bash
PKDUMP_CI_JOBS=1 bash deploy/ci.sh   # one at a time — first thing to try if a
                                     # parallel run misbehaves
```

Measured, two full green runs back to back on the CI box, same checkout and the
same warm caches, the cap the only difference:

| | cap 1 | cap 3 | |
|---|---|---|---|
| the eleven gates | 1683s | 728s | 2.31x |
| the whole suite | 1982s | 1175s | 1.69x |

Individual gates get **slower** — `prices` 141s → 371s, `value-snapshots`
93s → 257s — because three of them share four cores. The wave still finishes in
43% of the time, which is the argument: these gates are latency, not
throughput. They spend their time waiting on containers to come up, replicate
and stop, and sequentially that waiting is most of a CI run's wall clock.

Both runs sat on a shared box under a 1-min load between 2 and 16, and the cap-1
baseline drew the quieter half of that, so those ratios are a floor.

Three by default, **four at the very most**. This is a 15G box with four cores
that also runs prod, and each of these gates stands up two or three containers
— a MinIO, sometimes a JVM (Nessie), sometimes a whole pkdump instance with a
Litestream sidecar. Above four the failures stop looking like resource
exhaustion and start looking like flaky gates, which is the worst possible
outcome for a suite whose job is to be believed. A `PKDUMP_CI_JOBS` above the
ceiling is clamped, out loud.

**The disk floor is checked before every dispatch**, not once at startup.
Startup was enough when one gate ran at a time and the previous one's teardown
had already returned its space; two at a time can be two images, two
volumes and two MinIO stores deep at once. It is the same
`deploy/diskcheck.sh --floor` guard as everywhere else, over the same two
filesystems, so `PKDUMP_DISK_FLOOR_GB` moves all of them together. Below the
floor with gates still running, the runner **holds** — the thing most likely to
give the space back is the gate that is about to tear itself down. Below the
floor with nothing running, the run fails naming the gates that never started,
rather than filling the disk to find out.

Two things read differently in the log afterwards:

* a parallel gate's output is printed in **one block when that gate finishes**,
  not as it happens, and the blocks come out in completion order. Concurrent
  writers would otherwise shred each other, and a shredded CI log is a gate
  nobody can diagnose;
* a failing gate **no longer stops the ones beside it**. The wave finishes,
  every gate's output is printed under its own name, and the run ends red
  naming all of them. Sequentially you learned about the earliest failure; now
  one red run tells you everything that is broken.

`tests/ci/parallel_test.sh` gates all of it — hermetically, in the lint tier,
in about four seconds: the cap holds and is actually reached, a failure among
passing gates still goes red and is still named, output survives concurrency,
the real `diskcheck.sh` trips against an impossible floor, the hold branch
waits instead of aborting, a background job the *caller* started is never
mistaken for a gate, and every one of the eleven gate scripts is still queued
exactly once under a real tier.

### The image, once

Five gates need the image built from this checkout's `Containerfile`, each
under its own tag so one gate's teardown cannot untag another's: the container
gate and the schema-version gate (both through `deploy/setup.sh`), plus the
upgrade, tenant-header and refresh gates. Every one of them used to run its own
`podman build` over identical content.

`deploy/ci.sh` builds it once and exports `PKDUMP_PREBUILT_IMAGE`;
`deploy/image-lib.sh::pkdump_image_ensure` is what each gate calls, and the
variable decides what that means:

| `PKDUMP_PREBUILT_IMAGE` | what happens |
|---|---|
| unset | `podman build` — prod's path, and a gate run by hand |
| set, image present | `podman tag` — no builder runs |
| set, image absent | refuses, naming the image |

The last row is deliberate. A silent rebuild would turn "the build-once wiring
broke" into "CI got slower again", which is a regression nobody files.

The saving is smaller than it looks: podman's layer cache already made repeat
builds ~4s rather than free (measured — first build after a Rust change 323s,
the next two 4s each). What this removes is the *dependence* on that cache. One
`podman image prune`, one store teardown or one cold box and those four repeats
become four 5-7 minute compiles (`podman build --no-cache`, warm cargo mount
cache: 447s).

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
reset, so a store-teardown command has to use this recipe. One does now — the
next section — and the steps above remain what it runs.

#### Removing a store, with one command

`teardown.sh` removes an *instance* and leaves the store standing, because the
store is shared by every instance on the box. Nothing removed the store itself,
so one accumulated forever — 3.9G of images and layers, plus a runroot per store
under `/run/user/$UID`.

```bash
bash deploy/store-teardown.sh    # the store store.env names; refuses if there is none
```

It runs exactly the recipe above — stop, remove, prune, then `rm -rf` the store
root and its runroot — plus the store's rootless-netns name. With no alternate
store configured it exits non-zero rather than defaulting to Podman's; that one
is prod's. It reports a failure rather than claiming success when something in
the store is still mounted.

#### Two stores, one rootless netns (pd-yfev)

Podman 4.9 does not fully support two rootless stores on one login, and the way
it fails is silent and total: the alternate store reaches a state where *every*
container on a user-defined network dies with

```
Error: failed to mount runtime directory for rootless netns: no such file or directory
```

Each store gets its own netns file (named from a hash of its graph root) but they
share one scaffolding directory, `$XDG_RUNTIME_DIR/libpod/tmp/rootless-netns` —
`--root`/`--runroot` do not move it. `RootlessNetNS.Cleanup()` deletes that shared
directory when the last bridge-network container *in its own store* exits, and it
counts containers out of its own store's database, so it cannot see the other
store's. The other store is then left holding a netns file that still looks valid
and mounts into nothing, permanently.

`tests/litestream/run.sh` and `drill.sh` both create a user-defined network, so a
wedged store means `deploy/ci.sh` cannot pass — and it wedges mid-session, from
another store's cleanup, with nothing in the message to suggest the store.

`pkdump_store_activate` repairs it: if this store's netns file is present while
the shared scaffolding is gone, the file is stale and is dropped, which puts
podman back on the branch that rebuilds it. Deliberately *not* `podman system
migrate` (the repair found by hand first) — that kills the pause process, which
is per-user and shared with the store prod runs in.

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
| `backup-check.sh <inst> [user]` | Layer 1 — verify every S3 replica (tenants on freshness, the registry on correspondence), ping the off-box monitor (run by the `pkdump-backup-check@` timer). The verification always runs; the ping URL controls only the ping |
| `alarm-status.sh <inst> [--verify]` | Is alarming actually ARMED on this instance? Exit 0 = yes. `--verify` fires it for real |
| `diskcheck.sh` | Layer 4 — push a Pushover alert when the disk crosses the threshold (run by `pkdump-diskcheck.timer`) |
| `diskcheck.sh --floor [path...]` | Gate — exit non-zero under `PKDUMP_DISK_FLOOR_GB` free; run by `ci.sh` before it builds |
| `setup-lake.sh <inst> [--port N] [--remove]` | Install the offline lakehouse — the Nessie catalog's Quadlet units and the PyIceberg job image. Refuses to run without `~/.config/pkdump/lake.env`. See [Offline lakehouse](#offline-lakehouse--nessie--iceberg) |
| `store-lib.sh` | Sourced — resolves which Podman store an instance's image and volume live in (`PKDUMP_STORE_ROOT`) |
| `units-lib.sh` | Sourced — renders every unit template this checkout ships into `~/.config`, preserving the instance's published port. Shared by `setup.sh` and `deploy.sh` so a deploy cannot ship a binary and leave the units behind (pd-2t6u) |
| `alert.sh "<title>" ["<msg>"]` | Shared Pushover sender used by every alarming layer (message also accepted on stdin); trims to the first 900 bytes |
| `journal-summary.sh <unit>` | Layer 2 — turn a failed unit's journal tail (on stdin, or fetched when run by hand) into one readable page: cause first, no OCI metadata, no systemd boilerplate |
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

- `pkdump-refresh@<instance>` — nightly `pkdump data refresh`, 06:00 + jitter.
  Runs `deploy/refresh.sh`, which starts its own container from the instance's
  image over the instance's data volume — the same shape as the derive and
  transform wrappers. It used to `podman exec` into the running server, which
  silently dropped the environment the drop-in that turns raw landing on sets
  (pd-kncd); see [deploy/LAKE.md](LAKE.md) §4.
- `pkdump-backup-check@<instance>` — backup-freshness dead-man's switch
  (Layer 1, every 6h). See [Backup-failure alarming](#backup-failure-alarming).
- `pkdump-value-snapshots@<instance>` — the transform tier's nightly run
  (07:00): per-tenant collection value snapshots computed from the lake, for
  **every** registered tenant. `pkdump data refresh` no longer snapshots anybody
  (its step 7 is deleted — pd-hkbc), so this is the only thing that records
  today's value. Ordered `After=pkdump-refresh@%i.service`, and inert until the
  lakehouse is configured. Exit 2 (a tenant skipped) is a partial run, not a
  failure. See [deploy/LAKE.md](LAKE.md) §7.
- `pkdump-diskcheck` — host-wide low-disk alert (Layer 4, daily). Not
  per-instance; enable once.

The `@`-templated units are `%i`-templated, so one copy serves every instance —
the instance name is the part after `@`. Enable per-instance:

```bash
systemctl --user enable --now pkdump-refresh@prod.timer
systemctl --user enable --now pkdump-backup-check@prod.timer   # after arming alerts.env
systemctl --user enable --now pkdump-value-snapshots@prod.timer # after setup-lake.sh
systemctl --user enable --now pkdump-diskcheck.timer           # host-wide, once
systemctl --user list-timers 'pkdump-*'        # check schedule
```

Backups themselves are **not** a timer — the Litestream sidecar replicates
continuously (see below). `teardown.sh` disables the refresh, backup-check and
value-snapshot timers for the instance (the host-wide disk timer is left alone).

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

## Offline lakehouse — Nessie + Iceberg

**Offline only.** Nothing on the serving path touches any of this: the app keeps
reading `shared.sqlite` and tenant SQLite, and a catalog that is down costs a
nightly batch job, not a request. **No tenant data ever enters the lake** — the
lake holds catalog data (prices, products, sets, cards) and nothing keyed by a
tenant, ever. Per-tenant point-in-time recovery is Litestream's job, above.

Two pieces, both instance-scoped:

- **`pkdump-nessie-<inst>`** — the versioned Iceberg catalog. The one JVM
  service in this system, treated as a black box; our jobs speak the Iceberg
  REST API to it at `/iceberg/`. Version store is **ROCKSDB on a host
  directory**, not a podman volume: a rootless volume lives in the container
  store, and this box's default store is the 98 G disk prod runs from.
- **`localhost/pkdump-lake:<inst>`** — the job runtime, `lake/` in this repo.
  PyIceberg, no JVM. Built by `setup-lake.sh`.

```bash
bash deploy/setup-lake.sh prod                 # unit + network + job image
systemctl --user start pkdump-nessie-prod
journalctl --user -u pkdump-nessie-prod -f

# The round trip, against the live catalog (writes to the `proof` namespace only):
podman run --rm --network pkdump-lake-prod \
  -e PKDUMP_LAKE_NESSIE_URI=http://pkdump-nessie-prod:19120/iceberg/ \
  localhost/pkdump-lake:prod pkdump-lake-roundtrip

bash deploy/setup-lake.sh prod --remove        # unit + network; state is kept
```

`setup-lake.sh` **refuses to run without `~/.config/pkdump/lake.env`** — host
config beside `alerts.env`, `litestream.env` and `store.env`:

```bash
PKDUMP_LAKE_S3_BUCKET=<bucket>        # NOT the Litestream backup bucket
PKDUMP_LAKE_S3_REGION=us-west-2
AWS_PROFILE=pkdump                    # assume-role profile, same as Litestream
PKDUMP_LAKE_NESSIE_DATA=/workspaces/pkdump-lake/nessie
```

The bucket is **separate from the Litestream backup bucket** — same account,
same `AWS_PROFILE=pkdump` role path, different bucket. The backup bucket holds
the only irreplaceable data in the system and everything in the lake is
reproducible by construction, so a lifecycle rule written for one must not be
able to reach the other. `setup-lake.sh` fails if the two names match.

**There is no lifecycle rule on `raw/`, deliberately.** Indefinite retention was
measured, not assumed: ~4.1 MB/day compressed, 1.5 GB/year, ~$0.03/month in year
one — cheaper than losing the ability to rebuild a date. Do not tidy it up.

**The catalog has no authentication.** Nessie says so in its own startup log.
It publishes on `127.0.0.1` only and the jobs reach it by name over the
`pkdump-lake-<inst>` podman network. Do not publish it on `0.0.0.0`.

Measured on this box by `tests/lake/run.sh` (Nessie 0.104.3, PyIceberg 0.11.1):

| | |
|---|---|
| Nessie RSS | **265 MiB** under a 1 GiB container cap (`PodmanArgs=--memory=1g`) |
| Object cache | pinned to 64 MB — **unpinned it claimed 6.7 GB** on this box, sized as a fraction of the heap |
| Startup | ~29 s under the cap (~6 s uncapped) — hence `TimeoutStartSec=120` |
| Version store on disk | **146 MB** for a two-commit toy table, of which ~200 KB is content: the rest is RocksDB WAL preallocation, so it is a floor rather than growth |

### `catalog.prices` — the first table, built from `raw/` alone

```bash
podman run --rm --network pkdump-lake-prod \
  -e PKDUMP_LAKE_NESSIE_URI=http://pkdump-nessie-prod:19120/iceberg/ \
  -e PKDUMP_LAKE_S3_BUCKET=<bucket> -e PKDUMP_LAKE_S3_REGION=us-west-2 \
  localhost/pkdump-lake:prod \
  pkdump-lake-build-prices --ingest-date 2026-08-11
```

One row per price actually quoted, at grain `(tcgplayer_product_id,
sub_type_name, price_type, observed_date)`, partitioned by `observed_date`.
The date is **required and never taken from the clock** — rebuilding an old
day is the same operation as building today — and the build replaces that
day's partition in one commit, so re-running is a replacement rather than a
doubling.

**It never calls an upstream.** That is the whole claim the landing zone is
there to support, so `tests/lake/prices.sh` runs the job on a podman
`--internal` network and proves the network is dead before trusting anything
the job says. Full runbook, including what happens when a day landed no
complete run: [LAKE.md](LAKE.md).

### Time travel, and what Nessie costs to get it

Iceberg + Nessie is deliberately overkill at this data size; **time travel is
the primitive being bought**, so `tests/lake/run.sh` asserts it rather than
assuming it — write, read, commit again, then read the table as of the first
commit. Two findings from standing it up (pd-fzeb), both measured:

- **Catalog-level time travel works.** `main@<commit-hash>` addresses every
  table at once, which is the single-value provenance handle a published
  artefact records.
- **Per-table Iceberg snapshot travel does not survive Nessie.** The metadata
  Nessie hands a client carries **only the current snapshot**, so
  `scan(snapshot_id=…)` raises `Snapshot not found` for any earlier one. The
  same two commits through PyIceberg's service-free SQL catalog keep both
  snapshots and travel fine. Nessie does not add history to Iceberg's — it
  **replaces** it. Worth knowing before anything depends on a snapshot id.

Whether Nessie is needed *yet* is an open recommendation, not a settled
decision — see `pd-by3x`.

## Backup-failure alarming

Motivated by a Jun 2026 incident where the (then local) backup unit failed every
night for ~11 days with nobody watching, and a key rotation that left the
Litestream sidecar showing systemd `active` while silently *not* replicating.
**Liveness is not freshness** — the monitor verifies that data actually lands in
S3, not just that the service is up. Defense in depth (pokedumpster-ivq):

- **Layer 1 — replication dead-man's switch (primary).** `backup-check.sh` runs
  every 6h (`pkdump-backup-check@<inst>.timer`), asks S3 about every replica,
  and pings an **off-box** monitor (healthchecks.io) only when they all pass. A
  broken-creds / stalled / dead-box / disabled-timer state stops the pings → the
  monitor alerts. This is the layer that catches the silent modes. It also writes
  a `.backup-last-ok` marker for Layer 3.

  **Two databases, two different questions** (pd-me6h). A *tenant* collection
  changes daily, so it is judged on FRESHNESS: a replica whose newest write is
  older than `PKDUMP_BACKUP_MAX_AGE_HOURS` (36h) has stopped. The *user
  registry* is static by design — handle → database_id changes only when a
  tenant is added, removed or renamed, legitimately months apart — so a replica
  with no new objects means nothing is wrong, and freshness is simply the wrong
  question. It is judged on CORRESPONDENCE instead: Litestream's local txid for
  the registry against the furthest txid its replica holds. That passes while
  the two agree however old the last write is, and fails the moment the replica
  falls behind or is missing. Do not "fix" a registry alarm by raising the
  threshold — that was the false positive, and a bigger number only moves it.

  A lag is re-asked for up to `PKDUMP_BACKUP_CORRESPONDENCE_GRACE_SECONDS`
  (90s) before it counts. Litestream can hold an un-uploaded checkpoint across a
  transient S3 error and clear it at its next compaction tick ~30s later
  (measured, `tests/alarming/run.sh` §4b); a replica that has genuinely stopped
  never catches up, so the window only costs the run that would have paged over
  a blip.

  The checker is READ-ONLY on both sides: S3 is only ever listed, and the data
  volume is mounted `:ro` for the one command that reads local state.
- **Layer 2 — `OnFailure` push.** The Litestream sidecar, the refresh run, and
  the backup-check itself fire `pkdump-alert@.service` on failure, pushing the
  failed unit's journal tail to Pushover. Catches hard crashes fast; does *not*
  catch never-ran (that's Layer 1). The tail goes through `journal-summary.sh`
  first — see [Reading the page](#reading-the-page).
- **Layer 4 — low-disk alert.** `diskcheck.sh` (daily, host-wide) pushes when the
  disk crosses `PKDUMP_DISK_THRESHOLD` (default 90%).
- **Layer 3 — in-app banner.** The app shows a staleness banner when the
  `.backup-last-ok` marker goes old (`/api/backup-status`). Passive visibility;
  no paging.

### Reading the page

A page that arrives and says nothing is a page that did not fire. Layer 2 spent
its whole 900-byte budget on the wrong end of the journal for its first weeks in
service (pd-pwk8): `alert.sh` kept the LAST 900 bytes, the last lines of a failed
unit's journal are systemd's own boilerplate, and above those a podman-backed
unit has podman's event log — a container id and every OCI label on the image.
The line that said what went wrong was in there, and it was not what you saw.

So `pkdump-alert@.service` pipes the tail through `journal-summary.sh` before
`alert.sh`, and the budget now buys:

```
pkdump-backup-check@prod FAILED (exit 1) — backup-check: STALE — the user
registry: newest S3 replica write is 66h old (> 36h threshold)

earlier:
level  min_txid          max_txid          size  created
0      0000000000000003  0000000000000003  2595  2026-08-09T21:19:15Z  (x9)
```

The manager's boilerplate and podman's event log are dropped, the service's own
stdout/stderr is kept, the exit status becomes a suffix rather than the body,
and the newest line that reads like a failure leads — the sidecar prints a
heartbeat every second, so "the last line" is not the same thing as "the cause".
A run of near-identical lines collapses to one, counted. A unit that failed
without printing anything still gets a page naming it and how it failed.
`alert.sh` now trims to the FIRST 900 bytes for the same reason.

To see the page a unit would produce right now, without sending it:

```bash
bash deploy/journal-summary.sh pkdump-backup-check@prod.service
```

`tests/alarming/journal_summary_test.sh` (hermetic, sub-second, run by `ci.sh`)
asserts the content against journal tails captured from the real units,
including the 2026-08-12 failure verbatim.

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
ping URL in both directions to hold the pd-1717 fix in place. §6 fires two
failing units — a plain one and a podman-backed one — and asserts the push
carries the causal line and no OCI metadata (pd-pwk8). Nothing it does touches
`pkdump-*@prod`: its units live under their own name prefix, and both external
endpoints resolve to `127.0.0.1`.

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
