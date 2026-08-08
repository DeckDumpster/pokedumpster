# PokeDumpster — Restore runbook (DR SOP)

**Restore is the feature; backup is the plumbing.** This is the procedure to get
your collection back. It assumes nothing but a shell — no Claude, no extra tooling.

## What's backed up, and where

The irreplaceable set is **two things**, and one sidecar replicates both.

- **Every** tenant collection DB (`tenants/<database_id>.sqlite`) is continuously
  replicated to **S3** by the one Litestream sidecar
  (`pkdump-litestream-<inst>.service`). One sidecar, N tenants.
  Location: `s3://<bucket>/<LITESTREAM_S3_PATH>/<database_id>.sqlite`. The sidecar
  watches the `tenants/` directory and derives each tenant's prefix from its
  filename, so every tenant has its own and no two can collide.
- **The user registry** (`registry.sqlite`, at the data root) — the table that
  says which database belongs to which **handle**. Location:
  `s3://<bucket>/<LITESTREAM_S3_REGISTRY_PATH>`, *beside* the tenants prefix and
  never inside it. Bucket and both paths are in
  `~/.config/pkdump/<inst>/litestream.env`.

  **This is why the order in scenario C is not a style choice.** Tenant databases
  are named by an opaque `database_id`, not by a person's handle. Lose the
  registry and every byte is still there and *nothing is attributable* — a
  directory of ids with nobody's name on it. That is precisely the DR gap this
  project cited when it rejected libSQL/sqld (`bottomless` does not back up the
  namespace registry, so recovery has to re-declare namespaces before any data
  will restore), and we do not get to make that criticism unless our own registry
  is in the replicated set. It is. Drilled, not assumed — see the bottom of this
  file.
- **Point-in-time recovery: 6 months**, for both. You can restore either as it was
  at *any second* within the last 180 days (daily snapshots + the transaction
  log). The `snapshot:` policy is process-global, so the registry and every
  tenant share one cadence and one window.
- The **shared catalog** (`shared.sqlite`: cards/sets/prices) is **not** backed up
  — it's reproducible from upstream (`deploy/seed.sh <inst>`).
- Credentials: an assume-role profile (`~/.config/pkdump/<inst>/aws/config`) + the
  bootstrap key in podman secret `pkdump-<inst>-s3-bootstrap`. Auto-refreshing
  temporary creds; never static keys.

## Quick reference

```bash
# Restore the LATEST backup onto the live instance (stops app+sidecar, restores,
# restarts, verifies row count + that both services came back). Tenant
# defaults to `collection`:
bash deploy/restore-litestream.sh prod
bash deploy/restore-litestream.sh prod alice        # a different tenant

# Point-in-time restore (recover from a mistake within the last 6 months):
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod alice

# The user registry (handle -> database_id). Restore this FIRST after a total
# loss — it is what tells you which databases exist and whose they are:
bash deploy/restore-litestream.sh --registry prod
```

> **Where the two-identifier model is today.** The registry file, its schema and
> its replication are in place; resolution through it, the CLI that writes it,
> and the migration of existing tenants onto opaque ids are separate work
> (`pd-rqgv`, `pd-zr9n`, `pd-hqee`). So on an instance that has not migrated,
> `database_id` and the handle are still the same string and every command below
> reads identically either way — `prod alice` and `prod <alice's id>` are the
> same restore. The ordering is written for the model being built, because the
> order is what has to be right *before* the ids go opaque, not after.

> **Upgrading an existing instance.** The registry entry needs two keys
> (`LITESTREAM_REGISTRY_DB`, `LITESTREAM_S3_REGISTRY_PATH`) that a
> `litestream.env` written before it does not have. Run
> `bash deploy/setup.sh <inst>` — it backfills them in place without touching
> anything you chose — then
> `systemctl --user restart pkdump-litestream-<inst>.service`.
> You cannot forget this quietly: without them the sidecar **refuses to start**
> (`must specify either 'path' or 'dir'`) rather than backing up the tenants and
> leaving the registry off, and the unit's `OnFailure` alert fires.
>
> **This is not enough on an instance that also predates the `tenants/` layout**
> — one whose `litestream.env` still has `LITESTREAM_DB_PATH` and
> `LITESTREAM_S3_PATH=<inst>/collection`. As of 2026-08-08 `prod` is such an
> instance: it is replicating happily on the pre-`tenants/` single-database
> config. Backfilling the registry keys there would leave it with a tenants
> directory it does not use and a replica prefix that means something else.
> Do [TENANTS.md](TENANTS.md)'s production cutover first — that one changes a
> replica prefix and is deliberately not automated.

---

## Restoring ONE tenant while the others stay live

This is the case the whole layout exists for: recovering one person's collection
without rolling anybody else back.

**You do not need to stop the other tenants, and you cannot accidentally restore
them.** Two structural reasons, both drilled (see the bottom of this file):

- **One file per tenant.** A restore writes exactly
  `tenants/<tenant>.sqlite` on the data volume. No other tenant's file is read,
  written, or renamed — they come out of the restore *byte-identical*.
- **One replica prefix per tenant.** The sidecar runs in Litestream's directory
  mode, so each tenant's prefix is derived from its filename and a restore reads
  only that prefix. Restoring one tenant neither consumes nor invalidates
  another's replica: an untouched tenant still restores to *current* afterwards.

What the restore *does* interrupt is replication for **everyone**, briefly —
there is one sidecar, and it is stopped for the duration (seconds to a minute).
A write another tenant takes during that window is not lost: the sidecar picks it
up when it resumes (drilled — a tenant written to mid-restore had that row in S3
afterwards). It is simply not off-box until then. That is the whole blast radius
of restoring one tenant, and it is the cost of one sidecar serving all of them.

```bash
cd ~/pokedumpster
bash deploy/restore-litestream.sh prod alice        # add --yes to skip the prompt
```

### Confirm the others were untouched

Do this — do not assume it. Take the hashes **before** you start (with the
sidecar stopped, so nothing is mid-checkpoint), and compare after. A tenant that
was *written to* between the two fingerprints will differ, of course; what must
not differ is a tenant nobody touched.

```bash
INSTANCE=prod; VICTIM=alice
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-${INSTANCE}-data)

# BEFORE — quiesce, then fingerprint every OTHER tenant.
systemctl --user stop pkdump-litestream-${INSTANCE}.service
sha256sum "${MP}"/tenants/*.sqlite | grep -v "/${VICTIM}.sqlite$" | tee /tmp/tenants-before

# ... run the restore ...
bash deploy/restore-litestream.sh --yes ${INSTANCE} ${VICTIM}

# AFTER — same fingerprints. Stop the sidecar first: once it is running again it
# writes its own bookkeeping tables into every database it picks up, which is not
# the restore's doing but does change the bytes.
systemctl --user stop pkdump-litestream-${INSTANCE}.service
sha256sum "${MP}"/tenants/*.sqlite | grep -v "/${VICTIM}.sqlite$" | diff - /tmp/tenants-before \
    && echo "OK: every other tenant is byte-identical"
systemctl --user reset-failed pkdump-litestream-${INSTANCE}.service
systemctl --user start pkdump-litestream-${INSTANCE}.service

# And that their BACKUPS are still flowing (per tenant, against S3):
bash deploy/backup-check.sh ${INSTANCE}
```

---

## Scenario A — a collection lost or corrupted (box intact)

```bash
cd ~/pokedumpster
bash deploy/restore-litestream.sh prod          # tenant `collection`
bash deploy/restore-litestream.sh prod alice    # or a named tenant
```
It stops the app + sidecar, restores `tenants/<tenant>.sqlite` from S3 onto the data
volume (temp-then-rename), restarts both, prints the row count, and **fails loudly
if either service did not come back** — a restored collection that is no longer
replicating is a half-finished recovery, and that failure is otherwise silent.

## Scenario B — undo a mistake (point-in-time)

You deleted/changed data and noticed later (within 6 months). **Inspect first,
then commit** so you don't overwrite good data with the wrong point.

```bash
# 1. Restore the suspected good time to a TEMP file and check it (non-destructive):
#    The sidecar's config names no database (it watches a directory), so a restore
#    addresses the tenant's replica by URL. `tenant_replica_url` derives it — the
#    same function deploy/restore-litestream.sh uses, so this cannot drift from
#    what the sidecar actually wrote.
D=$(mktemp -d); chmod 777 "$D"
cd ~/pokedumpster
. deploy/litestream-lib.sh
set -a; . ~/.config/pkdump/prod/litestream.env; set +a
podman run --rm --user 0:0 -v "$D:/out" \
  -v ~/.config/pkdump/prod/aws/config:/aws/config:ro \
  --secret pkdump-prod-s3-bootstrap,type=mount,target=/aws/credentials \
  -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
  docker.io/litestream/litestream:latest \
  restore -integrity-check full -timestamp 2026-06-01T12:00:00Z \
  -o /out/check.sqlite "$(tenant_replica_url collection)"
sqlite3 "$D/check.sqlite" 'SELECT count(*) FROM collection;'   # looks right?

# 2. If good, commit it onto the live instance:
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod collection
rm -rf "$D"
```

**A rollback is not a one-way door — but it does move the clock.** Once the
sidecar comes back it replicates the *rolled-back* database, so from that moment
`latest` **is** the rollback. The replica is append-only (the rollback lands as a
new transaction; it does not rewrite history), so the state you discarded is
still recoverable — ask for a timestamp from just **before** you ran the
rollback:

```bash
bash deploy/restore-litestream.sh --at=<a time before the rollback> prod collection
```

Which is the argument for step 1: check the temp copy before committing, and note
the wall-clock time when you do commit.

## Scenario C — total box loss (rebuild from scratch)

```bash
cd ~ && git clone git@github.com:rgantt/pokedumpster.git && cd pokedumpster
bash deploy/setup.sh prod 8090            # build image + install units (keep the :8090 port)

# Re-create the backup config + creds for this instance:
#   ~/.config/pkdump/prod/litestream.env   (bucket/region; LITESTREAM_S3_PATH=prod/tenants,
#                                           LITESTREAM_TENANTS_DIR=/data/tenants,
#                                           LITESTREAM_REGISTRY_DB=/data/registry.sqlite,
#                                           LITESTREAM_S3_REGISTRY_PATH=prod/registry.sqlite)
#     setup.sh writes all of these; it is only listed here so you can check them.
#   ~/.config/pkdump/prod/aws/config       ([profile pkdump] role_arn=... source_profile=bootstrap region=...)
#   podman secret create pkdump-prod-s3-bootstrap -   (paste the [bootstrap] key; from your password manager)
```

### ⚠️ Restore the REGISTRY first, then the tenant databases

**This order is load-bearing. Do not invert it.**

A tenant database is named by an opaque `database_id`, not by a handle. So the
bucket on its own answers *"how many databases are there"* and **not** *"whose is
which"*. Restore the tenants first and you are holding a directory of files called
`01k2c7hq8n….sqlite` with no way to tell whose collection is inside any of them —
every byte present, nothing attributable. The registry is the only thing that
puts the names back.

```bash
set -a; . ~/.config/pkdump/prod/litestream.env; set +a

# ── STEP 1 — the registry. Works on a bare volume; nothing has to exist first.
bash deploy/restore-litestream.sh --yes --registry prod
```

```bash
# ── STEP 2 — ask it who exists. THIS is the tenant list.
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-prod-data)
sqlite3 "file:${MP}/registry.sqlite?mode=ro" \
  "SELECT handle, database_id FROM user WHERE state='active';"
#   alice|01k2c7hq8nz0xw3v9r5m6d0abc
#   bob  |01k2c7hq8p41k8t2y7q3n5e1def
```

```bash
# ── STEP 3 — restore each database the registry named, then rebuild the catalog.
bash deploy/restore-litestream.sh --yes prod 01k2c7hq8nz0xw3v9r5m6d0abc
bash deploy/restore-litestream.sh --yes prod 01k2c7hq8p41k8t2y7q3n5e1def
bash deploy/seed.sh prod                        # rebuild the shared catalog from upstream
systemctl --user start pkdump-prod              # start the app
```

**Cross-check against the bucket.** Neither source is trusted alone: the registry
says which ids *should* exist, and the bucket says which ones *do*. They must
agree. A prefix the registry does not name is an orphan (a detached user, or a
tenant whose registry row was lost); an id the registry names with no prefix is a
tenant that was never replicated, which is an incident of its own.

```bash
podman run --rm \
  -v ~/.config/pkdump/prod/aws/config:/aws/config:ro \
  --secret pkdump-prod-s3-bootstrap,type=mount,target=/aws/credentials \
  -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
  -e AWS_PROFILE=pkdump -e AWS_REGION="${LITESTREAM_S3_REGION}" \
  docker.io/amazon/aws-cli:latest \
  s3 ls "s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_PATH}/"
#   PRE 01k2c7hq8nz0xw3v9r5m6d0abc.sqlite/    <- one prefix per tenant
#   PRE 01k2c7hq8p41k8t2y7q3n5e1def.sqlite/      strip ".sqlite/" for the id
```

**If the registry itself is gone** — the one case the order cannot save you from
— the databases are still fully recoverable, but anonymous: restore each prefix
and identify them by their contents. Assume nothing about which is which. This is
the failure the registry being replicated exists to prevent, so if you are here,
find out why its replica was missing before rebuilding anything on top of it.

The data volume does not need to be prepared: the restore creates `tenants/` if
it is missing, so restoring onto a bare volume is the same command. Provisioning
(`pkdump tenant create`) is **not** part of recovery — the restored file *is* the
tenant, and `watch: true` puts it back under replication within ~5s.

---

## Verify a restore

```bash
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-prod-data)
sqlite3 "file:${MP}/tenants/collection.sqlite?mode=ro" 'SELECT count(*) FROM collection;'
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8090/api/sets     # 200 = serving
systemctl --user is-active pkdump-litestream-prod.service                    # backups resumed
bash deploy/backup-check.sh prod                                             # ...and are FRESH, per tenant
```

`is-active` is liveness, not freshness — the sidecar can sit `active` while
error-looping on bad credentials. `backup-check.sh` is the one that asks S3.

## If something is wrong

- **`InvalidClientTokenId` / `cannot load profile`** → the podman secret holds a
  bad/rotated key. Recreate it (`podman secret rm` + `create`) and
  `systemctl --user restart pkdump-litestream-prod.service`.
- **Sidecar won't start** → it gates on `~/.config/pkdump/prod/aws/config` existing
  (`ConditionPathExists`); create it.
- **"the restore succeeded but these units did NOT come back up"** → most likely
  the systemd start-rate limit: the sidecar unit allows 5 starts per 300s so a
  crash-loop pages instead of thrashing, and several restores in a row spend that
  budget. The script clears it before starting, so if you still see this, the
  sidecar is failing for a real reason:
  `systemctl --user status pkdump-litestream-prod.service` and
  `journalctl --user -u pkdump-litestream-prod.service -n 50`. **Backups are off
  until it runs.**
- **`litestream restore` says no snapshots** → check the bucket/path in
  `litestream.env` and that the secret's key can assume the role. Print the
  prefix it is reading with
  `. deploy/litestream-lib.sh; tenant_replica_url <tenant>` (or
  `registry_replica_url` for the registry).
- **`missing in litestream.env — run deploy/setup.sh <instance>`** → this
  instance's config predates the registry entry. `bash deploy/setup.sh <inst>`
  backfills the two keys, then restart the sidecar. The scripts refuse to guess
  a replica prefix on purpose: an empty one is the single misconfiguration
  Litestream does *not* complain about — it replicates to the bucket root in
  silence.
- **Sidecar exits immediately with `must specify either 'path' or 'dir'`** →
  same cause, same fix. This is the deliberate loud failure: an instance that
  has not been backfilled does not run at all, rather than running with the
  registry quietly outside the replicated set.
- **`timestamp does not exist`** → the timestamp is outside that tenant's replica
  history, in *either* direction. A time newer than the newest replicated write
  fails rather than silently giving you the latest state; drop `--at` to get
  latest, or list what exists:
  `litestream ltx -level all "$(tenant_replica_url <tenant>)"`.
- **`database not found in config`** → you passed a database path to
  `restore -config`. The sidecar config names no databases; address the replica
  by URL (above).
- **Restoring tenant `collection` from *before* the tenants migration** → its
  replica prefix changed at cutover. Pre-cutover history lives at the old prefix
  and is addressed by URL: `deploy/TENANTS.md`, "Backups across the migration".

## Test this runbook periodically

A backup you haven't restored from is a hypothesis, not a backup. The drill is
in-tree and automated:

```bash
bash tests/litestream/drill.sh                  # ~3 min, throwaway MinIO, offline
DRILL_REAL_S3=1 bash tests/litestream/drill.sh  # same, against the real bucket
                                                #   under a throwaway prefix
```

It stands up a four-tenant instance with the shipped Quadlet sidecar and walks
the scenarios above with the shipped scripts: deletes tenant #2 of 4 and restores
it in place, rolls it back in time, un-rolls it, then wipes the whole volume —
registry included — and recovers everything in the documented order, asserting
after each restore that the other three tenants are byte-identical, still
replicating, and still restore to their own current data. It is part of
`deploy/ci.sh`, so the runbook cannot rot silently.

Its tenants are named by opaque database ids on purpose. With `alpha.sqlite` on
disk, "restore the registry and check every tenant is reachable by handle" proves
nothing — the filename already said `alpha`. So the drill prints what the bucket
alone can tell you with the registry gone (four ids, not one name), restores the
registry first, and recovers every tenant *by handle* from what it says.

**Last exercised: 2026-08-08** (pd-nd6w), MinIO mode, 32/32 checks — including
the registry loss/restore path above. The previous full pass across both modes
was 2026-08-07 (pd-v8zf), 25/25 each: MinIO, and the production bucket under
prefix `pd-v8zf-drill/` with the real assume-role credentials (prefix deleted
afterwards — 0 objects; `prod/collection` untouched at 618 objects throughout).
`DRILL_REAL_S3=1` has **not** been re-run since the registry entry landed.
