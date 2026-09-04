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
`deploy/diskcheck.sh --floor` guard as everywhere else, over the same list —
copied from `PKDUMP_CI_DISK_PATHS` rather than spelled a second time, so
`PKDUMP_DISK_FLOOR_GB` moves all of them together and neither check can quietly
stop covering a disk. Below the
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
the real `diskcheck.sh` trips against an impossible floor, it measures the temp
filesystem and not only `$HOME`, the hold branch waits instead of aborting, a
background job the *caller* started is never mistaken for a gate, and every one
of the seventeen gate scripts is still queued exactly once under a real tier.

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

### A build collects what the build before it orphaned

Two kinds of image go unreachable every time the builder runs, and neither is
anybody's afterwards: a multi-stage build never tags its **stage** images (the
Rust builder here is 1.62 GB), and re-pointing a tag orphans the image it used
to name. Nothing on the box collected either. Measured: 5.1 GB in prod's default
store from three builds, 3.6 GB in the non-prod store from two CI runs —
roughly 2 GB per build, on the filesystem prod runs from. That is what took `/`
to 91%, and at 91% `deploy/ci.sh` stops at its own disk floor with every
container gate below it unrunnable (pd-h3wy).

`pkdump_image_build_collecting` in `deploy/image-lib.sh` is the builder
invocation every path on this box goes through — `ci.sh`'s single build,
`setup.sh`, `deploy.sh`, `seed.sh` and the lake job image — and the rule is one
sentence: **a build collects what the build BEFORE it orphaned.** (The macOS
scripts still build bare; they run against a podman machine rather than against
the store prod shares, and §11 has always excluded them.)

Not its own orphans. Those hold the layer cache — dropping the last reference
lets podman cascade the intermediates away, and a store measured going 45 MB to
5 MB recompiled from scratch next time. That is the "five compiles instead of
one" regression the section above exists to prevent, arriving disguised as a
disk fix. By the time the next build runs, its own predecessor holds those
layers, so removing the older generation frees only what has genuinely
diverged: over four consecutive builds the store stays flat and the cache hits
do not change. One generation of litter is the steady state.

The collection is confined **by label**, which is what makes it safe to run in
prod's store at all. That store is shared — another project's images and another
project's litter live in it — so `podman image prune` is not ours to run there.
Every image this repo builds carries `pkdump.build=1`, stage images included
(`Containerfile`, `lake/Containerfile`); a dangling image without it is somebody
else's and is never touched. `-f` is never passed either: an image something
still holds refuses to go, and that refusal is the right answer.

This is the complement of "a container gate removes the image it named"
(pd-5aba), not a duplicate of it — that rule is about the tag a gate *names*,
this one about the layers a tag stopped pointing at.

`tests/store/orphans.sh` is the gate (deploy tier, ~17s, not hermetic, pulls
nothing): the store flat across four builds, the cache intact, a neighbour's
dangling image untouched — and both red arms, a bare `podman build` growing the
store and collecting your OWN generation losing the cache.
`tests/deploy/run.sh` §11b holds the shell half, including that **every** stage
of **every** `Containerfile` in the tree carries the label, since the next stage
somebody adds is the one that would leak.

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
the scaffolding that is authoritative for it is gone, the file is stale and is
dropped, which puts podman back on the branch that rebuilds it. Deliberately
*not* `podman system migrate` (the repair found by hand first) — that kills the
pause process, which is per-user and shared with the store prod runs in.

#### …and the store it wedges is prod's (pd-3zjt)

The repair above only ever runs for a store this shell **opted into** —
`pkdump_store_netns_repair` returns immediately when `PKDUMP_STORE_ROOT` is
empty, and **prod's is always empty**. So the damage in the direction that
matters was never addressed at all. A CI gate's last bridge-network container
exits, podman `RemoveAll`s the shared directory, and the store left holding a
netns file that mounts into nothing is the one prod runs in.

That is not hypothetical: it failed `pkdump-value-snapshots@prod` every night
from 2026-08-12. Only jobs on a *user-defined* network are affected, so
`pkdump-refresh@` stayed green throughout and nothing else on the box said
anything.

**The scaffolding is now split, so there is nothing to take.** Activation writes
the store a `containers.conf` naming its own `[engine] tmp_dir` under its own
runroot, and passes it with `CONTAINERS_CONF_OVERRIDE` (the merge-on-top
spelling — plain `CONTAINERS_CONF` would replace whatever the box already has).
`--root`, `--runroot` and `--tmpdir` all leave `Engine.TmpDir` alone; this is the
only knob that moves it. Prod is given no `containers.conf` at all, so prod's tmp
dir stays exactly where podman puts it: the isolation is entirely on the non-prod
side, which is the side that was doing the damage.

**One caveat, and it needs a one-time action.** Podman records `tmp_dir` in the
store's libpod database when the store is **created** and pins it from then on:

```
level=debug msg="Overriding tmp dir \"…\" with \"/run/user/1000/libpod/tmp\" from database"
```

A store that already existed before this landed therefore keeps sharing prod's
scaffolding no matter what the generated config says. **Tear it down once** —

```bash
bash deploy/store-teardown.sh
```

— and the next `deploy/ci.sh` rebuilds it split. This applies to non-prod stores
only; prod has no store to tear down.

`ci.sh` asks podman once per run which tmp dir the store actually settled on
(`pkdump_store_split_check`) and prints the command above when the answer is
still the shared one — an operator action nothing checks is one nobody knows is
outstanding, and this one has no symptom until the night a cleanup lands between
prod and its network namespace. It warns rather than failing the run: the store
works, the gates pass, and since pd-3zjt the other end is bounded too
(`pkdump_store_netns_ensure` repairs a wedged namespace at the start of the jobs
that need one).

The same pin is what makes the split hold for callers that never see the
variable, and most of them do not. A Quadlet unit inherits none of this shell's
environment — systemd starts `podman run` with the unit's own environment and the
`GlobalArgs=` line, nothing else — and `pkdump-nessie.container` is on a
user-defined network, so a non-prod instance in an alternate store runs a
long-lived bridge container no exported variable can reach. Podman reads the
pinned tmp dir back out of the store's database, so those callers get the split
too. **Do not "finish" this by stamping `Environment=CONTAINERS_CONF_OVERRIDE=`
into the unit templates** — it is already covered, and a unit that names a path
inside a store is one more thing to get wrong when the store moves.

Two jobs run on a user-defined network (`deploy/value-snapshots.sh` and
`deploy/prices.sh`) and both ask before they start, via
`pkdump_store_netns_ensure`: `podman unshare --rootless-netns true` is the whole
probe, it runs the same setup a container start runs, and it needs no image,
network or container. With nothing else on the namespace it drops the stale file
and rebuilds; with only its own instance's containers on it, it restarts those
too (they are on the old namespace and would otherwise be unreachable). With
**anything else** on it — another project sharing prod's store — it refuses and
prints the two commands, because a nightly job may not restart someone else's
service to get its own work done.

**A repair that restarted something is not finished until that something
answers** (pd-p39v). `systemctl --user restart` returns when the *container* is
running, and Nessie is a JVM that will not serve for another 30–40 seconds — so
the repair used to hand a still-booting catalog to the job that asked for it,
which then died on a connection error. By the next run everything was healthy, so
the unit paged for a condition that had already fixed itself. Both wrappers
therefore pass `pkdump_store_netns_ensure` a readiness command
(`pkdump_lake_catalog_answering` from `deploy/lake-lib.sh`: an HTTP GET of
`/api/v2/config` from a throwaway container **on** the network, which is the same
path the job is about to take). The repair polls it for up to
`PKDUMP_NETNS_READY_TIMEOUT` seconds (120) and **fails** rather than proceeding if
it never holds. A caller that restarts something and supplies no readiness
command is refused: an unverifiable repair is this bug, and a default would buy a
silent second copy of it. The wait is paid only when something was actually
restarted — nothing running means nothing is mid-start.

Gates: `tests/deploy/run.sh` §8b–§8c for everything that is shell (including the
poll, its deadline, and the refusal), `tests/lake/value_snapshots.sh` §11 for the
readiness probe itself against a real Nessie — it runs only during a wedge, so
nothing else would ever exercise it — and `tests/store/netns_split.sh`, the one
non-hermetic store gate, for the parts only podman can answer: that an activated
store really does build its namespace under its own runroot and leaves the shared
directory alone (§1–§4), and that a store wedged for real is repaired and the
first start after it succeeds (§5).

### Low-disk guard

`deploy/diskcheck.sh` has two modes off one threshold source:

```bash
bash deploy/diskcheck.sh                    # alert mode — Layer 4 timer, always exits 0
bash deploy/diskcheck.sh --floor /some/path # gate mode — exits 1 under the floor
```

Gate mode is what `ci.sh` runs before it builds anything, and again before every
parallel dispatch. `PKDUMP_DISK_FLOOR_GB` (default 10) sets the floor. It exists
because running out of room mid-build does not announce itself as a disk problem.

**It measures the three disks a run writes to**, named once in `ci.sh` as
`PKDUMP_CI_DISK_PATHS` and spent by both checks:

| path | what lives there |
|------|------------------|
| `$HOME` | prod's default Podman store and its volumes; the toolchain caches on a box that has not relocated them |
| `$PKDUMP_STORE_ROOT` | the non-prod container store, where the host moved it (`store.env`) |
| `$TMPDIR` (default `/tmp`) | every `mktemp` under `deploy/` and `tests/` — the gates' work directories, the source trees the isolation guards copy, and the per-gate output `ci-parallel.sh` buffers |

The third one is not hypothetical (pd-20ia). On the deployment box `/tmp` is its
own LVM volume, so a full `/tmp` is invisible to the other two: the check
reported `/ has 40G free — ok` with **818M** left on `/tmp` — below the free
space that produced pd-fite's bus error in the first place. `diskcheck.sh`
reports each **device** once, so where `/tmp` shares a filesystem with either of
the others the extra arm costs nothing.

Note that **alert mode still watches one filesystem** (`PKDUMP_DISK_PATH`,
default `$HOME`). Widening what pages the operator is a separate decision from
widening what blocks a build.

**Alert mode exits 0 even when the push could not be delivered** (pd-4sqi).
`alert.sh` exits 1 when it reached nobody — an unconfigured or still-`CHANGE_ME`
channel, a failed `curl` — and under `set -e` that used to become diskcheck's
own status, so the script exited 0 every day the disk was fine and *failed its
unit* the day it was not. `systemctl status pkdump-diskcheck` then reported the
inversion of what happened, and the `OnFailure=` it fired escalated through
`alert.sh` — the same channel that had just proved it could not deliver. The
drop is still loud, on stderr and in the journal (`ALERT NOT DELIVERED`, on top
of `alert.sh`'s own diagnosis); it is `deploy/alarm-status.sh` that answers "is
the channel armed", not the exit code of a timer.

### Reclaiming `$TMPDIR` — the scratchpad reaper (pd-xgh6)

The guard above is what tells you `/tmp` is full. `deploy/tmpreap.sh` is what
stops it filling.

```bash
bash deploy/tmpreap.sh --dry-run   # name every directory it would remove
bash deploy/tmpreap.sh             # remove them
systemctl --user enable --now pkdump-tmpreap.timer   # host-wide, once, 05:30
```

Every Claude Code session on the box gets `$TMPDIR/claude-<uid>/<cwd-slug>/<session-uuid>/`
and **nothing ever collects it**. Measured here on 2026-08-30: 42G of a 49G
filesystem, 2261 session directories against a couple of dozen live sessions,
growing about 1G a day — and `ci.sh` correctly refusing to start at 817M free.
It is nobody's leak in particular, which is how it went uncollected for months.

A session directory is removed only when **all three** hold:

| | |
|---|---|
| it is a session directory | exactly `<root>/<slug>/<uuid>`, name matching the session-id shape. The root also holds unrelated caches (there was 579M of `uv-cache-<agent>` beside the sessions here) and those are counted and left |
| nothing live holds it | read from the process table — `CLAUDE_CODE_SESSION_ID` in a process's environment, a uuid on its command line (`--resume <id>`), or a cwd inside the directory. Never from a timestamp: a long-running session can sit quiet for days |
| nothing in it has been touched since the cutoff | `PKDUMP_TMPREAP_AGE_DAYS` (default 3) days ago. Redundant with the check above by design — it is the margin for a session that has started and not yet exported its id |

**It costs nobody a `--resume`.** The transcript is
`~/.claude/projects/<slug>/<session>.jsonl` and the persisted tool-result bodies
sit beside it under `$HOME`; what is under `$TMPDIR` is `scratchpad/` and
`tasks/*.output`, the working files of a process that is running.

**Exit 1 is the interesting outcome**, and it is what `OnFailure=` pages on: it
means the script refused to act because it could not tell a live session from a
dead one — claude processes running and not one of them yielding a session id.
That is indistinguishable from "every session is dead" by looking at the answer,
so it is asked as its own question. A reaper that has quietly stopped reaping is
a disk that fills again with nothing saying so.

`PKDUMP_TMPREAP_PROC` says where the process table is (default `/proc`), which
is what lets `tests/deploy/run.sh` §17 state both halves against a fake one —
the real one has this box's own live sessions in it. There is deliberately no
way to hand the script a liveness set or to switch the check off.

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
| `deploy.sh <name>` | Rebuild **both** images (the app, and the lake job image when the instance has a lakehouse), reinstall the unit files from this checkout, and restart one instance |
| `teardown.sh <name> [--purge]` | Stop and remove an instance; `--purge` deletes the data volume |
| `restore-litestream.sh [--yes] [--at=<RFC3339>] [--unattributed] <inst> [database-id]` | Restore ONE collection from the S3 backup (latest or point-in-time). Addressed by the database's file stem, not by a handle — `pkdump tenant list` says which is whose. **Refuses a database the registry cannot name** (restore `--registry` first; `--unattributed` for a purged one). See [RESTORE.md](RESTORE.md) |
| `backup-check.sh <inst> [user]` | Layer 1 — verify every S3 replica (tenants on freshness, the registry on correspondence), ping the off-box monitor (run by the `pkdump-backup-check@` timer). The verification always runs; the ping URL controls only the ping |
| `alarm-status.sh <inst> [--verify]` | Is alarming actually ARMED on this instance? Exit 0 = yes. `--verify` fires it for real |
| `diskcheck.sh` | Layer 4 — push a Pushover alert when the disk crosses the threshold (run by `pkdump-diskcheck.timer`) |
| `diskcheck.sh --floor [path...]` | Gate — exit non-zero under `PKDUMP_DISK_FLOOR_GB` free; run by `ci.sh` before it builds |
| `tmpreap.sh [--dry-run]` | Layer 4b — remove abandoned Claude session scratchpads under `$TMPDIR` (run by `pkdump-tmpreap.timer`). Exit 1 = it refused, because it could not tell a live session from a dead one |
| `setup-lake.sh <inst> [--port N] [--remove]` | Install the offline lakehouse — the Nessie catalog's Quadlet units and the PyIceberg job image. Refuses to run without `~/.config/pkdump/lake.env`. See [Offline lakehouse](#offline-lakehouse--nessie--iceberg) |
| `lake-lib.sh` | Sourced — the lake network as seen from outside a job (catalog URI, health URL, readiness probe), and the one place `localhost/pkdump-lake:<inst>` is named and built. Shared by `setup-lake.sh` and `deploy.sh` so a deploy cannot ship the app image and leave the job image behind (pd-rn4c) |
| `store-lib.sh` | Sourced — resolves which Podman store an instance's image and volume live in (`PKDUMP_STORE_ROOT`) |
| `units-lib.sh` | Sourced — renders every unit template this checkout ships into `~/.config`, preserving the instance's published port. Shared by `setup.sh` and `deploy.sh` so a deploy cannot ship a binary and leave the units behind (pd-2t6u) |
| `alert.sh "<title>" ["<msg>"]` | Shared Pushover sender used by every alarming layer (message also accepted on stdin); trims to the first 900 bytes, and sends an unchanged alert once per 24h ([the same page, twice](#the-same-page-twice)) |
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
    It **lands and builds nothing** (pd-lunn): the catalog is written by
    `pkdump-derive@<instance>`, from the partition this job leaves. So the two
    are a pair — this wrapper refuses to fetch anything while the derive timer
    is disabled, because landing without deriving is a box that is green every
    night and serves a catalog frozen at the day of the upgrade. `lake.env` is
    required for the same reason, and there is no `--land-raw` any more.
    Exit 2 is a **partial** run: the pokemontcg.io tail failed every retry, so the
    set list in tonight's partition is stale, but the run continued and TCGCSV's
    prices — the half a night cannot get back — landed (pd-nons). Unlike the
    transform tier's, it is deliberately not declared a success, so a persistent
    stall still pages.
- `pkdump-derive@<instance>` — nightly `pkdump-lake-derive shared` (07:00),
  ordered `After=` the landing. **The only thing that builds `shared.sqlite`.**
  Enable it on any box that runs the refresh; enable it *first*. See
  [deploy/LAKE.md](LAKE.md) §8.
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
- `pkdump-tmpreap` — host-wide scratchpad reaper (Layer 4b, daily at **05:30**,
  ahead of the 06:00 chain so the space is reclaimed before the night uses it).
  Not per-instance; enable once. See
  [Reclaiming `$TMPDIR`](#reclaiming-tmpdir--the-scratchpad-reaper-pd-xgh6).

The `@`-templated units are `%i`-templated, so one copy serves every instance —
the instance name is the part after `@`. Enable per-instance:

```bash
systemctl --user enable --now pkdump-derive@prod.timer         # BUILDS — enable first
systemctl --user enable --now pkdump-refresh@prod.timer        # LANDS — refuses without it
systemctl --user enable --now pkdump-backup-check@prod.timer   # after arming alerts.env
systemctl --user enable --now pkdump-value-snapshots@prod.timer # after setup-lake.sh
systemctl --user enable --now pkdump-diskcheck.timer           # host-wide, once
systemctl --user enable --now pkdump-tmpreap.timer             # host-wide, once
systemctl --user list-timers 'pkdump-*'        # check schedule
```

### One copy per box — and it belongs to the deploy clone (pd-onyd)

Every unit above is **a single file shared by every instance on the box**. The
`@` templates look per-instance and are not: `pkdump-refresh@.service` is one
file backing `pkdump-refresh@prod` and `pkdump-refresh@ci-9f2c1a` alike, and
`pkdump-diskcheck` is not templated at all. Each of them has `{{REPO_DIR}}`
substituted into its `ExecStart`, so a copy of `deploy/` has to still be there
at 06:00 for the unit to do anything. (The two Quadlet units —
`pkdump-<instance>.container` and `pkdump-litestream-<instance>.container` —
carry the instance in their file name, so they are genuinely per-instance.)

That made "install the units" mean "point prod's alerting, landing and disk
check at whichever checkout ran `setup.sh` **last**". `deploy/ci.sh` runs
`setup.sh` from a per-checkout worktree and `gt done` deletes that worktree.
Observed on the deployment box 2026-08-09: `deploy/setup.sh vault-unitfix
--test` from a polecat worktree left prod's units executing
`.../polecats/vault/pokedumpster/deploy/alert.sh` — 203/EXEC the moment the
branch landed, and the same shape as the Jun 2026 backup outage where the unit
was installed, enabled and executing nothing.

An install now rewrites them only when it is entitled to:

- it is for an instance this box treats as a **real deployment** — `prod`, or
  whatever `PKDUMP_ALERT_INSTANCES` names. That is the same predicate that
  decides whether an instance's units may page (`deploy/alert-gate.sh`); "may
  this instance own the pager" and "may it own the unit the pager lives in" get
  one answer rather than two that can drift;
- **nobody owns them yet** — a fresh box needs somebody to install them, and a
  dev box may never have a `prod` at all;
- the current owner's checkout **is gone** — those units are already broken, so
  pointing them at a directory that exists is a repair;
- `PKDUMP_INSTALL_HOST_UNITS=1`, the explicit override (`0` forces the skip).

Otherwise the install says whose they are and leaves them alone; the instance's
own Quadlet units are still written, because standing up a throwaway instance is
the normal case. The owner is read back out of the installed `ExecStart` rather
than kept in a marker file beside it — a marker can disagree with what systemd
will execute.

`deploy/alarm-status.sh` reports the other half: an alarming unit whose
`ExecStart` names a file that is no longer on disk is **NOT ARMED**, naming the
missing path, instead of reading as "alert unit installed".

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

That rule is mechanical rather than a convention people remember
(`tests/lake/tenant_isolation_test.sh`, lint tier, pd-cgi9). It asserts the two
halves separately: no Iceberg schema field name is tenant-identifying, and no
lake write path can open a tenant database — `crates/pkdump-lake` links no
SQLite crate at all, and the Python write-path modules import no `sqlite3`, so
both are true by construction the way "images are never landed" is true of the
closed `Source` enum. The transform tier (`value_snapshots.py`) is the one job
that opens tenant databases on purpose, and its half of the rule is the other
direction: it reads the lake and never writes to it.

**The rule is about the CATALOG ZONE**, and since the inbound leg (pd-8lw7) that
distinction is load-bearing: the same bucket also holds the **tenant zone** under
`tenant/` — holdings and valuations, always tenant-keyed, retained 90 days,
reached by credentials that reach nothing else (`deploy/TENANT_ZONE.md`). So the
guard's axis is the catalog zone against the tenant zone rather than the lake
against everything else (pd-7x83), and the tenant zone is a carve-out with the
rules INVERTED: every key it builds takes a `database_id`, it resolves no tenant
identity of its own, the shipper that fills it reaches no catalog prefix and no
catalog credential, and the online path (`pkdump-db`, `pkdump-server`,
`pkdump-keys`) links neither zone. The carve-out is total rather than a list of
exceptions — every Rust file in `crates/pkdump-lake` must be classified into
exactly one zone, so adding one is a decision the gate makes you take.

And the guard has been **seen red**: `tests/lake/tenant_isolation_selftest.sh`
(lint tier) injects one violation at a time into a copy of the tree and requires
the specific assertion to fail on each — including a tenant-keyed column added
to a catalog table, and including the four cases that must *not* fire, where the
tenant zone is being legitimately tenant-keyed.

Two pieces, both instance-scoped:

- **`pkdump-nessie-<inst>`** — the versioned Iceberg catalog. The one JVM
  service in this system, treated as a black box; our jobs speak the Iceberg
  REST API to it at `/iceberg/`. Version store is **ROCKSDB on a host
  directory**, not a podman volume: a rootless volume lives in the container
  store, and this box's default store is the 98 G disk prod runs from.
- **`localhost/pkdump-lake:<inst>`** — the job runtime, `lake/` in this repo.
  PyIceberg, no JVM. Installed by `setup-lake.sh` and **rebuilt by every
  `deploy.sh <inst>`** thereafter (pd-rn4c). It is a second image this checkout
  ships, and while only the installer built it, a change under `lake/` reached a
  box only if an operator remembered a second command — invisibly, because the
  stale jobs go on exiting 0. Prod ran a value-snapshots transform six hours
  older than its own checkout for a day, recording no sealed value series at
  all.

```bash
bash deploy/setup-lake.sh prod                 # unit + network + job image (install)
bash deploy/deploy.sh prod                     # ...and every deploy rebuilds the job image
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

**Nightly it is `pkdump-prices@<instance>.timer`**, running
`deploy/prices.sh` — the middle of the chain land → derive → prices →
transform. That job builds the day even when the landing run did not finish
(and records `pkdump.raw-complete=false`), because completeness is
conservative across datasets and failing on it would page most nights. The
alarm is on the **age** of the newest partition instead: more than two days
behind pages, which is the shape that actually matters — collections valued
nightly from prices that stopped arriving.

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

- **Layer 0 — uptime heartbeat.** `heartbeat.sh` curls the live listener every
  5 minutes (`pkdump-heartbeat@<inst>.timer`) and pings a **separate**
  healthchecks.io check (`PKDUMP_UPTIME_PING_URL`) only on HTTP 200. Numbered
  below Layer 1 because it is the question the other layers assume the answer
  to.

  Added 2026-08-16, after the site was hard down and nothing paged. Layer 1 was
  the only signal that could survive the box going away, and it infers liveness
  from backup freshness on a 6h period behind a 3h grace — up to nine hours late
  when it works. It was not working: the checker had false-alarmed on every run
  for four days, tripping `/fail` each time, so the monitor was already down and
  a real outage produced no state transition to alert on.

  Two rules, both load-bearing. **Its own check, never shared with Layer 1** —
  sharing is precisely how one noisy failure mode masked a vital one, and
  `alarm-status.sh` fails if the two URLs match. **A failed probe sends nothing
  and pages nothing from this box** — an outage is signalled by SILENCE, because
  a machine that lost power cannot transmit an alarm, and the monitor's grace
  window is what converts silence into a page.
- **Layer 1 — replication dead-man's switch (primary).** `backup-check.sh` runs
  every 6h (`pkdump-backup-check@<inst>.timer`), asks S3 about every replica,
  and pings an **off-box** monitor (healthchecks.io) only when they all pass. A
  broken-creds / stalled / dead-box / disabled-timer state stops the pings → the
  monitor alerts. This is the layer that catches the silent modes. It also writes
  a `.backup-last-ok` marker for Layer 3.

  **Both databases, one question: CORRESPONDENCE** (pd-me6h, extended
  2026-08-16). Is the replica BEHIND the database — not, was either touched
  recently. A quiescent database in correspondence is a perfect backup: restore
  it and you get the current file byte for byte. Do not "fix" an alarm here by
  raising a threshold; that has now been the false positive three times, and a
  bigger number only moves it.

  The registry was judged this way first, because it is static by design —
  handle → database_id changes only when a tenant is added, removed or renamed,
  legitimately months apart — so freshness was obviously the wrong question.
  Tenants took two more tries to get there. Judged on replica AGE, a collection
  nobody edited paged. Judged on `mtime` LAG (PR #51), the `.sqlite` mtime moves
  on checkpoint and on open, so it paged too. Both were proxies.

  **Why tenants are read from the sidecar's journal.** The registry gets its
  local txid from `litestream status`, but that command cannot answer for a
  tenant: tenants are one `dir:` + `pattern: "*.sqlite"` entry (so a new tenant
  needs no config rewrite), and v0.5.16 does not expand it — `status -json
  /data/tenants/<id>.sqlite` returns `[]`, and `status` with no path lists the
  directory as `"database": "/"`, `"status": "not initialized"`. The *running*
  sidecar publishes exactly the pair we need, once per second per database:

  ```
  msg="replica sync" db=<id>.sqlite replica=s3 txid.replica=…9 txid.db=…9
  ```

  `status` reads config; this reads behaviour. A tenant with no such line inside
  `PKDUMP_BACKUP_SIDECAR_LOOKBACK` (6h) — or one whose newest line is older than
  `PKDUMP_BACKUP_SIDECAR_GRACE_SECONDS` (1800s, against a one-second cadence) —
  is not being replicated, which is the loudest thing this checker can find.

  S3 is still asked directly: the sidecar reporting a tenant fully replicated
  while its prefix holds no LTX files at all is a fault, because a check that only
  asks the backing-up process how it is getting on is a check that agrees with a
  liar.

  **The grace is the sidecar's uptime, never a file timestamp** (pd-30yy). "Has
  this tenant had a fair chance to replicate yet" is a question about the process
  doing the replicating, and both file-timestamp answers to it failed toward
  silence: `mtime` moves on every write, so an old database reads new, and birth
  time does not move but says nothing about whether anything is watching the file
  — a database created an hour ago on a sidecar up for a week took a newborn's
  grace and passed unreplicated. So `podman inspect -f '{{.State.StartedAt}}'` is
  what the grace is measured against, and the three answers are three facts about
  the sidecar: not running is a fault, up less than the grace is not judged, and
  up longer than the grace having never named the database is an orphan.

  A lag is re-asked for up to `PKDUMP_BACKUP_CORRESPONDENCE_GRACE_SECONDS`
  (90s) before it counts. Litestream can hold an un-uploaded checkpoint across a
  transient S3 error and clear it at its next compaction tick ~30s later
  (measured, `tests/alarming/run.sh` §4b); a replica that has genuinely stopped
  never catches up, so the window only costs the run that would have paged over
  a blip.

  **One window, both legs.** One outage lags the tenants and the registry alike
  — same sidecar, same un-uploaded checkpoint — so both re-ask over it, each
  asking whichever component can answer for that database: the registry re-asks
  S3, tenants re-ask the sidecar's `replica sync` pair. Only the registry had a
  window until pd-yglw, and the asymmetry read as a flaky gate rather than as a
  bug: §4b passed on an idle box and paged over the tenant beside the registry
  it had just waited out on a loaded one.

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

### The same page, twice

`pkdump-value-snapshots@prod` failed the same way four nights running and pushed
four byte-identical pages. Every one was correct and not one was actionable, and
what they bought was a channel that gets swiped away without reading — which is
how the outage that came next, the sidecar's rootless-netns failure, reached
nobody (pd-hqdt). **A pager that repeats itself is a pager being switched off by
hand.**

So `alert.sh` sends the same alert **once per 24h**. The first page says so, in
its own words:

```
value-snapshots PARTIAL — Skipped: collection. The run completed for everyone
else; see journalctl --user -u pkdump-value-snapshots@prod.service

(Repeats of this same alert are suppressed for 24h — you will NOT be paged
again for it unless it changes.)
```

Four rules keep it from becoming silence:

- **The first occurrence always pages**, and always carries that notice. A
  reader who is not told reads the quiet afterwards as "it stopped".
- **A changed alert pages immediately**, however recently its neighbour did.
  The key is `(exact title, message with digit runs collapsed)`: the title is
  where every caller puts identity and severity, so two units never share a key
  and `LOW DISK (85%)` → `LOW DISK (99%)` rings. The message is normalised
  because the *same* failure carries numbers that move every night — ages, byte
  counts, pids — and a key any digit defeats would suppress nothing on the days
  it matters. The cost is deliberate: two failures of one caller whose text
  differs only in a number read as one alert.
- **Anything undecidable pages.** No `sha256sum`, no writable state directory, a
  clock that moved backwards, an unreadable stamp — every one sends. Same rule
  as `alert-gate.sh`: a silently disarmed alert is indistinguishable from a
  backup that quietly stopped running.
- **The window opens on delivery, not on the attempt.** A push `curl` could not
  deliver still fails the unit and records nothing, so the retry is not mistaken
  for a repeat.

Suppression is a decision, not a delivery failure: a withheld page exits 0 and
says why on stderr, so it lands in the unit's journal —

```
alert.sh: SUPPRESSED — identical to the page sent 7h ago; this alert can page again in 17h.
```

State is one small file per signature under
`${XDG_STATE_HOME:-~/.local/state}/pkdump/alerts/`, pruned as it expires.
`PKDUMP_ALERT_SUPPRESS_SECONDS` changes the window (`0` turns it off, which is
what `tests/alarming/run.sh` does — that gate provokes one failure repeatedly by
design); `PKDUMP_ALERT_NO_SUPPRESS=1` exempts a single call, which is how
`alarm-status.sh --verify` stays honest about whether a page would reach you
right now. `tests/alarming/alert_suppress_test.sh` (hermetic, sub-second, run by
`ci.sh`) is the gate, and most of it asserts what still gets **sent**.

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

# Per-instance: TWO healthchecks.io checks, never one. They watch unrelated
# things, and sharing one lets the noisier failure hold the check down while the
# other has nothing left to say — which is how a real outage went unreported on
# 2026-08-16. alarm-status.sh fails if these two URLs match.
#   Layer 0: period 5m,  grace 15m   — is the site serving?
#   Layer 1: period 6h,  grace 3h    — is the data reaching S3?
$EDITOR ~/.config/pkdump/<inst>/alerts.env   # PKDUMP_UPTIME_PING_URL + PKDUMP_HEARTBEAT_URL
                                             # PKDUMP_BACKUP_PING_URL

# Then enable the timers:
systemctl --user enable --now pkdump-heartbeat@<inst>.timer
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
