"""Per-tenant collection value snapshots, computed from the lake.

pd-ruwh. This is the transform tier's first job: it reads ``catalog.prices``
at a **pinned Nessie commit**, joins it against each tenant's own collection,
and writes ``collection_value_snapshot`` back into **that tenant's** database.

## The bug it exists to fix

``pkdump data refresh`` step 7 calls ``value_history::snapshot_today`` on the
**one** collection ``$PKDUMP_USER`` resolves to. There is no loop. Every other
registered tenant gets no value history, ever, and nothing errors — the run
looks perfectly successful (pd-s5yn). Latent only because prod has one tenant.
So the unit of work here is *the registry*, not *the current user*: the loop is
the feature.

## What it does not do

**Tenant data never enters the lake.** The lake holds catalog data only —
nothing keyed by tenant, ever. Prices come *out* of Iceberg; the collection is
read from, and the snapshot written to, the tenant's SQLite file directly, and
neither ever travels the other way. Someone reading this to add "just the
collection totals" to a lake table is the thing that decision was written
against.

## Why the price is "latest as of the date" rather than "the date's partition"

``snapshot_today`` reads ``latest_prices``, which is one row per (product,
sub_type, source, price_type) at its newest ``observed_at`` — a product that
was not quoted today keeps the last price anyone saw for it. Taking a single
``observed_date`` partition instead would silently value those copies at zero,
so this job reduces every partition at or before ``--date`` to the newest quote
per (product, sub_type). Which is also what makes **backfill** correct: point
it at an older date and it reconstructs what the collection was worth *then*,
not what it is worth now.

## The publish contract

* **Idempotent** for a given (tenant, artefact, date, lake_ref). The rows are a
  delete-then-insert per date, exactly as ``snapshot_today`` does it, and the
  provenance row is replaced with them.
* **A failing tenant is logged and skipped; the run continues** and says so in
  its exit status — :data:`EXIT_PARTIAL`. A run that half-completes and reports
  success is the failure mode of the missing loop this job replaces;
  reintroducing it in a new shape would be worse than leaving the bug.
* **The app must never block on the artefact.** Nothing here is on the serving
  path, and ``GET /api/collection/value-history`` reads an empty table as an
  empty chart. This job being absent, late, or half-done is not an outage.

Run it against a data directory the app is not currently writing::

    pkdump-lake-value-snapshots --date 2026-08-11 --data-dir /data

"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path

from pyiceberg.expressions import And, EqualTo, LessThanOrEqual

from .catalog import DEFAULT_REF, REF_ENV, catalog, head_hash

#: The lake table this job reads. Its grain is (product, sub_type, price_type,
#: observed_date) — see :mod:`pkdump_lake.prices`.
DEFAULT_TABLE = "catalog.prices"

#: The one price type a collection is valued at, matching the collection page
#: and ``value_history.rs``.
PRICE_TYPE = "market"

#: Where the databases live when ``--data-dir`` is not given. Same default as
#: ``pkdump_db::paths::pkdump_home``.
HOME_ENV = "PKDUMP_HOME"
DEFAULT_HOME = "~/.pkdump"

#: Layout owned by ``pkdump_db::paths`` — the registry at the data root, one
#: file per tenant under ``tenants/`` named by opaque database id. Duplicated
#: here because this job is not a Rust program; ``tests/lake/value_snapshots.sh``
#: builds its fixture with the REAL Rust provisioning and then runs this job
#: against it, so a layout change breaks the gate rather than production.
REGISTRY_FILE = "registry.sqlite"
TENANTS_DIR = "tenants"

#: All tenants snapshotted.
EXIT_OK = 0
#: The run could not start at all: no registry, no lake, bad arguments.
EXIT_FAILED = 1
#: The run completed, and at least one tenant was skipped. Deliberately its own
#: code: "some tenants have no value history today" must not read as success,
#: and must not read as "the job did not run" either.
EXIT_PARTIAL = 2

#: The per-copy projection, one row per owned copy, carrying its set, binder,
#: purchase price, condition multiplier and current market price.
#:
#: A transliteration of ``OWNED_TODAY_SQL`` in
#: ``crates/pkdump-db/src/value_history.rs``, with two substitutions and no
#: other change:
#:
#: * ``latest_prices`` becomes ``_lake_prices`` — the same shape, staged from
#:   Iceberg below, which is the entire point of the rewrite.
#: * the catalog tables are ``shared.``-qualified. The Rust path exposes them
#:   through TEMP VIEWs over the same ATTACH; this job just names them.
#:
#: The two implementations must agree to the byte, and are held to it: the
#: container gate runs Rust ``snapshot_today`` and this job over one fixture
#: and compares the rows.
OWNED_SQL = """
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
 WHERE c.status = 'owned';
"""

#: The three aggregates, in the order ``value_history::insert_dimensions``
#: writes them. Same expressions, same COALESCE, same CAST — see OWNED_SQL.
DIMENSION_SQL = (
    """
    INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
    SELECT ?, 'all', NULL,
           COALESCE(SUM(market_price * mult), 0.0),
           COALESCE(SUM(purchase_price), 0.0),
           COUNT(*)
      FROM _snap_owned
    """,
    """
    INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
    SELECT ?, 'set', set_code,
           COALESCE(SUM(market_price * mult), 0.0),
           COALESCE(SUM(purchase_price), 0.0),
           COUNT(*)
      FROM _snap_owned
     GROUP BY set_code
    """,
    """
    INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
    SELECT ?, 'binder', CAST(binder_id AS TEXT),
           COALESCE(SUM(market_price * mult), 0.0),
           COALESCE(SUM(purchase_price), 0.0),
           COUNT(*)
      FROM _snap_owned
     WHERE binder_id IS NOT NULL
     GROUP BY binder_id
    """,
)


class TransformError(RuntimeError):
    """The run cannot start. Not the same thing as one tenant failing."""


@dataclass(frozen=True)
class Tenant:
    """A registered user and the file their collection lives in."""

    handle: str
    database_id: str
    path: Path


@dataclass
class Outcome:
    """What happened to one tenant."""

    tenant: Tenant
    rows: int = 0
    error: str | None = None

    @property
    def ok(self) -> bool:
        return self.error is None


def data_dir(explicit: str | None = None) -> Path:
    """The pkdump data directory: ``--data-dir``, ``$PKDUMP_HOME``, or the default."""
    return Path(explicit or os.environ.get(HOME_ENV) or DEFAULT_HOME).expanduser()


def tenants(root: Path, only: list[str] | None = None) -> list[Tenant]:
    """Every **active** user in the registry, in registry order.

    The loop's input, and the reason this job exists. A detached user is not
    included: their handle has been released and their database is kept for
    attribution, not for serving.
    """
    registry = root / REGISTRY_FILE
    if not registry.exists():
        raise TransformError(
            f"no registry at {registry} — this data directory predates the opaque-id "
            "layout. Run `pkdump tenant migrate` (see deploy/TENANTS.md) and re-run."
        )
    # Not `mode=ro`: the registry is a WAL database (`open_registry`) and SQLite
    # cannot open one read-only at all — it has to be able to manage the -shm.
    # `query_only` is the honest equivalent, and the same thing verify.py does.
    conn = sqlite3.connect(registry)
    try:
        conn.execute("PRAGMA query_only = ON")
        rows = conn.execute(
            "SELECT handle, database_id FROM user WHERE state = 'active' ORDER BY created_at, handle"
        ).fetchall()
    finally:
        conn.close()

    found = [
        Tenant(handle, database_id, root / TENANTS_DIR / f"{database_id}.sqlite")
        for handle, database_id in rows
    ]
    if only is None:
        return found

    by_handle = {t.handle: t for t in found}
    missing = [handle for handle in only if handle not in by_handle]
    if missing:
        raise TransformError(
            f"no active user for handle(s) {', '.join(missing)} — `pkdump tenant list` "
            "shows who is registered"
        )
    return [by_handle[handle] for handle in only]


def pin(ref: str | None) -> str:
    """The ref to read, resolved to a single commit **once** for the whole run.

    Every tenant is then valued from the same catalog state. Pinning per tenant
    would let a concurrent lake write land mid-run and leave two tenants'
    snapshots for one date derived from two different catalogs — a difference
    nothing downstream could ever explain.
    """
    ref = ref or os.environ.get(REF_ENV) or DEFAULT_REF
    if "@" in ref:
        return ref
    return f"{ref}@{head_hash(ref)}"


def market_prices(identifier: str, date: dt.date, lake_ref: str) -> dict[tuple[int, str], float]:
    """The newest market price per (product, sub_type) at or before ``date``.

    Read in batches and reduced as they arrive: the scan is every partition up
    to ``date``, which grows without bound as the lake accumulates days, while
    the result is bounded by the product count. Materializing the scan first
    would make the job's memory a function of how long the lake has existed.
    """
    table = catalog(lake_ref).load_table(identifier)
    scan = table.scan(
        row_filter=And(
            EqualTo("price_type", PRICE_TYPE),
            LessThanOrEqual("observed_date", date.isoformat()),
        ),
        selected_fields=("tcgplayer_product_id", "sub_type_name", "price", "observed_date"),
    )

    latest: dict[tuple[int, str], tuple[dt.date, float]] = {}
    batches = 0
    for batch in scan.to_arrow_batch_reader():
        batches += 1
        columns = batch.to_pydict()
        for product_id, sub_type, price, observed in zip(
            columns["tcgplayer_product_id"],
            columns["sub_type_name"],
            columns["price"],
            columns["observed_date"],
        ):
            key = (product_id, sub_type)
            seen = latest.get(key)
            if seen is None or observed >= seen[0]:
                latest[key] = (observed, price)
    print(f"    {len(latest)} product/sub-type price(s) from {batches} batch(es)")
    return {key: price for key, (_, price) in latest.items()}


def stage_prices(conn: sqlite3.Connection, prices: dict[tuple[int, str], float]) -> int:
    """Stage the prices this tenant can actually use into ``_lake_prices``.

    Restricted to the products the tenant owns a copy of — a collection touches
    a few thousand products out of the catalog's hundreds of thousands, and the
    join in :data:`OWNED_SQL` cannot tell the difference.
    """
    conn.execute("DROP TABLE IF EXISTS temp._lake_prices")
    conn.execute(
        "CREATE TEMP TABLE _lake_prices ("
        "  tcgplayer_product_id INTEGER NOT NULL,"
        "  sub_type_name        TEXT NOT NULL,"
        "  price                REAL NOT NULL,"
        "  PRIMARY KEY (tcgplayer_product_id, sub_type_name)"
        ")"
    )
    owned = conn.execute(
        "SELECT DISTINCT p.tcgplayer_product_id, p.sub_type_name "
        "  FROM collection c "
        "  JOIN shared.printings p ON c.printing_id = p.printing_id "
        " WHERE c.status = 'owned' AND p.tcgplayer_product_id IS NOT NULL"
    ).fetchall()
    staged = [
        (product_id, sub_type, prices[(product_id, sub_type)])
        for product_id, sub_type in owned
        if (product_id, sub_type) in prices
    ]
    conn.executemany("INSERT INTO _lake_prices VALUES (?, ?, ?)", staged)
    return len(staged)


def snapshot(
    tenant: Tenant,
    shared: Path,
    prices: dict[tuple[int, str], float],
    date: dt.date,
    *,
    identifier: str,
    lake_ref: str,
    dry_run: bool = False,
) -> int:
    """Compute and write one tenant's snapshot for ``date``. Returns rows written.

    Raises on anything that makes this tenant unsnapshottable — a missing file,
    a database another process holds, a schema that predates the provenance
    table. The caller logs it and moves to the next tenant.
    """
    if not tenant.path.exists():
        raise TransformError(f"no database at {tenant.path}")

    # Not `mode=ro` for the catalog either: these are WAL databases, and SQLite
    # cannot open one read-only when it has to manage the -shm. The ATTACH is
    # read-write at the file level and read-only by construction — nothing
    # below writes to a `shared.` table.
    conn = sqlite3.connect(tenant.path, isolation_level=None, timeout=5.0)
    try:
        conn.execute("PRAGMA busy_timeout = 5000")
        conn.execute("ATTACH DATABASE ? AS shared", (str(shared),))
        require_provenance_table(conn)

        conn.execute("BEGIN IMMEDIATE")
        staged = stage_prices(conn, prices)
        conn.execute("DROP TABLE IF EXISTS temp._snap_owned")
        # `execute`, not `executescript`: OWNED_SQL is one statement, and
        # executescript commits whatever transaction is open before it runs.
        conn.execute(OWNED_SQL)
        copies = conn.execute("SELECT COUNT(*) FROM _snap_owned").fetchone()[0]

        conn.execute("DELETE FROM collection_value_snapshot WHERE date = ?", (date.isoformat(),))
        written = 0
        for sql in DIMENSION_SQL:
            written += conn.execute(sql, (date.isoformat(),)).rowcount
        conn.execute(
            "INSERT OR REPLACE INTO collection_value_snapshot_run "
            "    (date, artefact, lake_ref, rows, written_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (
                date.isoformat(),
                identifier,
                lake_ref,
                written,
                dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            ),
        )

        if dry_run:
            conn.execute("ROLLBACK")
            print(f"    dry-run: {written} row(s) from {copies} owned cop(ies), not written")
            return written
        conn.execute("COMMIT")
        print(f"    {written} row(s) from {copies} owned cop(ies), {staged} price(s) staged")
        return written
    finally:
        conn.close()


def require_provenance_table(conn: sqlite3.Connection) -> None:
    """Refuse a database that has no ``collection_value_snapshot_run``.

    The schema is owned by ``crates/pkdump-db/src/schema_user.sql`` and applied
    on every open by the app, so this job creating the table itself would make
    two files the authority on one table's shape. It refuses instead, naming
    the thing that fixes it.
    """
    present = conn.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        ("collection_value_snapshot_run",),
    ).fetchone()[0]
    if not present:
        raise TransformError(
            "this database has no collection_value_snapshot_run table — it was last "
            "opened by a build that predates pd-ruwh. Any pkdump command that opens it "
            "(e.g. `pkdump tenant list`) re-applies the schema."
        )


def run(
    date: dt.date,
    *,
    root: Path,
    identifier: str = DEFAULT_TABLE,
    ref: str | None = None,
    only: list[str] | None = None,
    dry_run: bool = False,
) -> list[Outcome]:
    """Snapshot every registered tenant for ``date``. One tenant's failure is not the run's."""
    shared = root / "shared.sqlite"
    if not shared.exists():
        raise TransformError(f"no catalog at {shared} — run `pkdump setup` first")

    registered = tenants(root, only)
    if not registered:
        raise TransformError(
            f"the registry at {root / REGISTRY_FILE} holds no active user — "
            "`pkdump tenant create <handle>` provisions one"
        )

    lake_ref = pin(ref)
    print(f"==> lake {identifier} at {lake_ref}")
    print(f"==> data directory {root}, {len(registered)} active tenant(s)")
    prices = market_prices(identifier, date, lake_ref)
    if not prices:
        raise TransformError(
            f"{identifier} holds no {PRICE_TYPE} price at or before {date} at {lake_ref} — "
            "there is nothing to value a collection with"
        )

    outcomes = []
    for tenant in registered:
        print(f"==> {tenant.handle} ({tenant.database_id})")
        try:
            rows = snapshot(
                tenant,
                shared,
                prices,
                date,
                identifier=identifier,
                lake_ref=lake_ref,
                dry_run=dry_run,
            )
            outcomes.append(Outcome(tenant, rows=rows))
        except (TransformError, sqlite3.Error) as exc:
            # Logged and skipped, deliberately: the next tenant's snapshot does
            # not depend on this one, and a run that stops at the first bad
            # database is a run that snapshots whoever sorts first.
            print(f"    SKIPPED: {exc}")
            outcomes.append(Outcome(tenant, error=str(exc)))
    return outcomes


def report(outcomes: list[Outcome], date: dt.date) -> int:
    """Print the per-tenant tally and return the process exit status."""
    done = [o for o in outcomes if o.ok]
    skipped = [o for o in outcomes if not o.ok]
    print("")
    print(f"==> {date}: {len(done)} tenant(s) snapshotted, {len(skipped)} skipped")
    for outcome in skipped:
        print(f"    skipped {outcome.tenant.handle}: {outcome.error}")
    if skipped:
        print(f"==> exiting {EXIT_PARTIAL} (partial): a run that half-completes says so")
        return EXIT_PARTIAL
    return EXIT_OK


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="pkdump-lake-value-snapshots",
        description="Per-tenant collection value snapshots from catalog.prices. Every tenant.",
    )
    parser.add_argument(
        "--date",
        required=True,
        help="the snapshot date, YYYY-MM-DD. Required, and never defaulted from the clock: "
        "backfilling an older date is the same operation as taking today's.",
    )
    parser.add_argument(
        "--data-dir",
        help=f"the pkdump data directory (default: ${HOME_ENV}, then {DEFAULT_HOME})",
    )
    parser.add_argument("--table", default=DEFAULT_TABLE, help=f"default {DEFAULT_TABLE}")
    parser.add_argument(
        "--ref",
        help="the Nessie ref to read (default: $PKDUMP_LAKE_REF, then main). A bare branch is "
        "pinned to its current commit for the whole run; pass 'main@<hash>' to pin it yourself.",
    )
    parser.add_argument(
        "--tenant",
        action="append",
        metavar="HANDLE",
        help="snapshot only this tenant (repeatable). The default is every active tenant, "
        "which is the point of the job — use this for a one-off repair.",
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="compute everything, write nothing"
    )
    args = parser.parse_args(argv)

    try:
        outcomes = run(
            dt.date.fromisoformat(args.date),
            root=data_dir(args.data_dir),
            identifier=args.table,
            ref=args.ref,
            only=args.tenant,
            dry_run=args.dry_run,
        )
    except TransformError as exc:
        print(f"!! {exc}", file=sys.stderr)
        return EXIT_FAILED
    return report(outcomes, dt.date.fromisoformat(args.date))


if __name__ == "__main__":
    sys.exit(main())
