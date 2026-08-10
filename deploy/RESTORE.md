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

  **And the order is now ENFORCED, not just documented** (`pd-7f46`).
  `restore-litestream.sh` refuses to restore a database named by an opaque id
  unless the volume's registry names that id — because the wrong order does not
  fail on its own. It *succeeds*: every byte comes back, `integrity_check` says
  ok, and what you are holding is anonymous. A recovery that looks finished and
  is not is the same failure this project already owns once (`pd-1717`: the
  sidecar `active`, "snapshot complete" in the journal, and `txid.replica` at
  zero), so it is an error here rather than a footnote. `--unattributed` is the
  explicit way past it, for the one case that belongs there — see scenario B2.
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
# FIRST, after any total loss: the user registry (handle -> database_id). It is
# what tells you which databases exist and whose they are — see scenario C:
bash deploy/restore-litestream.sh --registry prod

# Restore the LATEST backup of ONE collection onto the live instance (stops
# app+sidecar, restores, restarts, verifies row count + that both services came
# back). The argument is the database's FILE STEM, defaulting to `collection`:
bash deploy/restore-litestream.sh prod
bash deploy/restore-litestream.sh prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC

# Point-in-time restore (recover from a mistake within the last 6 months):
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC
```

> **The argument names a FILE, not a person.** A collection is stored under an
> opaque `database_id`, so "restore alice's collection" is two steps: ask the
> registry which database is alice's, then restore that. **`pkdump tenant list`
> is step one** (`deploy/TENANTS.md`, "Which file is whose") — a handle you
> remember is not an answer, because a rename moves the handle and leaves the id
> alone, and a recreated handle points at a different database entirely.
>
> Which is also why the restore will not run without the registry: it prints
> `Attribution: <id> belongs to <handle>` before it touches anything, and if it
> cannot say that, it stops. A **detached** user is still attributable (the row
> survives under their real handle), so their data restores normally. A
> **handle-named** database — `tenants/collection.sqlite`, which is what `prod`
> still has — is exempt, because its filename already says whose it is; the gate
> is about opaque ids, so `restore-litestream.sh prod` is unaffected by it.
>
> **A data volume mid-migration holds BOTH shapes**, so the scripts accept
> either stem. `pkdump tenant create` has minted opaque ids since `pd-zr9n`;
> databases predating it are still named by their handle until
> `pkdump tenant migrate` (`pd-hqee`) puts them on ids, so
> `restore-litestream.sh prod collection` is valid on a box that has not
> migrated yet. **The ids are case-sensitive**: `01J8…` and `01j8…` are one file
> on a case-insensitive filesystem but two different S3 prefixes, so the scripts
> reject the lowercase form rather than restore from a prefix that does not
> exist.

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
> **`prod` needs this.** Its `litestream.env` took [TENANTS.md](TENANTS.md)'s
> cutover on 2026-08-08 (`LITESTREAM_S3_PATH=prod/tenants`,
> `LITESTREAM_TENANTS_DIR=/data/tenants`) and has neither registry key, so it is
> exactly the instance described above: the moment it is deployed with this
> config, the sidecar stops starting until `deploy/setup.sh prod` has run.
> **Backups are off while it will not start** — do the backfill in the same
> maintenance window as the deploy, not after it.
>
> On an instance that has not taken that cutover either — `litestream.env` still
> carrying `LITESTREAM_DB_PATH` and `LITESTREAM_S3_PATH=<inst>/collection` — the
> backfill is still not enough on its own. It deliberately never touches
> `LITESTREAM_S3_PATH`, because changing a replica prefix is a data cutover and
> not a config edit; do TENANTS.md's migration first.

---

## Restoring ONE tenant while the others stay live

This is the case the whole layout exists for: recovering one person's collection
without rolling anybody else back.

**You do not need to stop the other tenants, and you cannot accidentally restore
them.** Two structural reasons, both drilled (see the bottom of this file):

- **One file per tenant.** A restore writes exactly
  `tenants/<database_id>.sqlite` on the data volume. No other tenant's file is
  read, written, or renamed — they come out of the restore holding exactly the
  data they held.
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
# Step 1 — which database is alice's? The disk cannot tell you; the registry can.
podman exec systemd-pkdump-prod pkdump tenant list
# Step 2 — restore that id. Add --yes to skip the prompt.
bash deploy/restore-litestream.sh prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC
```

### Confirm the others were untouched

Do this — do not assume it. Take the fingerprints **before** you start (with the
sidecar stopped, so nothing is mid-checkpoint), and compare after. A tenant that
was *written to* between the two will differ, of course; what must not differ is
a tenant nobody touched.

**Fingerprint the CONTENT, not the file.** `sha256sum` on the `.sqlite` file is
the obvious thing to reach for and it is wrong: SQLite makes no promise that a
database keeps the same bytes across the operations a running box performs. A WAL
checkpoint folds committed frames back into the main file, the freelist gets
reused, pages get reordered — all of which rewrite the file and change not one
fact in it. Even `pkdump tenant list` does it, because it opens each database to
read its schema version and checkpoints on close. A byte comparison will tell you
a bystander was clobbered on a day when nothing of the sort happened, which is the
worst possible day to be chasing a phantom. This is drilled the same way
(`tests/litestream/drill.sh`, pd-zk0c).

```bash
INSTANCE=prod; VICTIM=01K2C7HQ8NZ0XW3V9R5M6D0ABC   # the database id, from `pkdump tenant list`
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-${INSTANCE}-data)

# What a tenant database SAYS: its schema and all of its rows, in a stable order,
# less Litestream's own bookkeeping tables (which change across a restart for the
# benign reason above). Read-only — opening one read-write would checkpoint it,
# and the measurement would move what it is measuring.
fingerprint() {   # fingerprint <path to a tenant .sqlite>
    local uri="file:$1?mode=ro" tbl
    sqlite3 "$uri" 'PRAGMA integrity_check;'
    sqlite3 "$uri" "SELECT type, name, sql FROM sqlite_master
                     WHERE name NOT LIKE '\_litestream\_%' ESCAPE '\' ORDER BY type, name;"
    sqlite3 "$uri" "SELECT name FROM sqlite_master WHERE type='table'
                     AND name NOT LIKE '\_litestream\_%' ESCAPE '\' ORDER BY name;" |
    while IFS= read -r tbl; do
        echo "== $tbl"; sqlite3 "$uri" "SELECT * FROM \"$tbl\";" | sort
    done
}
fingerprint_others() {
    for db in "${MP}"/tenants/*.sqlite; do
        [ "$db" = "${MP}/tenants/${VICTIM}.sqlite" ] && continue
        echo "### $db"; fingerprint "$db"
    done
}

# BEFORE — quiesce, then fingerprint every OTHER tenant.
systemctl --user stop pkdump-litestream-${INSTANCE}.service
fingerprint_others > /tmp/tenants-before

# ... run the restore ...
bash deploy/restore-litestream.sh --yes ${INSTANCE} ${VICTIM}

# AFTER — same fingerprints, and `diff` names the row if one ever does move.
systemctl --user stop pkdump-litestream-${INSTANCE}.service
fingerprint_others | diff - /tmp/tenants-before \
    && echo "OK: every other tenant holds exactly the data it held"
systemctl --user reset-failed pkdump-litestream-${INSTANCE}.service
systemctl --user start pkdump-litestream-${INSTANCE}.service

# And that their BACKUPS are still flowing (per tenant, against S3):
bash deploy/backup-check.sh ${INSTANCE}
```

---

## Scenario A — a collection lost or corrupted (box intact)

```bash
cd ~/pokedumpster
podman exec systemd-pkdump-prod pkdump tenant list   # whose database is which
bash deploy/restore-litestream.sh prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC
bash deploy/restore-litestream.sh prod               # or the default stem, `collection`
```
It stops the app + sidecar, restores `tenants/<database_id>.sqlite` from S3 onto the data
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

# Which database holds the collection you mean? `pkdump tenant list` is the
# only thing that answers it — the filename is an opaque id. On a box that has
# not run `pkdump tenant migrate` yet, this is still the handle (`collection`).
DB=01K2C7HQ8NZ0XW3V9R5M6D0ABC
podman run --rm --user 0:0 -v "$D:/out" \
  -v ~/.config/pkdump/prod/aws/config:/aws/config:ro \
  --secret pkdump-prod-s3-bootstrap,type=mount,target=/aws/credentials \
  -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump \
  docker.io/litestream/litestream:latest \
  restore -integrity-check full -timestamp 2026-06-01T12:00:00Z \
  -o /out/check.sqlite "$(tenant_replica_url "$DB")"
sqlite3 "$D/check.sqlite" 'SELECT count(*) FROM collection;'   # looks right?

# 2. If good, commit it onto the live instance:
bash deploy/restore-litestream.sh --at=2026-06-01T12:00:00Z prod "$DB"
rm -rf "$D"
```

**A rollback is not a one-way door — but it does move the clock.** Once the
sidecar comes back it replicates the *rolled-back* database, so from that moment
`latest` **is** the rollback. The replica is append-only (the rollback lands as a
new transaction; it does not rewrite history), so the state you discarded is
still recoverable — ask for a timestamp from just **before** you ran the
rollback:

```bash
bash deploy/restore-litestream.sh --at=<a time before the rollback> prod "$DB"
```

Which is the argument for step 1: check the temp copy before committing, and note
the wall-clock time when you do commit.

## Scenario B2 — you removed a user and want them back

**Check whether this is a restore at all first.** `pkdump tenant remove` is now
an alias for `detach` and **deletes nothing** (`deploy/TENANTS.md`); the
database and its replica are both still there, and the row survives — under the
person's **own handle**, with `retired_at` saying when it was released. So the
usual answer is a registry edit, not a recovery:

```bash
podman exec systemd-pkdump-prod pkdump tenant list   # carol, detached, RETIRED <when>
```

If the database is still on the volume, **no restore is needed and none will
help** — the bytes never moved. What a detach costs is the *mapping*: the handle
is released, the row is marked `detached`, and a detached row does not resolve.

**There is no reattach command** — a gap, not a decision (`pd-rtjk`). `tenant
rename` is not a way round it either: it addresses a *live* user and a detached
one is not found. Until that is fixed, reviving a detached user is a hand edit
of the registry — and it is now one column, because the row never stopped
saying who they were. Resolution reads the registry per request, so nothing
needs restarting:

```bash
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-prod-data)
sqlite3 "${MP}/registry.sqlite" \
  "UPDATE user SET state='active', retired_at=NULL WHERE database_id='<id>';"
podman exec systemd-pkdump-prod pkdump tenant list                  # carol, active
```

If someone else took the handle in the meantime, the write fails rather than
merging two people onto one name (`UNIQUE constraint failed: user.handle` —
`user_one_active_handle` is unique across live users). Set a free `handle` in
the same `UPDATE`, or rename the live holder first.

Only a **purge** destroys the local copy, and even then the S3 replica outlives
it until retention expires (6 months). Recovering it is the one case that has to
say `--unattributed`, because a purge takes the registry row with it and the
attribution gate would otherwise (correctly) refuse a database nothing on the box
can name:

```bash
bash deploy/restore-litestream.sh --unattributed prod <the purged database id>
```

Without the flag it stops and tells you this is the case to use it for. With it,
the restored file is under a `database_id` that no registry row names any more,
so `pkdump tenant list` reports it as a database no registered user claims until
you decide who it belongs to. That is the intended shape: the bytes come back
before the attribution does, and neither is guessed.

## Scenario C — total box loss (rebuild from scratch)

```bash
cd ~ && git clone git@github.com:DeckDumpster/pokedumpster.git && cd pokedumpster
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
`01K2C7HQ8N….sqlite` with no way to tell whose collection is inside any of them —
every byte present, nothing attributable. The registry is the only thing that
puts the names back.

**The script enforces this.** Step 3 below fails if you skip step 1:

```
ERROR: refusing to restore 01K2C7HQ8N… — nothing on this box says whose it is.
       There is no registry.sqlite on 'pkdump-prod-data'.
```

That is the whole point of the gate: without it, skipping step 1 does not look
like a mistake. It looks like a completed recovery.

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
#   alice|01K2C7HQ8NZ0XW3V9R5M6D0ABC
#   bob  |01K2C7HQ8P41K8T2Y7Q3N5E1DEF
```

```bash
# ── STEP 3 — restore each database the registry named, then rebuild the catalog.
bash deploy/restore-litestream.sh --yes prod 01K2C7HQ8NZ0XW3V9R5M6D0ABC
bash deploy/restore-litestream.sh --yes prod 01K2C7HQ8P41K8T2Y7Q3N5E1DEF
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
#   PRE 01K2C7HQ8NZ0XW3V9R5M6D0ABC.sqlite/    <- one prefix per tenant
#   PRE 01K2C7HQ8P41K8T2Y7Q3N5E1DEF.sqlite/      strip ".sqlite/" for the id
```

**If the registry itself is gone** — the one case the order cannot save you from
— the databases are still fully recoverable, but anonymous: restore each prefix
with `--unattributed` and identify them by their contents. Assume nothing about
which is which. Typing that flag once per database is the intended friction: it
is the difference between recovering anonymous data knowingly and believing you
recovered a system. This is the failure the registry being replicated exists to
prevent, so if you are here, find out why its replica was missing before
rebuilding anything on top of it.

The data volume does not need to be prepared: the restore creates `tenants/` if
it is missing, so restoring onto a bare volume is the same command. Provisioning
(`pkdump tenant create`) is **not** part of recovery — the restored file *is* the
tenant, and `watch: true` puts it back under replication within ~5s.

---

## Verify a restore

```bash
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-prod-data)
podman exec systemd-pkdump-prod pkdump tenant list       # no (DATABASE MISSING) rows
sqlite3 "file:${MP}/tenants/${DB}.sqlite?mode=ro" 'SELECT count(*) FROM collection;'
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8090/api/sets     # 200 = serving
systemctl --user is-active pkdump-litestream-prod.service                    # backups resumed
bash deploy/backup-check.sh prod                                             # ...and are FRESH, per tenant
```

`is-active` is liveness, not freshness — the sidecar can sit `active` while
error-looping on bad credentials. `backup-check.sh` is the one that asks S3 —
**always**. It used to print a skip and exit 0 when `PKDUMP_BACKUP_PING_URL` was
unset, which is a pass to anything reading its exit status; now the ping is the
only thing that URL controls, and a stale replica fails the check whether or not
there is a monitor to tell (`pd-7f46`).

## If something is wrong

- **`refusing to restore <id> — nothing on this box says whose it is`** → the
  attribution gate. Either the registry has not been restored yet (do that
  first: `--registry`), or this database is one you **purged** and its row is
  gone by design — that is what `--unattributed` is for. Do not reach for the
  flag to get past the first case; restoring the registry takes one command and
  is what puts a name on everything else you are about to restore.
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
after each restore that the other three tenants hold exactly the data they held,
are still replicating, and still restore to their own current data. It is part of
`deploy/ci.sh`, so the runbook cannot rot silently.

Its tenants are named by opaque database ids on purpose. With `alpha.sqlite` on
disk, "restore the registry and check every tenant is reachable by handle" proves
nothing — the filename already said `alpha`. So the drill prints what the bucket
alone can tell you with the registry gone (four ids, not one name), restores the
registry first, and recovers every tenant *by handle* from what it says.

**The recovery matrix** (`pd-7f46`). A restore drilled only against a live,
first, never-renamed tenant is drilled in the one state a real box is least
likely to be in, so the four tenants are deliberately in four different states by
the time total loss hits them: one untouched, one restored twice and rolled back,
one **renamed**, and one **detached**. The drill asserts a collection comes back
after a rename (same file, same replica prefix, same recovery window — only the
label moved) and after a detach (kept, still attributable to the person who had
it, restorable), and that total loss recovers all four *under their current
handles*.

Its last section is the load-bearing negative: it restores every prefix the
bucket offers onto a registry-less volume, shows that all four come back
complete and healthy and that `pkdump tenant list` cannot name a single one of
them — and then shows the shipped script refusing to do it, without stopping a
service to say so. Recreated handles are proved separately and completely by
`tests/litestream/recreate.sh` (`pd-pm7b`), which `deploy/ci.sh` also runs.

**Last exercised: 2026-08-09** (pd-7f46), **both modes, 77/77 checks each** — the
whole matrix above, including the registry loss/restore path and the refusal.
MinIO, and the production bucket under prefix `pd-v8zf-drill/` with the real
assume-role credentials, which is also what confirms the backup role can write
the registry's prefix and not just the tenants one. The throwaway prefix was
deleted afterwards (0 objects remaining) and `prod/tenants/` was untouched
throughout. `tests/litestream/recreate.sh` passed 32/32 alongside it in both
modes, against the registry schema as of `pd-9wif`.
