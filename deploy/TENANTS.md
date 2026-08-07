# Tenants — layout, provisioning, and the production migration

Every tenant gets its own collection database. The card catalog stays a single
shared copy, `ATTACH`ed read-only per connection exactly as it always was.

> **Status: integration branch only.** Provisioning and request-path
> *resolution* both exist; there is **no authentication at all** (a separate
> epic). Resolution is off by default — `pkdump serve` opens the one
> collection `$PKDUMP_USER` names, exactly as it always did. Do not deploy a
> multi-tenant instance to the internet.

## Layout

```
$PKDUMP_HOME/                 # /data in the container, ~/.pkdump otherwise
  shared.sqlite               # the catalog — ONE copy, shared by every tenant
  tenants/
    collection.sqlite         # tenant `collection` — the original single user
    <tenant>.sqlite           # one file per additional tenant
```

Two things about that directory are load-bearing:

- **The catalog is not in it.** `shared.sqlite` is rebuildable from upstream
  (`pkdump setup`) and is deliberately not replicated. Keeping it out of
  `tenants/` means "every `*.sqlite` under `tenants/`" is an exact description
  of the set of things that hold irreplaceable data.
- **The whole set is one glob.** That is the shape Litestream's
  `dir:` + `pattern:` + `watch:` mode wants
  (`deep-dives/litestream-multi-db/RESULT.md` §4), where each replica path is
  *derived from the filename*. Distinct tenant names therefore give distinct
  replica prefixes by construction — which forecloses the silent cross-tenant
  substitution that the same spike demonstrated is otherwise possible (§2).

Tenant names are validated to `[a-z0-9][a-z0-9_-]{0,31}`. A name is a filename
*and* an S3 path component at the same time, so uppercase (which collides on
case-insensitive filesystems), dots, and slashes are all rejected.

## The two commands

```bash
pkdump tenant create <name>          # provision a tenant
pkdump tenant remove <name> --yes    # delete a tenant's collection
```

`create` writes `tenants/<name>.sqlite` with the user schema applied; the
tenant is usable immediately. It fails if the tenant already exists.

`remove` deletes that file and its WAL sidecars. It is the only destructive
command here — the S3 replica outlives it, but only for as long as retention
holds (6 months; `deploy/RESTORE.md`).

Also available: `pkdump tenant list`.

## Serving more than one tenant

`pkdump serve` serves exactly one collection. Which one is decided at startup
from `$PKDUMP_USER`, and no request can change it. That is the default, that is
what production runs, and with it a tenant header is not read at all — send one
and nothing happens.

Passing `--multi-tenant` (or `PKDUMP_MULTITENANT=1`) switches on per-request
resolution instead: every `/api` request must name its tenant in an
`x-pkdump-tenant` header, and is served that tenant's database.

```bash
pkdump serve --multi-tenant
curl -H 'x-pkdump-tenant: alice' localhost:8080/api/collection
```

### Read this before you turn it on

**Nothing authenticates that header.** A caller who sends
`x-pkdump-tenant: alice` *is* Alice as far as the server is concerned. There is
no login, no session, no token — identity is a separate epic that has not been
built. An instance running with this flag on and reachable by anyone but you
hands every collection to whoever asks for it.

Which is why:

- The flag is off unless explicitly set, and `PKDUMP_MULTITENANT` only counts
  `1`, `true` or `yes` as on — `PKDUMP_MULTITENANT=0` does not switch it on by
  the mere fact of being set.
- The server prints a warning line at startup when it is on.
- The mechanism is a **header**, not a hostname or a URL prefix. A browser does
  not send it on its own, so a multi-tenant instance cannot be driven by
  pointing a browser at it — the frontend is unchanged and remains
  single-tenant. Browser-reachable multi-tenancy waits on the identity epic.
- **Production stays single-tenant.** This work lives on the integration
  branch; `deploy/pkdump.container` does not set the variable and must not.

### What isolation rests on

A tenant's requests reach a connection opened against that tenant's own
database file, so another tenant's rows are not in scope for any query — there
is no `WHERE tenant_id = ?` that a route could forget. Three things hold that
up, all in `crates/pkdump-server/src/tenant.rs`:

- The application state holds **no connection**. The only way to a database is
  `blocking()`, and the only way to name one is a `TenantId` that the
  resolution middleware alone can mint.
- The resolved tenant lives in a task-local for the life of the request.
  Handlers do not pass it, so they cannot pass the wrong one.
- Opening a tenant connection asserts `pragma_database_list` holds exactly
  `main` = that tenant's file and `shared` = the catalog, and fails otherwise.

Tenant names from the header go through the same validation as provisioning,
before they touch the filesystem, and a name with no database is a 404 — the
resolver never creates one.

The load-bearing test is
`one_tenant_cannot_reach_another_tenants_collection` in
`crates/pkdump-server/src/lib.rs`. It asserts the negative — Bob cannot read,
and cannot delete, Alice's card — and it has been shown to fail when the
resolver is bypassed (see `pd-5emg`).

In a container, prefix with `podman exec`:

```bash
podman exec systemd-pkdump-prod pkdump tenant create alice
podman exec systemd-pkdump-prod pkdump tenant list
```

## Migrating the existing production database

The production data directory predates `tenants/`: its collection sits at
`/data/collection.sqlite`, beside the catalog. Migration makes it tenant
`collection` — the first tenant — by moving that one file.

`pkdump tenant adopt` does it. It is a `rename(2)` within the data directory:
no bytes are copied, and it cannot half-finish. Before renaming it checkpoints
the WAL with `PRAGMA wal_checkpoint(TRUNCATE)` so the moved file is complete on
its own, and **refuses to proceed if the checkpoint reports the database busy**
— moving a file out from under a running server leaves it writing to an
unlinked inode.

That check catches an app that is actively serving, but it cannot see an idle
open handle. **Stop the services first anyway.**

Litestream's own per-database state directory (`.collection.sqlite-litestream`,
holding the LTX cache and txid) moves with the database. It has to: left
behind, the sidecar would treat the relocated file as a brand-new database
while its S3 prefix already holds months of history.

```bash
INSTANCE=prod

# 1. Stop every writer: the app AND the Litestream sidecar.
systemctl --user stop pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}

# 2. Move the collection into the tenant layout.
podman run --rm -v pkdump-${INSTANCE}-data:/data -e PKDUMP_HOME=/data \
    --entrypoint pkdump localhost/pkdump:${INSTANCE} tenant adopt collection

# 3. Point the backup sidecar at the new path.
#    Only the DB path changes. LITESTREAM_S3_PATH stays exactly as it was, so
#    the existing replica history — and the 6-month recovery window with it —
#    carries straight over.
sed -i 's|^LITESTREAM_DB_PATH=.*|LITESTREAM_DB_PATH=/data/tenants/collection.sqlite|' \
    ~/.config/pkdump/${INSTANCE}/litestream.env

# 4. Start both back up.
systemctl --user start pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}
```

### Verify

```bash
INSTANCE=prod
# The collection is where it should be, and has its rows.
podman exec systemd-pkdump-${INSTANCE} pkdump tenant list        # -> collection
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-${INSTANCE}-data)
sqlite3 "file:${MP}/tenants/collection.sqlite?mode=ro" 'SELECT count(*) FROM collection;'
# Nothing left at the old location.
ls "${MP}/collection.sqlite" 2>&1     # -> No such file or directory
# The catalog did NOT move.
ls -l "${MP}/shared.sqlite"
# Backups are still flowing.
bash deploy/backup-check.sh ${INSTANCE}
```

### Rollback

`pkdump tenant revert` is `adopt` run backwards — the same checkpoint, the same
`rename(2)`, the opposite direction — so a build that predates the tenant
layout finds its collection exactly where it left it.

```bash
INSTANCE=prod
systemctl --user stop pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}

podman run --rm -v pkdump-${INSTANCE}-data:/data -e PKDUMP_HOME=/data \
    --entrypoint pkdump localhost/pkdump:${INSTANCE} tenant revert collection

sed -i 's|^LITESTREAM_DB_PATH=.*|LITESTREAM_DB_PATH=/data/collection.sqlite|' \
    ~/.config/pkdump/${INSTANCE}/litestream.env

# Roll the code back too — the tenant-layout build refuses to run against an
# un-adopted data dir (see below).
git -C ~/pokedumpster checkout <pre-tenant-commit> && bash deploy/deploy.sh ${INSTANCE}

systemctl --user start pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}
```

Rollback is only ever needed while the code and the data disagree. Once the
tenant-layout build is running against an adopted data dir, the relevant
recovery mechanism is the S3 replica (`deploy/RESTORE.md`), not this.

### If you forget step 2

The app will not quietly come up with an empty collection. `user_db_path`
refuses to hand out `tenants/<name>.sqlite` while an un-adopted database still
sits at the old location, and says so:

```
collection database for tenant "collection" is still at the pre-tenants
location /data/collection.sqlite and has not been adopted into /data/tenants.
Run `pkdump tenant adopt collection` (see deploy/TENANTS.md).
```

A collection silently reading as empty is the worst outcome available to this
change, so it is the one the code makes impossible.

## What is not here yet

- **Litestream config for N tenants** — `pd-fof4`. `deploy/litestream.yml` still
  names a single `LITESTREAM_DB_PATH`; the `dir:`/`watch:` mode that would pick
  up new tenants automatically is that bead's call to make.
- **Authentication** — a separate epic, not started. Until it lands, the
  `--multi-tenant` resolver believes whatever a caller claims, which is why
  nothing running it may be exposed.
- **Restoring one tenant without touching the others** — `pd-v8zf`.
  `deploy/restore-litestream.sh <instance> <tenant>` already takes the tenant
  name and now writes under `tenants/`, but the multi-tenant DR runbook is that
  bead's deliverable.
