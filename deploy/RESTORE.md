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
- Restoring **one** tenant reads only that tenant's prefix; the others keep
  replicating untouched. The full multi-tenant DR procedure is `pd-v8zf`.
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
# restarts, verifies row count). Tenant defaults to `collection`:
bash deploy/restore-litestream.sh prod
bash deploy/restore-litestream.sh prod alice        # a different tenant

# Point-in-time restore (recover from a mistake within the last 6 months):
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod
```

---

## Scenario A — collection lost or corrupted (box intact)

```bash
cd ~/pokedumpster
bash deploy/restore-litestream.sh prod          # add --yes to skip the prompt
```
It stops the app + sidecar, restores `tenants/<tenant>.sqlite` from S3 onto the data
volume (temp-then-rename), restarts both, and prints the row count. Done.

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
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod
rm -rf "$D"
```

## Scenario C — total box loss (rebuild from scratch)

```bash
cd ~ && git clone git@github.com:rgantt/pokedumpster.git && cd pokedumpster
bash deploy/setup.sh prod 8090            # build image + install units (keep the :8090 port)

# Re-create the backup config + creds for this instance:
#   ~/.config/pkdump/prod/litestream.env   (bucket/region; LITESTREAM_S3_PATH=prod/tenants,
#                                           LITESTREAM_TENANTS_DIR=/data/tenants)
#   ~/.config/pkdump/prod/aws/config       ([profile pkdump] role_arn=... source_profile=bootstrap region=...)
#   podman secret create pkdump-prod-s3-bootstrap -   (paste the [bootstrap] key; from your password manager)

bash deploy/restore-litestream.sh --yes prod   # pull the collection back from S3
bash deploy/seed.sh prod                        # rebuild the shared catalog from upstream
systemctl --user start pkdump-prod              # start the app
```

---

## Verify a restore

```bash
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-prod-data)
sqlite3 "file:${MP}/tenants/collection.sqlite?mode=ro" 'SELECT count(*) FROM collection;'
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8090/api/sets     # 200 = serving
systemctl --user is-active pkdump-litestream-prod.service                    # backups resumed
```

## If something is wrong

- **`InvalidClientTokenId` / `cannot load profile`** → the podman secret holds a
  bad/rotated key. Recreate it (`podman secret rm` + `create`) and
  `systemctl --user restart pkdump-litestream-prod.service`.
- **Sidecar won't start** → it gates on `~/.config/pkdump/prod/aws/config` existing
  (`ConditionPathExists`); create it.
- **`litestream restore` says no snapshots** → check the bucket/path in
  `litestream.env` and that the secret's key can assume the role. Print the
  prefix it is reading with
  `. deploy/litestream-lib.sh; tenant_replica_url <tenant>`.
- **`database not found in config`** → you passed a database path to
  `restore -config`. The sidecar config names no databases; address the replica
  by URL (above).

## Test this runbook periodically

Do a non-destructive drill (Scenario B step 1) every so often and confirm the row
count. A backup you haven't restored from is a hypothesis, not a backup.
