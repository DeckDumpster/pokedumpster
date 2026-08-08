# PokeDumpster — Restore runbook (DR SOP)

**Restore is the feature; backup is the plumbing.** This is the procedure to get
your collection back. It assumes nothing but a shell — no Claude, no extra tooling.

## What's backed up, and where

- **Every** tenant collection DB (`tenants/<tenant>.sqlite`) is continuously
  replicated to **S3** by the one Litestream sidecar
  (`pkdump-litestream-<inst>.service`). One sidecar, N tenants.
- Location: `s3://<bucket>/<LITESTREAM_S3_PATH>/<tenant>.sqlite` — bucket and
  path are in `~/.config/pkdump/<inst>/litestream.env`. The sidecar watches the
  `tenants/` directory and derives each tenant's prefix from its filename, so
  every tenant has its own and no two can collide.
- **Point-in-time recovery: 6 months.** You can restore the DB as it was at *any
  second* within the last 180 days (daily snapshots + the transaction log).
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
```

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
#                                           LITESTREAM_TENANTS_DIR=/data/tenants)
#   ~/.config/pkdump/prod/aws/config       ([profile pkdump] role_arn=... source_profile=bootstrap region=...)
#   podman secret create pkdump-prod-s3-bootstrap -   (paste the [bootstrap] key; from your password manager)
```

**Which tenants existed?** Ask the bucket — it is the registry. Directory mode
names each prefix after the tenant's file, so the list of recoverable tenants is
the list of prefixes. Nothing local has to survive for this to work, and there is
no separate registry object that can be missing (the rejected libSQL/sqld path
had exactly that gap: `bottomless` did not back up the namespace registry, so DR
had to re-declare every namespace before any data would restore. File-per-tenant
has no equivalent — verified, not assumed; see the drill below).

```bash
set -a; . ~/.config/pkdump/prod/litestream.env; set +a
podman run --rm \
  -v ~/.config/pkdump/prod/aws/config:/aws/config:ro \
  --secret pkdump-prod-s3-bootstrap,type=mount,target=/aws/credentials \
  -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
  -e AWS_PROFILE=pkdump -e AWS_REGION="${LITESTREAM_S3_REGION}" \
  docker.io/amazon/aws-cli:latest \
  s3 ls "s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_PATH}/"
#   PRE collection.sqlite/
#   PRE alice.sqlite/          <- one prefix per tenant; strip ".sqlite/" for the name
```

Then restore each one, and rebuild the catalog:

```bash
bash deploy/restore-litestream.sh --yes prod collection
bash deploy/restore-litestream.sh --yes prod alice        # once per tenant
bash deploy/seed.sh prod                        # rebuild the shared catalog from upstream
systemctl --user start pkdump-prod              # start the app
```

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
  `. deploy/litestream-lib.sh; tenant_replica_url <tenant>`.
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
it in place, rolls it back in time, un-rolls it, wipes the whole volume and
recovers every tenant from the bucket alone — asserting after each restore that
the other three are byte-identical, still replicating, and still restore to their
own current data. It is part of `deploy/ci.sh`, so the runbook cannot rot
silently.

**Last exercised: 2026-08-07** (pd-v8zf), both modes, 25/25 checks each:
MinIO, and the production bucket under prefix `pd-v8zf-drill/` with the real
assume-role credentials (prefix deleted afterwards — 0 objects; `prod/collection`
untouched at 618 objects throughout).
