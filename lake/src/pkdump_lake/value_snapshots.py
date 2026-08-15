"""Per-tenant collection value snapshots, computed from the lake.

pd-ruwh. This is the transform tier's first job: it reads ``catalog.prices``
at a **pinned Nessie commit**, joins it against each tenant's own collection,
and writes ``collection_value_snapshot`` back into **that tenant's** database.

## The bug it exists to fix

``pkdump data refresh`` used to end with a step 7 calling
``value_history::snapshot_today`` on the **one** collection ``$PKDUMP_USER``
resolves to. There was no loop. Every other registered tenant got no value
history, ever, and nothing errored — the run looked perfectly successful
(pd-s5yn). Latent only because prod has one tenant. So the unit of work here is
*the registry*, not *the current user*: the loop is the feature.

That step is now deleted rather than fixed in place (pd-hkbc), which makes this
job the **only** thing that records today's value for anybody. The refresh no
longer opens a tenant database at all — ``tests/refresh/tenant_bytes.sh`` holds
it to that — so a day this job does not run is a gap in every tenant's chart.
It is recoverable (``--date`` reconstructs any day still in the lake) but it is
not self-healing, which is the argument for putting this on a timer rather than
running it by hand.

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

## Phase 3: where the holdings come from (pd-szh2)

``--holdings`` chooses the holdings source, and it is the ONLY thing that
differs between the two paths:

* ``collection`` (the default, unchanged) — the tenant's live table. The
  design calls this the *online* read: the write moved offline, the read
  stayed online, and that is the half of the loop the inbound-leg epic exists
  to close.
* ``zone`` — ``zone_holdings``, materialised out of the **tenant zone** by
  ``pkdump-ship holdings``. This is Phase 3 of the cycle: land raw, build the
  catalog, ingest tenant state, **compute valuations**, publish back.

Everything else — the price rule, the three dimensions, the CAST and the
COALESCE — is one body of SQL parameterised by that one table name, which the
source resolves to through :data:`HOLDINGS_TABLES` rather than being named by
a caller. That is
deliberate and it is what makes ``--compare`` mean anything: a difference
between the two runs cannot be a difference in arithmetic, because there is
only one arithmetic. It can only be a difference in *holdings*, which is the
question being asked.

``--compare`` runs both for every tenant, diffs the rows, and writes nothing.
Exit :data:`EXIT_MISMATCH` if any tenant's two computations disagree. Until
that is clean, the online path stays (`pd-i08u` is the change that removes
it).

**What the zone does not carry.** Only ``collection`` rows are shipped
(``pkdump_db::outbox::SOURCE_TABLES``). The condition multiplier
(``conditions``), and the third arm of the price rule (``manual_prices`` over
``user_printings``), are still read from the tenant's own database on both
paths. They are not holdings and the outbox does not emit them, so Phase 3
narrows *which table the copies come from* and nothing else.

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
from dataclasses import dataclass, field
from pathlib import Path

from pyiceberg.exceptions import NoSuchTableError
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
#: The run could not start at all — no registry, no lake, bad arguments — or
#: it started and snapshotted **nobody**. The second is the same fact as the
#: first arriving one tenant at a time, and :data:`EXIT_PARTIAL` would report
#: it as a warning about a night when nothing happened.
EXIT_FAILED = 1
#: The run completed, and at least one tenant was skipped. Deliberately its own
#: code: "some tenants have no value history today" must not read as success,
#: and must not read as "the job did not run" either.
EXIT_PARTIAL = 2
#: ``--compare`` only: every tenant was compared and at least one pair of
#: computations DISAGREED. Its own code, above EXIT_PARTIAL, because it is not
#: a run that half-happened — it is a run that happened and found the two
#: valuation paths saying different things about somebody's money. Nothing
#: downstream should treat that as a skip.
EXIT_MISMATCH = 4

#: The tenant's live holdings — the ONLINE read, and still the default.
SOURCE_ONLINE = "collection"
#: The tenant zone, materialised by ``pkdump-ship holdings``. Phase 3.
SOURCE_ZONE = "zone"
#: What ``--holdings`` accepts. These are the names of the two *paths*, not
#: the names of two tables: "zone" is where the holdings came from, and
#: ``zone_holdings`` is only where they are parked on the way through.
HOLDINGS_SOURCES = (SOURCE_ONLINE, SOURCE_ZONE)

#: The staging table Phase 3 reads. Spelled the same in
#: ``pkdump_db::outbox::ZONE_HOLDINGS_TABLE`` and in ``pkdump_ship::zone`` —
#: three places, because this job is not a Rust program, and
#: ``tests/lake/phase3.sh`` is what holds them together.
ZONE_HOLDINGS = "zone_holdings"
#: The provenance of :data:`ZONE_HOLDINGS`, written by the same command.
ZONE_HOLDINGS_RUN = "zone_holdings_run"

#: Which table each source is read from. The indirection is the point: the
#: online path reads the collection itself, the zone path reads a copy of the
#: zone, and the *source* is the thing a caller chooses.
HOLDINGS_TABLES = {SOURCE_ONLINE: "collection", SOURCE_ZONE: ZONE_HOLDINGS}

#: The per-copy projection, one row per owned copy, carrying its set, binder,
#: purchase price, condition multiplier and current market price.
#:
#: A transliteration of ``OWNED_TODAY_SQL`` in
#: ``crates/pkdump-db/src/value_history.rs``, with two substitutions and no
#: other change:
#:
#: * ``latest_prices`` becomes ``_lake_prices`` — the same shape, staged from
#:   Iceberg below, which is the entire point of the rewrite. The other two
#:   arms of the price rule are transliterated as they stand in
#:   ``pkdump_db::prices::MARKET_PRICE_EXPR``: the curated
#:   ``catalog_price_overrides`` patch, and the tenant's ``manual_prices``
#:   guarded to printings that tenant invented (pd-m4gw). Drop either and this
#:   job values a collection by a different rule than every page that renders
#:   it.
#: * the catalog tables are ``shared.``-qualified. The Rust path exposes them
#:   through TEMP VIEWs over the same ATTACH; this job just names them.
#:
#: ``conditions`` is deliberately NOT among them: it lives in the tenant's own
#: database (pd-s4c2), beside the ``collection`` rows its ``name`` is joined
#: to. Qualifying it ``shared.`` would read another tenant's multipliers, or —
#: since the catalog no longer has the table at all — fail outright.
#:
#: The two implementations must agree to the byte, and are held to it: the
#: container gate runs Rust ``snapshot_today`` and this job over one fixture
#: and compares the rows.
#:
#: ``{holdings}`` is the one substitution, and the only difference between the
#: online path and Phase 3 (pd-szh2). It is a table NAME from
#: :data:`HOLDINGS_SOURCES`, checked against that tuple before it is formatted
#: in — never a value that reached this module from anywhere a user can write.
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
         (SELECT o.price FROM shared.catalog_price_overrides o
            WHERE o.printing_id = p.printing_id),
         (SELECT mp.price FROM manual_prices mp
            WHERE mp.printing_id = p.printing_id
              AND EXISTS (SELECT 1 FROM user_printings up
                           WHERE up.printing_id = mp.printing_id)
            ORDER BY mp.observed_at DESC LIMIT 1)
       ) AS market_price
  FROM {holdings} c
  JOIN (
         SELECT printing_id, card_id, tcgplayer_product_id, sub_type_name
           FROM shared.printings
         UNION ALL
         SELECT printing_id, card_id, NULL, NULL
           FROM user_printings
       ) p ON c.printing_id = p.printing_id
  JOIN shared.cards cd ON p.card_id = cd.card_id
  LEFT JOIN conditions cond ON cond.name = c.condition
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


@dataclass
class Comparison:
    """One tenant's two valuations, and whether they are the same rows."""

    tenant: Tenant
    online: list[tuple] = field(default_factory=list)
    offline: list[tuple] = field(default_factory=list)
    error: str | None = None

    @property
    def ok(self) -> bool:
        """Was this tenant compared at all? Not "did they agree"."""
        return self.error is None

    @property
    def agrees(self) -> bool:
        return self.ok and not self.differences()

    def differences(self) -> list[str]:
        """Every row that differs, described. Empty means row-for-row equal.

        Keyed by (dimension, bucket), so a bucket present on one side and
        absent on the other is reported as what it is — a missing row — rather
        than as every subsequent row being misaligned.

        The values are compared **exactly**. Both sides are SQLite doubles
        summed by the same expression over the same price map, so equal inputs
        give bit-identical outputs; a tolerance here would be a place for a
        real difference in holdings to hide.
        """
        if not self.ok:
            return []
        online = {(row[0], row[1]): row[2:] for row in self.online}
        offline = {(row[0], row[1]): row[2:] for row in self.offline}
        out = []
        for key in sorted(set(online) | set(offline), key=lambda k: (k[0], k[1] or "")):
            bucket = f"{key[0]}/{key[1]}" if key[1] is not None else key[0]
            if key not in offline:
                out.append(f"{bucket}: online has it, the zone does not — {online[key]}")
            elif key not in online:
                out.append(f"{bucket}: the zone has it, online does not — {offline[key]}")
            elif online[key] != offline[key]:
                out.append(f"{bucket}: online {online[key]} != zone {offline[key]}")
        return out


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
    try:
        table = catalog(lake_ref).load_table(identifier)
    except NoSuchTableError as exc:
        raise TransformError(
            f"no {identifier} at {lake_ref} — build it first with "
            "`pkdump-lake-build-prices --ingest-date <date>` (deploy/LAKE.md §6)"
        ) from exc
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


def stage_prices(
    conn: sqlite3.Connection,
    prices: dict[tuple[int, str], float],
    source: str = SOURCE_ONLINE,
) -> int:
    """Stage the prices this tenant can actually use into ``_lake_prices``.

    Restricted to the products the tenant owns a copy of — a collection touches
    a few thousand products out of the catalog's hundreds of thousands, and the
    join in :data:`OWNED_SQL` cannot tell the difference.

    ``source`` is the same holdings source :data:`OWNED_SQL` reads. Staging
    from one table and valuing from another would price the products in one
    collection and count the copies in another, so the two resolve together.
    """
    holdings = holdings_table(source)
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
        f"  FROM {holdings} c "
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


def holdings_table(source: str) -> str:
    """The table `source` is read from, refusing anything not a known source.

    The name is formatted into SQL — a table name can never be a bound
    parameter — so it is resolved through :data:`HOLDINGS_TABLES` at every
    point it enters rather than passed through. A caller cannot name a table;
    it can only name a path, and the mapping decides the rest.
    """
    if source not in HOLDINGS_TABLES:
        raise TransformError(
            f"{source!r} is not a holdings source; expected one of {', '.join(HOLDINGS_SOURCES)}"
        )
    return HOLDINGS_TABLES[source]


def require_zone_holdings(conn: sqlite3.Connection, tenant: Tenant) -> str:
    """Refuse a tenant whose staged zone read is missing or behind the zone.

    Two refusals, and the second is the one worth having. An **absent**
    staging table is obvious. A **stale** one is not: it values today's
    collection at the holdings of whenever somebody last ran the reader, and
    every number it produces looks entirely reasonable. The shipper's own
    cursor is what makes it detectable — ``shipped_thru`` is the highest seq
    in the zone, ``max_seq`` is the highest seq that was read out of it, and
    the second being behind the first means the read predates the last ship.

    A tenant whose OUTBOX is ahead of the zone is *not* refused: that is the
    zone lagging the collection, which is a real difference between the two
    paths and exactly what ``--compare`` exists to show rather than hide.

    Returns the ``read_at`` of the materialisation, for the log line.
    """
    row = conn.execute(
        "SELECT max_seq, read_at, parts, rows FROM " + ZONE_HOLDINGS_RUN + " WHERE dataset = ?",
        ("holdings",),
    ).fetchone()
    if row is None:
        raise TransformError(
            f"{tenant.handle} has no staged zone holdings — run `pkdump-ship holdings "
            f"--tenant {tenant.database_id}` first (deploy/TENANT_ZONE.md). Valuing from a "
            "missing table would be valuing from nothing; valuing from a stale one would be "
            "worse, so neither is guessed at."
        )
    max_seq, read_at, parts, rows = row

    cursor = conn.execute(
        "SELECT shipped_thru FROM ownership_outbox_cursor WHERE id = 1"
    ).fetchone()
    shipped_thru = cursor[0] if cursor else 0
    if shipped_thru > max_seq:
        raise TransformError(
            f"{tenant.handle}'s staged zone holdings stop at seq {max_seq} but the shipper has "
            f"put {shipped_thru} into the zone — the materialisation ({read_at}) predates the "
            "last ship, so these numbers would be yesterday's holdings at today's prices. "
            f"Re-run `pkdump-ship holdings --tenant {tenant.database_id}`."
        )
    print(f"    zone: {rows} holding(s) from {parts} part(s) through seq {max_seq}, read {read_at}")
    return read_at


def snapshot(
    tenant: Tenant,
    shared: Path,
    prices: dict[tuple[int, str], float],
    date: dt.date,
    *,
    identifier: str,
    lake_ref: str,
    holdings: str = SOURCE_ONLINE,
    dry_run: bool = False,
) -> tuple[int, list[tuple]]:
    """Compute one tenant's snapshot for ``date``. Returns (rows, the rows).

    Writes them unless ``dry_run``. The rows come back either way, because
    ``--compare`` needs two computations' output and must persist neither.

    Raises on anything that makes this tenant unsnapshottable — a missing file,
    a database another process holds, a schema that predates the provenance
    table, a zone read that never happened. The caller logs it and moves to the
    next tenant.
    """
    table = holdings_table(holdings)
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
        if holdings == SOURCE_ZONE:
            require_zone_holdings(conn, tenant)

        conn.execute("BEGIN IMMEDIATE")
        staged = stage_prices(conn, prices, holdings)  # resolves to `table`
        conn.execute("DROP TABLE IF EXISTS temp._snap_owned")
        # `execute`, not `executescript`: OWNED_SQL is one statement, and
        # executescript commits whatever transaction is open before it runs.
        conn.execute(OWNED_SQL.format(holdings=table))
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
        # Read back INSIDE the transaction, so what is compared is what was
        # computed rather than what survived a concurrent writer.
        computed = conn.execute(
            "SELECT dimension, bucket, market_value, cost_basis, card_count "
            "  FROM collection_value_snapshot WHERE date = ? ORDER BY dimension, bucket",
            (date.isoformat(),),
        ).fetchall()

        if dry_run:
            conn.execute("ROLLBACK")
            print(
                f"    dry-run ({holdings}): {written} row(s) from {copies} owned cop(ies) "
                f"in {table}, not written"
            )
            return written, computed
        conn.execute("COMMIT")
        print(
            f"    {written} row(s) from {copies} owned cop(ies) in {table}, "
            f"{staged} price(s) staged"
        )
        return written, computed
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
    holdings: str = SOURCE_ONLINE,
    dry_run: bool = False,
) -> list[Outcome]:
    """Snapshot every registered tenant for ``date``. One tenant's failure is not the run's."""
    registered, shared, prices, lake_ref = _prepare(date, root, identifier, ref, only)
    holdings_table(holdings)  # refuse an unknown source before any tenant

    outcomes = []
    for tenant in registered:
        print(f"==> {tenant.handle} ({tenant.database_id})")
        try:
            rows, _ = snapshot(
                tenant,
                shared,
                prices,
                date,
                identifier=identifier,
                lake_ref=lake_ref,
                holdings=holdings,
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


def compare(
    date: dt.date,
    *,
    root: Path,
    identifier: str = DEFAULT_TABLE,
    ref: str | None = None,
    only: list[str] | None = None,
) -> list[Comparison]:
    """Value every tenant BOTH ways for ``date`` and diff the rows.

    The acceptance bar for pd-szh2, executable. Both computations run against
    the same pinned catalog commit, the same prices, the same SQL and the same
    database — only :data:`OWNED_SQL`'s holdings table differs — so a
    difference is a difference in holdings and cannot be anything else.

    **Writes nothing.** Both runs are rolled back, including the online one
    that would otherwise be the real snapshot: a comparison that also
    published would make "did they agree" and "what is recorded" the same
    question, and the answer to the second would depend on which ran last.
    """
    registered, shared, prices, lake_ref = _prepare(date, root, identifier, ref, only)

    comparisons = []
    for tenant in registered:
        print(f"==> {tenant.handle} ({tenant.database_id})")
        try:
            both = {}
            for source in HOLDINGS_SOURCES:
                _, both[source] = snapshot(
                    tenant,
                    shared,
                    prices,
                    date,
                    identifier=identifier,
                    lake_ref=lake_ref,
                    holdings=source,
                    dry_run=True,
                )
            comparison = Comparison(
                tenant,
                online=both[SOURCE_ONLINE],
                offline=both[SOURCE_ZONE],
            )
            for line in comparison.differences():
                print(f"    !! {line}")
            if comparison.agrees:
                print(f"    ok   {len(comparison.online)} row(s) identical")
            comparisons.append(comparison)
        except (TransformError, sqlite3.Error) as exc:
            print(f"    SKIPPED: {exc}")
            comparisons.append(Comparison(tenant, error=str(exc)))
    return comparisons


def _prepare(
    date: dt.date,
    root: Path,
    identifier: str,
    ref: str | None,
    only: list[str] | None,
) -> tuple[list[Tenant], Path, dict[tuple[int, str], float], str]:
    """Everything both entry points need before they touch a tenant.

    One function so ``--compare`` reads the same catalog, at the same pinned
    commit, as the run it is a comparison of.
    """
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
    return registered, shared, prices, lake_ref


def report(outcomes: list[Outcome], date: dt.date) -> int:
    """Print the per-tenant tally and return the process exit status."""
    done = [o for o in outcomes if o.ok]
    skipped = [o for o in outcomes if not o.ok]
    print("")
    print(f"==> {date}: {len(done)} tenant(s) snapshotted, {len(skipped)} skipped")
    for outcome in skipped:
        print(f"    skipped {outcome.tenant.handle}: {outcome.error}")
    if not done and skipped:
        # "Some tenants" and "nobody" are different nights. A run that
        # snapshotted no one achieved nothing however politely each tenant
        # declined, and a warning is the wrong volume for it — this is the
        # shape of a missing catalog or a data directory nobody has read the
        # zone into, not of one database mid-import. Same rule as
        # `pkdump_ship::run`'s Outcome::Failed, and for the same reason.
        print(f"==> exiting {EXIT_FAILED}: nobody was snapshotted at all")
        return EXIT_FAILED
    if skipped:
        print(f"==> exiting {EXIT_PARTIAL} (partial): a run that half-completes says so")
        return EXIT_PARTIAL
    return EXIT_OK


def report_comparison(comparisons: list[Comparison], date: dt.date) -> int:
    """Print the equivalence result in numbers, and return the exit status.

    The numbers are the deliverable — "tenants compared, rows compared, diffs
    found" — because "it matched" is not a result anybody can check later.
    """
    compared = [c for c in comparisons if c.ok]
    skipped = [c for c in comparisons if not c.ok]
    rows = sum(len(c.online) for c in compared)
    disagreed = [c for c in compared if not c.agrees]

    print("")
    print(
        f"==> {date}: {len(compared)} tenant(s) compared, {rows} row(s), "
        f"{len(disagreed)} tenant(s) DIFFERING, {len(skipped)} skipped"
    )
    for comparison in skipped:
        print(f"    skipped {comparison.tenant.handle}: {comparison.error}")
    for comparison in disagreed:
        print(f"    {comparison.tenant.handle} ({comparison.tenant.database_id}):")
        for line in comparison.differences():
            print(f"      {line}")

    if disagreed:
        print(
            f"==> exiting {EXIT_MISMATCH}: the online and Phase 3 valuations DISAGREE. "
            "Do not delete the online path (pd-i08u) — find out why first."
        )
        return EXIT_MISMATCH
    if skipped:
        print(
            f"==> exiting {EXIT_PARTIAL} (partial): every tenant compared agreed, but "
            "some were not compared at all, which is not the same as agreement"
        )
        return EXIT_PARTIAL
    if not compared:
        print(f"==> exiting {EXIT_FAILED}: nobody was compared")
        return EXIT_FAILED
    print("==> the two valuations are identical, row for row")
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
    parser.add_argument(
        "--holdings",
        choices=HOLDINGS_SOURCES,
        default=SOURCE_ONLINE,
        help=f"where the owned copies come from (default {SOURCE_ONLINE}, the tenant's "
        f"live table). {SOURCE_ZONE} is Phase 3: the tenant zone, read into "
        f"{ZONE_HOLDINGS} by `pkdump-ship holdings`. Everything else is identical.",
    )
    parser.add_argument(
        "--compare",
        action="store_true",
        help="value every tenant BOTH ways and diff the rows. Writes nothing; exits "
        f"{EXIT_MISMATCH} if any tenant's two valuations disagree. This is the gate "
        "the online path is not removed until it passes (pd-szh2 -> pd-i08u).",
    )
    args = parser.parse_args(argv)

    date = dt.date.fromisoformat(args.date)
    if args.compare and args.holdings != SOURCE_ONLINE:
        # Not ignored: `--compare --holdings zone` reads as "compare, but only
        # the zone", which is not a thing a comparison can be.
        print(
            f"!! --compare runs both holdings sources; --holdings {args.holdings} says to run "
            "one. Drop one of them.",
            file=sys.stderr,
        )
        return EXIT_FAILED

    try:
        if args.compare:
            comparisons = compare(
                date,
                root=data_dir(args.data_dir),
                identifier=args.table,
                ref=args.ref,
                only=args.tenant,
            )
            return report_comparison(comparisons, date)

        outcomes = run(
            date,
            root=data_dir(args.data_dir),
            identifier=args.table,
            ref=args.ref,
            only=args.tenant,
            holdings=args.holdings,
            dry_run=args.dry_run,
        )
    except TransformError as exc:
        print(f"!! {exc}", file=sys.stderr)
        return EXIT_FAILED
    return report(outcomes, date)


if __name__ == "__main__":
    sys.exit(main())
