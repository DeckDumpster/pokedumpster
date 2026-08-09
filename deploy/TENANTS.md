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
  registry.sqlite             # handle -> database_id: who owns which file
  tenants/
    <database_id>.sqlite      # one file per user, named by an opaque ULID
    <handle>.sqlite           # ...or by handle, before `pkdump tenant migrate`
```

A tenant's database is named by its `database_id`, never by the handle of the
person whose collection it holds — that is `pd-fci1`, and it is why a rename
costs one `UPDATE` and a recycled handle cannot inherit a predecessor's replica.
Handle-named files are the pre-migration shape; they still work, and
`pkdump tenant migrate` moves them over (below).

Two things about that directory are load-bearing:

- **The catalog is not in it.** `shared.sqlite` is rebuildable from upstream
  (`pkdump setup`) and is deliberately not replicated. Keeping it out of
  `tenants/` means "every `*.sqlite` under `tenants/`" is an exact description
  of the set of things that hold irreplaceable data.
- **The whole set is one glob.** That is the shape Litestream's
  `dir:` + `pattern:` + `watch:` mode wants
  (`deep-dives/litestream-multi-db/RESULT.md` §4), where each replica path is
  *derived from the filename*. Distinct `database_id`s therefore give distinct
  replica prefixes by construction — which forecloses the silent cross-tenant
  substitution that the same spike demonstrated is otherwise possible (§2). And
  because ids are never reissued, that holds across time too: no later database
  can land on a prefix an earlier one used.

## The two identifiers

A user is **two facts joined by a row** in `registry.sqlite`, not one string
doing every job. Which one you are looking at decides what you can do with it.

| | `handle` | `database_id` |
|---|---|---|
| What it is | the user's name — `alice`, `collection` | an opaque ULID — `01K2C7HQ8N3Q4E9YB5R7MDX0VT` |
| Who chooses it | a person, at `pkdump tenant create` | the registry, and nothing else |
| Where it appears | the `x-pkdump-tenant` header, `$PKDUMP_USER`, `pkdump tenant` arguments | the filename, the S3 replica prefix, `pkdump tenant purge` |
| On disk | **nowhere** | `tenants/<database_id>.sqlite` |
| Can it change? | yes — `pkdump tenant rename`, one `UPDATE`, nothing moves | never, for the life of the database |
| In the table | a mutable label; unique among **active** users only | the **PRIMARY KEY** |
| Charset | `[a-z0-9][a-z0-9_-]{0,31}`, a `CHECK` on the column | 26 characters of uppercase Crockford base32, minted here |

Read the "on disk" row twice, because it is the point of the whole design: **a
handle is never a path component.** It is a lookup key and only a lookup key,
so nothing an unauthenticated caller sends is concatenated into a filename, and
a handle someone released cannot inherit its predecessor's file or replica —
the successor gets a new id, so a new file and a new prefix, by construction
(`pd-fci1`, closing `pd-pm7b`).

The handle charset is still narrow, because a handle is a name people type and
a narrow one keeps `alice` and `Alice` from being two users. But it is no longer
load-bearing for safety: a hostile handle is refused for not being in the table,
not for its characters.

Both rules live **in the schema**, not in the accessor — `database_id` is the
primary key, the charset is a `CHECK` on `handle`, and "one live user per
handle" is `CREATE UNIQUE INDEX user_one_active_handle ON user(handle) WHERE
state = 'active'`. So they hold for every writer: the app, a migration, and an
operator with `sqlite3` open on `registry.sqlite`.

## The commands

```bash
pkdump tenant create <handle>            # register a user; mint an id; write the database
pkdump tenant list                       # who is registered, and which file is theirs
pkdump tenant rename <from> <to>         # one column; nothing on disk moves
pkdump tenant detach <handle> --yes      # release the handle, KEEP the collection
pkdump tenant purge <database-id> --yes  # destroy a detached collection. Irreversible.
```

`create` mints a `database_id`, writes `tenants/<database_id>.sqlite` with the
user schema applied, and registers the handle against it. The tenant is usable
immediately. It fails if the handle is already taken.

`rename` writes one column. The database keeps its id, therefore its filename,
therefore its Litestream prefix and every LTX file already under it — a user
changing their name costs no replication history. This is the capability the
split buys, and it was impossible while the name *was* the path.

`detach` frees the handle and keeps everything else (see below). `purge` is the
only destructive command here, and it takes an **id**, not a name.

All of them read `$PKDUMP_HOME`, so against a running instance prefix with
`podman exec`:

```bash
podman exec systemd-pkdump-prod pkdump tenant create alice
podman exec systemd-pkdump-prod pkdump tenant list
```

### ⚠️ `tenant remove` no longer deletes anything

**`pkdump tenant remove <handle> --yes` is now an alias for `detach`.** It used
to unlink the database. It does not any more:

| | before | now |
|---|---|---|
| the handle | freed | freed |
| the collection database | **deleted** | **kept** |
| the S3 replica | left to expire with retention | kept |
| registry row | n/a | kept, still naming them, so the bytes stay attributable |

Every script and every piece of muscle memory that says `tenant remove alice
--yes` still parses and still succeeds — and now means something else. That is
deliberate (it fails safe, and it is gated by
`remove_is_an_alias_for_detach` in `crates/pkdump-cli/src/tenant.rs`), but it is
exactly the kind of silent change that bites at 3am, so: **if you ran `remove`
expecting the disk space back, you did not get it.** The command tells you so
and prints the second step:

```
Detached alice. The handle is free; the collection was KEPT.
  database 01KZHQVMSVRD4WPNTG9WWRXYC0 at /data/tenants/01KZHQVMSVRD4WPNTG9WWRXYC0.sqlite
  to destroy it: pkdump tenant purge 01KZHQVMSVRD4WPNTG9WWRXYC0 --yes
```

Hard deletion is that second command, and three things about it are on purpose:

- It takes a **`database_id`**, not a handle. A purge should not be reachable by
  mistyping a live person's name.
- It **refuses an active user** — detach first. So a purge is always the second
  half of a decision already taken, never the whole of one:

  ```
  Error: conflict: user "collection" is still active — `pkdump tenant detach collection` first
  ```
- It removes the file, its WAL sidecars, and Litestream's local state directory
  for it. The **S3 replica outlives it** until retention expires (6 months;
  `deploy/RESTORE.md`), so a purge is recoverable for that window and permanent
  after it.

Why the inversion: while a name was a filename, the retention window was a
liability — recreate `alice` and the new database sat inside the old one's
replica stream (`pd-pm7b`). With ids, the same window is a safety net, and the
default meaning of "remove" can afford to be the reversible half.

**Reversible in the sense that nothing is destroyed — not in the sense that one
command puts it back.** There is no `tenant attach` yet, and `tenant rename` is
not it: rename addresses a *live* user, and a detached one is not found.
`pd-rtjk` tracks the gap; `deploy/RESTORE.md`, scenario B2, has the hand edit in
the meantime — one column, `state`, because the row never stopped saying who
they were.

## Which file is whose

**A directory listing no longer tells you who exists.** Under opaque ids,
`ls tenants/` answers *how many* collections there are and nothing else. The
registry is the only thing that answers *whose*, so this is an operator step
rather than something you read off the disk:

```bash
podman exec systemd-pkdump-prod pkdump tenant list
```
```
HANDLE      DATABASE ID                 CREATED                              STATE     RETIRED                              SCHEMA  STATUS
collection  01KZHQVMMS4CSRASCMG5XRCPC3  2026-08-08T22:28:21.785088095+00:00  active    -                                         1  current
alicia      01KZHQVMQ6GVFQF1R5T7AW1K5X  2026-08-08T22:28:21.862295795+00:00  active    -                                         -  DATABASE MISSING
bob         01KZHQVMSVRD4WPNTG9WWRXYC0  2026-08-08T22:28:21.947244169+00:00  detached  2026-08-09T04:11:07.204416512+00:00        0  behind this build's 1 — adopted on its next open

1 database(s) under /data/tenants that no registered user claims:
  orphan.sqlite
```

It reads the registry, not the directory, and it reports both directions of
disagreement between them — which is what makes it worth running before
anything destructive:

- **`(DATABASE MISSING)`** beside a row — the registry names a database that is
  not on this volume. Restore it by that id (`deploy/RESTORE.md`). Do not try to
  provision your way out: `pkdump tenant create` refuses a handle that is already
  registered (`conflict: handle "alicia" is already registered`), and detaching
  first so it succeeds would mint a *new* id — a fresh empty collection, with the
  real bytes stranded under the old one.
- **"database(s) … that no registered user claims"** — files no row accounts
  for. Either handle-named databases from before `pkdump tenant migrate`, or an
  orphan worth understanding before you delete it. `purge` will not touch these:
  it resolves its argument through the registry first
  (`not found: no user with database id …`), so a file nothing claims has to be
  removed by hand, deliberately.

The `detached` rows are the other half of the answer. **A released handle keeps
its row, and the row keeps the person's real name** — `RETIRED` says when they
let it go. The handle is free the instant it is released all the same, because
the uniqueness rule is a *partial* index: `user(handle) WHERE state = 'active'`.
One live bob, any number of retired ones, and no composite string to parse to
tell which is which. Bytes that belong to nobody in particular still belong to
*someone identifiable*, which is the difference between a purge you can justify
and a file you deleted because you could not tell what it was.

So a handle in this table is not a key on its own. `WHERE handle = 'bob'` can
return several rows; the live one is `WHERE handle = 'bob' AND state = 'active'`,
and `database_id` — the primary key — is what addresses a database.

If the registry itself is gone, this question has no answer from the box alone.
That is why it is in the replicated set and why it is restored **first**:
`deploy/RESTORE.md`, scenario C.

## Reading the drift: `pkdump tenant list`

One database per user means they can legitimately hold different schema
versions: a user created today carries this build's, one restored from a
replica carries whatever it had when it was replicated, and one left over from
before `PRAGMA user_version` landed carries 0. The `SCHEMA` and `STATUS`
columns of the listing above are where that spread is visible — the version
read off each user's own file, and where it stands relative to the running
build.

*behind* is not a problem to fix by hand — the schema is re-applied and the
version stamped on that database's next open. *ahead* is the row that matters:
that database was written by a newer build and this one **refuses to open it**
(the gate in `crates/pkdump-db/src/schema_version.rs`; the server will not
start for that user). Run the newer build, or restore that database from a
replica taken before the upgrade.

A `-` in `SCHEMA` is the third case, and it is not a version at all: the
registry names a database that is not on this box (`DATABASE MISSING`). A row
with no bytes has no version, and reporting that as `0` would file real drift
under "behind, adopted on its next open".

Reading is not opening. `list` reads each file's header directly, so it neither
stamps nor applies schema — and it still reports a user the server itself
refuses, which is the case an operator is usually running it for.

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

- **The server refuses to start** with the flag on and a bind address that is
  not loopback. Not a warning — a refusal; see below.
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

### The refusal

Everything above except the first bullet is a convention plus a printed line.
The refusal is the mechanism:

```
$ pkdump serve --multi-tenant --host 0.0.0.0
Error: refusing to start: multi-tenant resolution is on and --host 0.0.0.0 is
not loopback.
...
```

Loopback (`127.0.0.1`, `::1`) still starts — that is the mode's intended shape:
a developer, a demo, or an SSH/WireGuard tunnel that does the reaching.

If you genuinely mean to expose it — behind a reverse proxy that authenticates
*for* it, once that exists — say so a second time:

```bash
PKDUMP_MULTITENANT=1 PKDUMP_MULTITENANT_INSECURE_BIND=1 pkdump serve --host 0.0.0.0
```

That second variable is parsed by the same strict helper as the first: only
`1`, `true` or `yes`; `PKDUMP_MULTITENANT_INSECURE_BIND=0` does not open it.
There is no `--allow-insecure-bind` flag — this should not be one
tab-completion away.

**Single-tenant mode is unaffected at any address.** Its tenant is fixed at
startup and no request can change it, which is why the container entrypoint's
`--host 0.0.0.0` is fine and stays. The refusal only ever fires on the
combination that has no defence. Note the consequence for containers: an image
binds `0.0.0.0`, so *any* containerised multi-tenant instance needs the second
opt-in — a container publishing a port is off-box reachable, and that is
precisely the case being caught.

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

**The header is a lookup key, not a filename.** What a request names is a
*handle*; what it is served from is `tenants/<database_id>.sqlite`, and the two
are joined by a row in the user registry (`registry.sqlite`, see
`crates/pkdump-db/src/registry.rs`) rather than by string equality. Resolution
is a `SELECT` with the header as a bound parameter, and the only string that
reaches a path constructor is the `database_id` that lookup returned — which
only the registry mints, and which `pkdump_db::tenant_db_file` re-checks is a
ULID before it becomes a path. A handle that is not registered, and one whose
registration was detached, are the same 404; neither creates anything. Nothing
an unauthenticated caller sends is concatenated into a filename.

**The header is validated all the same, and gets a 400.** Not to protect the
path — see above, no path is built from it — but because a boundary that
accepts anything answers wrongly. `pkdump tenant create` holds a handle to
`[a-z0-9][a-z0-9_-]{0,31}` and `schema_registry.sql` stores it under a `CHECK`
of the same rule, so a header outside that rule names something no row could
ever have held. Replying "404 no such tenant" would state that a well-formed
name is unused, which is false, and would leave the caller nothing to fix.
So resolution answers in three:

| what was sent | answer |
| --- | --- |
| not a handle (`Alice`, `../shared`, `a b`, empty, non-ASCII) | **400**, quoting the rule and not the value |
| a handle nobody actively holds (never registered, or detached) | **404** |
| an active user's handle | their `database_id`'s database |

The rule is written once, in `pkdump_db::HANDLE_RULE` beside
`validate_tenant_name`, and its two enforcers — that function and the SQL
`CHECK` — are held to one shared corpus (`paths::HANDLE_CASES`) by a test on
each side, so they cannot drift into disagreeing about what a handle is.
`pd-4g7c`; `pd-rqgv` is why the 404 half is safe without any of it.

Validation is necessary and nowhere near sufficient: the header is still
*asserted* identity, which is what the warning above is about. The auth epic
replaces it with a verified principal, with any header demoted to a selector
checked against that principal's entitlements.

> A database still sitting at `tenants/<handle>.sqlite` names nobody in the
> registry, so its handle is a 404 to the resolver until `pkdump tenant
> migrate` puts it on an id — see "Migrating onto opaque database ids" below.
> Single-tenant serving is unaffected either way: it does not resolve.

The load-bearing test is
`one_tenant_cannot_reach_another_tenants_collection` in
`crates/pkdump-server/src/lib.rs`. It asserts the negative — Bob cannot read,
and cannot delete, Alice's card — and it has been shown to fail when the
resolver is bypassed (see `pd-5emg`).

`tests/tenants/handles.sh` is the container-tier half: the shipped image with
resolution on, and curl reading the status line for each row of the table
above. The distinction is a status code, so a 400 the middleware flattened into
a 404 on the way out would pass every unit test in the crate and fail every
caller. It also asserts the case production actually runs — with the flag off,
the header is not read, so a malformed one is served exactly like any other
request.

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

### Backups across the migration — read this first

The sidecar no longer replicates one named database. `deploy/litestream.yml`
watches `tenants/` and **derives** each tenant's replica prefix from its
filename, which is what makes adding a tenant free and a cross-tenant prefix
collision impossible (`pd-fof4`). The cost is that tenant `collection` gets a new
prefix:

```
before   s3://<bucket>/prod/collection
after    s3://<bucket>/prod/tenants/collection.sqlite
```

**The retention policy does not change** — still `interval: 24h`,
`retention: 4320h`, still a 180-day window. But the *new* prefix's history starts
at cutover, so for the first 180 days after the migration, recovery splits in
two:

- **After the cutover** → `bash deploy/restore-litestream.sh prod collection`,
  which reads the derived prefix.
- **Before the cutover** → the old prefix, addressed directly by URL. Nothing
  writes to it any more and nothing prunes it, so it stays exactly as deep as it
  was on the day you cut over:

  ```bash
  set -a; . ~/.config/pkdump/prod/litestream.env; set +a
  D=$(mktemp -d); chmod 777 "$D"
  podman run --rm --user 0 -v "$D:/out" \
      -v ~/.config/pkdump/prod/aws/config:/aws/config:ro \
      --secret pkdump-prod-s3-bootstrap,type=mount,target=/aws/credentials \
      -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
      -e AWS_PROFILE=pkdump docker.io/litestream/litestream:latest \
      restore -integrity-check full -timestamp 2026-07-01T00:00:00Z -o /out/old.sqlite \
      "s3://${LITESTREAM_S3_BUCKET}/prod/collection?region=${LITESTREAM_S3_REGION}"
  sqlite3 "$D/old.sqlite" 'SELECT count(*) FROM collection;'
  ```

  Verified against the live production replica on 2026-08-07: point-in-time
  restores at 60, 37 and 6 days back returned 4600 / 4622 / 4763 rows.

**Do not try to migrate the history by copying the prefix.** `aws s3 cp` moves
all 618 objects and a *latest* restore from the copy succeeds with a passing
integrity check — but every `-timestamp` restore against it fails with
`timestamp does not exist`, because Litestream resolves point-in-time from the
S3 object's `LastModified` and a copy resets it to the copy time. A copied
prefix looks like a working backup and has silently lost its recovery window,
which is worse than leaving the original where it is. Tested 2026-08-07.

Once the new prefix is 180 days deep, the old one is redundant and can be
deleted.

```bash
INSTANCE=prod

# 1. Stop every writer: the app AND the Litestream sidecar.
systemctl --user stop pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}

# 2. Move the collection into the tenant layout.
podman run --rm -v pkdump-${INSTANCE}-data:/data -e PKDUMP_HOME=/data \
    --entrypoint pkdump localhost/pkdump:${INSTANCE} tenant adopt collection

# 3. Point the backup sidecar at the tenants DIRECTORY (see "Backups" below —
#    this step changes the replica prefix, and that has consequences).
sed -i -e 's|^LITESTREAM_DB_PATH=.*|LITESTREAM_TENANTS_DIR=/data/tenants|' \
       -e "s|^LITESTREAM_S3_PATH=.*|LITESTREAM_S3_PATH=${INSTANCE}/tenants|" \
    ~/.config/pkdump/${INSTANCE}/litestream.env
grep -q '^LITESTREAM_S3_ENDPOINT=' ~/.config/pkdump/${INSTANCE}/litestream.env \
    || echo 'LITESTREAM_S3_ENDPOINT=' >> ~/.config/pkdump/${INSTANCE}/litestream.env

# 4. Start both back up.
systemctl --user start pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}
```

### Verify

```bash
INSTANCE=prod
# The collection is where it should be, and has its rows.
podman exec systemd-pkdump-${INSTANCE} pkdump tenant list        # -> collection, schema current
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-${INSTANCE}-data)
sqlite3 "file:${MP}/tenants/collection.sqlite?mode=ro" 'SELECT count(*) FROM collection;'
# Nothing left at the old location.
ls "${MP}/collection.sqlite" 2>&1     # -> No such file or directory
# The catalog did NOT move.
ls -l "${MP}/shared.sqlite"
# Backups are still flowing. This step is only evidence because backup-check.sh
# now FAILS when it cannot verify (pd-1717): on 2026-08-08 this exact command
# printed "skipping", exited 0, and was read as a pass while Litestream sat
# ACTIVE with txid.replica pinned at zero. Non-zero exit here means the
# migration did not finish, whatever `systemctl is-active` says.
bash deploy/backup-check.sh ${INSTANCE}
# And the whole alarming picture, if this instance is meant to be armed.
bash deploy/alarm-status.sh ${INSTANCE}
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

# Restore the pre-tenants backup target too (single DB, original prefix).
sed -i -e 's|^LITESTREAM_TENANTS_DIR=.*|LITESTREAM_DB_PATH=/data/collection.sqlite|' \
       -e "s|^LITESTREAM_S3_PATH=.*|LITESTREAM_S3_PATH=${INSTANCE}/collection|" \
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
refuses to hand out `tenants/<handle>.sqlite` while an un-adopted database still
sits at the old location, and says so:

```
collection database for tenant "collection" is still at the pre-tenants
location /data/collection.sqlite and has not been adopted into /data/tenants.
Run `pkdump tenant adopt collection` (see deploy/TENANTS.md).
```

A collection silently reading as empty is the worst outcome available to this
change, so it is the one the code makes impossible.

## Migrating onto opaque database ids

The migration above puts the collection at `tenants/collection.sqlite` — named
by the *handle* of the user whose collection it is. This one puts it on an
opaque `database_id`, so the handle stops being a filename at all:

```
before   tenants/collection.sqlite
after    tenants/01K2C7HQ8N3Q4E9YB5R7MDX0VT.sqlite   + a row in registry.sqlite
```

`pkdump tenant migrate` does it: for every handle-named database under
`tenants/`, it registers the handle, mints a `database_id`, and renames the file
to match. Same mechanics as `adopt` — a `rename(2)` after a
`wal_checkpoint(TRUNCATE)` that **refuses if the database is busy** — one
transaction per database, so an interruption leaves the ones already done done.

It is **idempotent**: a second run finds no handle-named files and does nothing.

### This is not a startup gate, deliberately

`pkdump serve` serves an un-migrated data directory exactly as it finds it, and
prints which database it opened and that it is not on the model yet:

```
pkdump: tenant "collection" -> /data/tenants/collection.sqlite (named by handle, NOT YET MIGRATED)
warning: tenant "collection" is served from /data/tenants/collection.sqlite, which is named by handle …
```

A required migration is what took production down on the first automated deploy
of the previous epic (`pd-uoph`), and production is single-tenant. So this one
does not gate startup: the box keeps running and migrates when you choose.

What the app still refuses is coming up **empty**. A registry row naming a
database that is not on disk is a startup failure with the id in the message,
not a fresh empty collection — and neither is a handle that nothing on this
volume knows about.

### Backups across this migration — read this first

The file is **renamed**, and `deploy/litestream.yml` derives each replica prefix
from the filename. So the rename does not move a replica; it starts a new, empty
one:

```
before   s3://<bucket>/prod/tenants/collection.sqlite
after    s3://<bucket>/prod/tenants/01K2C7HQ8N3Q4E9YB5R7MDX0VT.sqlite
```

Same split as the `adopt` cutover: recovery **after** the cutover reads the new
prefix (`deploy/restore-litestream.sh prod <database-id>` — it takes the id, and
`pkdump tenant list` is what tells you whose it is); recovery **before** it
addresses the old prefix by URL, which stops advancing and keeps everything it
had. Do not try to copy the history across — `aws s3 cp` resets `LastModified`
and every `-timestamp` restore against the copy fails.

**Litestream's per-database state directory is removed by the rename, not moved
with it.** That is the opposite of what `adopt` does, and the difference is
whether the *filename* changes: `adopt` keeps the name, this changes it. Carrying
the state across a prefix change is exactly what left production replicating
nothing while the unit reported healthy — `txid.replica` stuck at
`0000000000000000` while `txid.db` climbed (`pd-1717`).

```bash
INSTANCE=prod

# 1. Stop every writer: the app AND the Litestream sidecar.
systemctl --user stop pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}

# 2. Look before you leap.
podman run --rm -v pkdump-${INSTANCE}-data:/data -e PKDUMP_HOME=/data \
    --entrypoint pkdump localhost/pkdump:${INSTANCE} tenant migrate --dry-run

# 3. Migrate. It prints the handle -> database_id mapping it created; that
#    mapping is the only thing that says which ULID-named file is whose.
podman run --rm -v pkdump-${INSTANCE}-data:/data -e PKDUMP_HOME=/data \
    --entrypoint pkdump localhost/pkdump:${INSTANCE} tenant migrate

# 4. Start both back up. No config edit: the sidecar watches the DIRECTORY and
#    derives the new prefix from the new filename by itself.
systemctl --user start pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}
```

### Verify

```bash
INSTANCE=prod
# Who is registered, and where. The handle is no longer readable off the disk,
# so this is the only thing that answers it.
podman exec systemd-pkdump-${INSTANCE} pkdump tenant list
DB_ID=$(podman exec systemd-pkdump-${INSTANCE} pkdump tenant list \
        | awk '$1=="collection" && $4=="active"{print $2}')

# The collection is in the file that id names, and has its rows.
MP=$(podman volume inspect -f '{{.Mountpoint}}' pkdump-${INSTANCE}-data)
sqlite3 "file:${MP}/tenants/${DB_ID}.sqlite?mode=ro" 'SELECT count(*) FROM collection;'
# Nothing left under the old name, and no Litestream state under either name.
ls "${MP}/tenants/collection.sqlite" 2>&1                    # -> No such file
ls -d "${MP}/tenants/.collection.sqlite-litestream" 2>&1     # -> No such file
# The app came up on the registered database, with no un-migrated warning.
journalctl --user -u pkdump-${INSTANCE} -n 20 | grep 'pkdump: tenant'

# BACKUPS — the step that matters, and the one the last migration skipped.
# `is-active` and "snapshot complete" both said healthy while nothing replicated.
# Watch txid.replica: zero on the first sync after a rename is expected, but it
# must move off zero and converge on txid.db.
journalctl --user -u pkdump-litestream-${INSTANCE} -n 50 | grep 'replica sync'
bash deploy/backup-check.sh ${INSTANCE}
```

### Rollback

`pkdump tenant unmigrate` is the inverse: every registered user's database goes
back to `tenants/<handle>.sqlite` and their registry row is dropped, so a build
predating the registry finds what it expects. Detached users are left as they
are and reported — a released handle is not a filename to give a database back.

```bash
INSTANCE=prod
systemctl --user stop pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}
podman run --rm -v pkdump-${INSTANCE}-data:/data -e PKDUMP_HOME=/data \
    --entrypoint pkdump localhost/pkdump:${INSTANCE} tenant unmigrate
systemctl --user start pkdump-${INSTANCE} pkdump-litestream-${INSTANCE}
```

The rollback renames too, so it changes the replica prefix back and the same
backup warning applies in reverse — check `txid.replica` afterwards.

`tests/tenants/upgrade.sh` runs this whole sequence in CI against the shipped
image: an old-layout volume, migrated, rolled back, and migrated again, with the
served collection asserted byte-identical at every step.

## Recovering one tenant

`deploy/RESTORE.md`, "Restoring ONE tenant while the others stay live".
`deploy/restore-litestream.sh <instance> <tenant>` restores exactly one tenant
from its own derived prefix; `tests/litestream/drill.sh` runs that procedure in
CI — in place, in time, and onto a bare volume — and asserts the other tenants
come out byte-identical.

## Recreating a handle someone else used to have

A released handle can be registered again immediately, and the new user gets a
new `database_id` — so a new file, and a new S3 replica prefix. The predecessor's
collection stays where it was, under the id that names it, until it is purged and
its retention expires. Nothing about the new user addresses it.

That is the whole reason a handle stopped being a filename, so it is gated rather
than asserted: `tests/litestream/recreate.sh` creates a user, writes a
recognisable card, removes her, purges her local database, creates the handle
again, and shows that no restore of the second user — latest, or point-in-time at
the exact instant the first user's card was live — produces that card, while the
card is demonstrably still in the bucket and still healthy under the old id. The
same script replicates a handle-named database beside it and shows the old
addressing handing the deleted user's card straight back, so the absence in the
first half means something. It runs in `deploy/ci.sh`.

## What is not here yet

- **Authentication** — a separate epic, not started. Until it lands, the
  `--multi-tenant` resolver believes whatever a caller claims, which is why
  nothing running it may be exposed.
