"""Publish per-tenant value snapshots from the lake — for EVERY tenant.

pd-ruwh, layer 3 of the offline-lakehouse design. This is the transform tier:
it reads ``catalog.prices`` at a pinned Nessie ref, joins it against each
tenant's own collection, and writes ``collection_value_snapshot`` back into
that tenant's database.

**The bug it exists to fix** (``pd-s5yn``, verified): ``pkdump data refresh``
step 7 opens *one* tenant — whichever ``$PKDUMP_USER`` names inside the
container — and snapshots it. There is no loop. Every other tenant gets no
value history, ever, with no error. It is latent only because prod has one
tenant. Looping inside the nightly refresh is the obvious patch and the wrong
one: a refresh is O(1) catalog work on the acquisition path, and one tenant's
locked database must not be able to strand the rest of a catalog build. So
the loop lives out here instead, offline, restartable, isolated per tenant.

**The standing decision this is built on: tenant data never enters the lake.**
It is one-way. The lake is read; a tenant SQLite is opened directly and
written back. Nothing tenant-keyed is ever written to Iceberg, and the only
tenant-shaped value that reaches a lake *query* is a set of product ids used
as a read predicate (see :func:`market_prices_asof`) — a filter on what to
read, which stores nothing and leaves no tenant column in any table.

The publish contract, from the design, is the part worth being strict about:

* **idempotent** for a given ``(tenant, artefact, date, lake_ref)``. The
  delete-then-insert per ``(date, dimension)`` that ``value_history.rs``
  already does gives us this; it is preserved verbatim rather than improved.
* **a failing tenant is logged and skipped, and the run continues**, reporting
  partial success in its exit status. A run that silently half-completes is
  the failure mode of the loop this design replaces, so the summary names
  every tenant that failed and the exit status is not 0.
* the app may find the artefact **absent** and must never block on it. Nothing
  here is on the serving path; an empty chart is fine, a 500 is not.

**Backfill falls out.** ``--date`` is required and never taken from the clock,
exactly as ``prices.py`` requires ``--ingest-date``: rebuilding an older date
is the same operation as building today's, so history that was never captured
is reconstructed by pointing this job at a past date.
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path

from pyiceberg.expressions import And, EqualTo

from .catalog import catalog, head_hash
from .prices import DEFAULT_TABLE

#: What this job publishes. One artefact today; the column exists so a second
#: one does not need a schema change to record its own provenance.
ARTEFACT = "collection_value_snapshot"

#: The price series a collection is valued at. The other four ``price_type``
#: values in ``catalog.prices`` are not what ``latest_prices`` feeds the
#: collection page, so they are not what a snapshot is made of.
MARKET = "market"

#: Mirrors ``crates/pkdump-db/src/paths.rs``: ``$PKDUMP_HOME``, else
#: ``$HOME/.pkdump``. Same two names, so one data directory is described once.
HOME_ENV = "PKDUMP_HOME"

#: How long to wait for a tenant's write lock before giving up on that tenant.
#:
#: A real trade-off, so it is a named number rather than a default nobody
#: chose: long enough to ride out an ordinary write from the running server,
#: short enough that one stuck database cannot hold the whole run open. Timing
#: out is not data loss — the tenant is skipped, named, and carried in the exit
#: status, and the next run publishes them.
BUSY_TIMEOUT = 15.0

#: Provenance, in the tenant database, of every artefact this tier publishes.
#:
#: **The canonical copy of this DDL is ``crates/pkdump-db/src/schema_user.sql``**
#: — the data model is the product, and a table the application may one day
#: read belongs in the schema file with every other one. It is repeated here
#: because this job writes tenant databases the Rust binary may not have opened
#: since the table was added, and a transform that refuses to run until someone
#: else opens the file is a worse failure than an idempotent ``CREATE``.
#: ``lake_publication_ddl_matches_schema_user`` in
#: ``crates/pkdump-db/tests/lake_publication.rs`` reads both files and fails if
#: they drift, so the duplication cannot go quiet.
PUBLICATION_DDL = """\
CREATE TABLE IF NOT EXISTS lake_publication (
    artefact     TEXT NOT NULL,
    date         TEXT NOT NULL,
    lake_ref     TEXT NOT NULL,
    published_at TEXT NOT NULL,
    PRIMARY KEY (artefact, date)
)"""

#: The owned-collection projection, one row per owned copy. This is
#: ``OWNED_TODAY_SQL`` from ``crates/pkdump-db/src/value_history.rs`` with one
#: substitution: the ``latest_prices`` subquery becomes ``_lake_prices``, the
#: as-of prices this job read from Iceberg. Everything else — the
#: ``printings``/``user_printings`` union, the ``conditions`` multiplier and
#: its 1.0 default, the ``manual_prices`` COALESCE, ``status = 'owned'`` — is
#: character-for-character the same, because the gate asserts the output is
#: byte-identical and any "improvement" here would be a silent behaviour change
#: to a number the user sees.
OWNED_SQL = """\
CREATE TEMP TABLE _snap_owned AS
SELECT c.id,
       c.purchase_price,
       c.binder_id,
       cd.set_code,
       COALESCE(cond.multiplier, 1.0) AS mult,
       COALESCE(
         (SELECT lp.price FROM _lake_prices lp
            WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id
              AND lp.sub_type_name = p.sub_type_name
            LIMIT 1),
         (SELECT mp.price FROM manual_prices mp
            WHERE mp.printing_id = p.printing_id
            ORDER BY mp.observed_at DESC LIMIT 1)
       ) AS market_price
  FROM collection c
  JOIN (
         SELECT printing_id, card_id, tcgplayer_product_id, sub_type_name
           FROM shared.printings
         UNION ALL
         SELECT printing_id, card_id, NULL, NULL
           FROM user_printings
       ) p ON c.printing_id = p.printing_id
  JOIN shared.cards cd ON p.card_id = cd.card_id
  LEFT JOIN shared.conditions cond ON cond.name = c.condition
 WHERE c.status = 'owned'"""

#: The three dimension aggregates, from ``insert_dimensions`` in
#: ``value_history.rs``. Same expressions, same COALESCE defaults, same
#: grouping — see the note on :data:`OWNED_SQL`.
DIMENSION_SQL: tuple[str, ...] = (
    """\
INSERT INTO collection_value_snapshot
     (date, dimension, bucket, market_value, cost_basis, card_count)
SELECT ?, 'all', NULL,
       COALESCE(SUM(market_price * mult), 0.0),
       COALESCE(SUM(purchase_price), 0.0),
       COUNT(*)
  FROM _snap_owned""",
    """\
INSERT INTO collection_value_snapshot
     (date, dimension, bucket, market_value, cost_basis, card_count)
SELECT ?, 'set', set_code,
       COALESCE(SUM(market_price * mult), 0.0),
       COALESCE(SUM(purchase_price), 0.0),
       COUNT(*)
  FROM _snap_owned
 GROUP BY set_code""",
    """\
INSERT INTO collection_value_snapshot
     (date, dimension, bucket, market_value, cost_basis, card_count)
SELECT ?, 'binder', CAST(binder_id AS TEXT),
       COALESCE(SUM(market_price * mult), 0.0),
       COALESCE(SUM(purchase_price), 0.0),
       COUNT(*)
  FROM _snap_owned
 WHERE binder_id IS NOT NULL
 GROUP BY binder_id""",
)


class TransformError(RuntimeError):
    """The run cannot start: no data directory, no registry, no table."""


class TenantError(RuntimeError):
    """One tenant failed. Logged, skipped, and named in the summary."""


@dataclass(frozen=True)
class Tenant:
    """A registered user and the file their collection lives in."""

    database_id: str
    handle: str

    @property
    def label(self) -> str:
        return f"{self.handle} ({self.database_id})"


def data_home() -> Path:
    """The PokeDumpster data directory, resolved as ``paths.rs`` resolves it."""
    configured = os.environ.get(HOME_ENV)
    if configured:
        return Path(configured)
    home = os.environ.get("HOME")
    if not home:
        raise TransformError(f"neither {HOME_ENV} nor HOME is set")
    return Path(home) / ".pkdump"


def connect(path: Path, *, read_only: bool = False) -> sqlite3.Connection:
    """Open a PokeDumpster SQLite database.

    Not ``mode=ro``: these are WAL databases and SQLite cannot read one through
    a read-only connection at all — it needs to manage the ``-shm``.
    ``query_only`` is the honest equivalent, and is what ``verify.py`` uses for
    the same reason.
    """
    if not path.exists():
        raise TenantError(f"{path} does not exist")
    # `isolation_level=None` turns OFF the driver's implicit transaction
    # management, so `ATTACH` (which cannot run inside one) and an explicit
    # `BEGIN IMMEDIATE` both mean what they say. The transaction boundaries
    # here are deliberate; they are not the driver's to choose.
    conn = sqlite3.connect(path, timeout=BUSY_TIMEOUT, isolation_level=None)
    if read_only:
        conn.execute("PRAGMA query_only = ON")
    return conn


def active_tenants(registry: Path) -> list[Tenant]:
    """Every user whose handle is live, oldest first.

    **Active only.** A detached user kept their bytes but released their
    handle, so nothing serves their collection and a snapshot written into it
    is a write nobody can read. The count of skipped detached rows is printed
    rather than dropped silently — "every tenant" should be an assertion the
    operator can check, not a claim.
    """
    if not registry.exists():
        raise TransformError(
            f"no user registry at {registry} — this data directory predates the "
            "handle/database_id split. Run `pkdump tenant migrate` (see "
            "deploy/TENANTS.md) and re-run."
        )
    conn = connect(registry, read_only=True)
    try:
        rows = conn.execute(
            "SELECT database_id, handle, state FROM user ORDER BY database_id"
        ).fetchall()
    finally:
        conn.close()

    live = [Tenant(r[0], r[1]) for r in rows if r[2] == "active"]
    detached = sum(1 for r in rows if r[2] != "active")
    print(f"==> registry: {len(live)} active tenant(s), {detached} detached (not snapshotted)")
    return live


def owned_keys(tenant_db: Path, shared_db: Path) -> set[tuple[int, str]]:
    """The ``(product, sub_type)`` pairs one tenant's owned copies price at.

    ``printings`` is a *catalog* table, so this needs the shared database
    attached — the collection holds ``printing_id`` and nothing else. The join
    is the same one ``OWNED_SQL`` makes; it is done separately and first only
    so the lake is read once for all tenants rather than once per tenant.

    This is what bounds the lake read to the size of the collections rather
    than the size of the catalog — the same restriction
    ``value_history::backfill`` makes with its ``_owned_products`` TEMP TABLE,
    and for the same reason.
    """
    conn = connect(tenant_db, read_only=True)
    try:
        conn.execute("ATTACH DATABASE ? AS shared", (str(shared_db),))
        return {
            (int(pid), sub)
            for pid, sub in conn.execute(
                "SELECT DISTINCT p.tcgplayer_product_id, p.sub_type_name "
                "  FROM collection c "
                "  JOIN shared.printings p ON c.printing_id = p.printing_id "
                " WHERE c.status = 'owned' "
                "   AND p.tcgplayer_product_id IS NOT NULL "
                "   AND p.sub_type_name IS NOT NULL"
            )
        }
    finally:
        conn.close()


def market_prices_asof(
    cat, identifier: str, date: dt.date, wanted: set[tuple[int, str]]
) -> dict[tuple[int, str], float]:
    """Market price **as of** ``date`` for each key in ``wanted``.

    As-of, not "that day's partition", and the distinction is the whole
    correctness of this job. ``latest_prices`` — what ``snapshot_today`` reads
    — is ``MAX(observed_at)`` per key over *all* of ``prices``
    (``crates/pkdump-db/src/latest_prices.rs``), so a product last quoted three
    days ago is still in it at that older price. Reading only
    ``observed_date = date`` would drop exactly those products to a NULL market
    price and quietly value the collection low. Taking the newest observation
    at or before ``date`` is both what reproduces today's numbers and what
    makes pointing this job at a past date real backfill rather than an
    approximation of one.

    Walked newest partition first, stopping as soon as every wanted key has a
    price. In the normal case — upstream quotes essentially every product every
    day — that is one partition. The cost is bounded by the collections, never
    by the years of history behind them.
    """
    table = cat.load_table(identifier)
    partitions = sorted(
        {
            row["partition"]["observed_date"]
            for row in table.inspect.partitions().to_pylist()
        },
        reverse=True,
    )
    # Iceberg hands identity-partition dates back as `datetime.date`; compare
    # as dates so `<=` means what it says rather than string-collating.
    usable = [p for p in partitions if _as_date(p) <= date]

    found: dict[tuple[int, str], float] = {}
    scanned = 0
    for observed in usable:
        if len(found) == len(wanted):
            break
        scanned += 1
        rows = (
            table.scan(
                row_filter=And(
                    EqualTo("price_type", MARKET),
                    EqualTo("observed_date", _as_date(observed).isoformat()),
                ),
                selected_fields=("tcgplayer_product_id", "sub_type_name", "price"),
            )
            .to_arrow()
            .to_pylist()
        )
        for row in rows:
            key = (row["tcgplayer_product_id"], row["sub_type_name"])
            if key in wanted and key not in found:
                found[key] = row["price"]

    missing = len(wanted) - len(found)
    print(
        f"==> prices as of {date}: {len(found)}/{len(wanted)} owned key(s) priced "
        f"from {scanned} partition(s) of {len(usable)} at or before that date"
    )
    if missing:
        # Not an error: a product that has never been quoted has no market
        # price, and `latest_prices` would not have one either. Said out loud
        # because a collection valued lower than expected should be traceable.
        print(f"    note {missing} owned key(s) have no market price at or before {date}")
    return found


def _as_date(value) -> dt.date:
    """A partition value as a ``date``, whichever way Iceberg spelled it."""
    if isinstance(value, dt.datetime):
        return value.date()
    if isinstance(value, dt.date):
        return value
    return dt.date.fromisoformat(str(value))


def publish(
    tenant_db: Path,
    shared_db: Path,
    date: dt.date,
    prices: dict[tuple[int, str], float],
    lake_ref: str,
) -> int:
    """Compute and write one tenant's snapshot rows for ``date``.

    One transaction: either the day is replaced or it is untouched. Re-running
    with the same inputs produces the same rows, which is the idempotence the
    publish contract asks for — inherited from ``snapshot_today``'s
    delete-then-insert rather than reinvented.
    """
    day = date.isoformat()
    conn = connect(tenant_db)
    try:
        conn.execute("ATTACH DATABASE ? AS shared", (str(shared_db),))
        conn.execute("BEGIN IMMEDIATE")
        conn.execute(PUBLICATION_DDL)

        conn.execute(
            "CREATE TEMP TABLE _lake_prices ("
            "  tcgplayer_product_id INTEGER NOT NULL,"
            "  sub_type_name TEXT NOT NULL,"
            "  price REAL NOT NULL,"
            "  PRIMARY KEY (tcgplayer_product_id, sub_type_name))"
        )
        conn.executemany(
            "INSERT INTO _lake_prices (tcgplayer_product_id, sub_type_name, price) "
            "VALUES (?, ?, ?)",
            [(pid, sub, price) for (pid, sub), price in prices.items()],
        )

        conn.execute("DELETE FROM collection_value_snapshot WHERE date = ?", (day,))
        conn.execute(OWNED_SQL)
        written = 0
        for statement in DIMENSION_SQL:
            written += conn.execute(statement, (day,)).rowcount

        conn.execute(
            "INSERT INTO lake_publication (artefact, date, lake_ref, published_at) "
            "VALUES (?, ?, ?, ?) "
            "ON CONFLICT(artefact, date) DO UPDATE SET "
            "  lake_ref = excluded.lake_ref, published_at = excluded.published_at",
            (ARTEFACT, day, lake_ref, dt.datetime.now(dt.timezone.utc).isoformat()),
        )
        conn.commit()
        return written
    finally:
        # DROP before close so a reused connection cannot see a stale temp
        # table; wrapped because the interesting failure is the one above.
        try:
            conn.execute("DROP TABLE IF EXISTS _snap_owned")
            conn.execute("DROP TABLE IF EXISTS _lake_prices")
        except sqlite3.Error:
            pass
        conn.close()


def run(date: dt.date, identifier: str, *, home: Path | None = None) -> int:
    """Snapshot every active tenant. Returns the number that failed."""
    home = home or data_home()
    shared_db = home / "shared.sqlite"
    if not shared_db.exists():
        raise TransformError(
            f"no shared catalog at {shared_db} — run `pkdump setup` first. "
            "The lake holds prices; sets, cards, printings and condition "
            "multipliers still come from the catalog."
        )

    tenants = active_tenants(home / "registry.sqlite")
    if not tenants:
        print("==> no active tenants — nothing to publish")
        return 0

    branch = catalog().properties.get("prefix") or "main"
    # The provenance handle the artefact records, resolved BEFORE a byte is
    # read. A branch name alone is not a handle — it moves — so it is pinned to
    # the commit it is at, and every tenant in this run is then computed from
    # that one pinned catalog. Resolving it afterwards instead would let a
    # build landing mid-run leave the artefact stamped with a ref it was not
    # derived from. Per pd-fzeb's measurement this is also the only time-travel
    # primitive available: under Nessie the per-table Iceberg snapshot id is
    # gone from the metadata a client is handed.
    lake_ref = branch if "@" in branch else f"{branch}@{head_hash(branch)}"
    pinned = catalog(ref=lake_ref, name="lake-pinned")
    print(f"==> lake ref: {lake_ref}")

    # Pass 1: what needs pricing. A tenant that cannot be read here is failed
    # now and not opened again — the run still covers everyone else.
    failures: list[tuple[Tenant, str]] = []
    wanted: set[tuple[int, str]] = set()
    readable: list[tuple[Tenant, Path, set[tuple[int, str]]]] = []
    for tenant in tenants:
        path = home / "tenants" / f"{tenant.database_id}.sqlite"
        try:
            keys = owned_keys(path, shared_db)
        except (TenantError, sqlite3.Error) as exc:
            print(f"    !! {tenant.label}: {exc}")
            failures.append((tenant, str(exc)))
            continue
        readable.append((tenant, path, keys))
        wanted |= keys

    prices = market_prices_asof(pinned, identifier, date, wanted) if wanted else {}

    # Pass 2: publish. Each tenant is its own transaction on its own file, so
    # one locked or corrupt database cannot strand the others.
    published = 0
    for tenant, path, keys in readable:
        try:
            rows = publish(
                path,
                shared_db,
                date,
                {key: prices[key] for key in keys if key in prices},
                lake_ref,
            )
        except (TenantError, sqlite3.Error) as exc:
            print(f"    !! {tenant.label}: {exc}")
            failures.append((tenant, str(exc)))
            continue
        published += 1
        print(f"    ok {tenant.label}: {rows} snapshot row(s) for {date}")

    print(f"==> published {published}/{len(tenants)} tenant(s) for {date}")
    if failures:
        # Partial success is reported, never inferred. The whole reason this
        # tier replaced a loop inside the refresh is that the loop could
        # half-complete and say nothing.
        print(f"!! {len(failures)} tenant(s) FAILED and were skipped:")
        for tenant, reason in failures:
            print(f"     {tenant.label}: {reason}")
    return len(failures)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="pkdump-lake-value-snapshot",
        description="Publish collection_value_snapshot into every registered tenant, "
        "computed from catalog.prices in the lake.",
    )
    parser.add_argument(
        "--date",
        required=True,
        help="the snapshot date, YYYY-MM-DD. Required, and never defaulted from the clock: "
        "pointing this at an older date is backfill, and it is the same operation.",
    )
    parser.add_argument("--table", default=DEFAULT_TABLE, help=f"default {DEFAULT_TABLE}")
    args = parser.parse_args(argv)

    try:
        failed = run(dt.date.fromisoformat(args.date), args.table)
    except TransformError as exc:
        # The run could not start at all, which is a different fact from "some
        # tenants failed" and gets its own status.
        print(f"!! {exc}", file=sys.stderr)
        return 2
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
