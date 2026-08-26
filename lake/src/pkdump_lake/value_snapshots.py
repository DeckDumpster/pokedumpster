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

## Phase 3: the holdings come from the TENANT ZONE, and from nowhere else

The copies this job values are read from :data:`ZONE_HOLDINGS`, materialised
out of the **tenant zone** by ``pkdump-ship holdings``. That is Phase 3 of the
cycle — land raw, build the catalog, ingest tenant state, **compute
valuations**, publish back — and it is the half of the loop the inbound-leg
epic exists to close: the write had moved offline and the read had stayed
online.

pd-szh2 shipped that read **alongside** the online one, behind ``--holdings``,
with a ``--compare`` that valued every tenant both ways and diffed the rows.
It came back clean, and pd-i08u is the change that acts on it: the online read
is **gone**, there is no flag to bring it back, and there is no fallback if
the zone has not been read. One valuation path, so a number on a chart has
exactly one provenance.

Two things follow, and they are the whole of the operational contract:

* **The zone must be read back before this job runs.** ``pkdump-ship run``
  puts the outbox in the zone; ``pkdump-ship holdings`` brings it back into
  :data:`ZONE_HOLDINGS`. ``deploy/ship.sh`` does both, and
  ``pkdump-value-snapshots@`` is ordered ``After=pkdump-ship@``. A tenant
  whose zone was never read is **skipped, naming the command** — see
  :func:`require_zone_holdings`.
* **A stale read is refused too**, and that is the refusal worth having. See
  :func:`require_zone_holdings` for why a plausible number is worse than none.

**What the zone does not carry.** The condition multiplier (``conditions``),
and the third arm of the price rule (``manual_prices`` over
``user_printings``), are read from the tenant's own database — they were on
the online path and they still are. Deleting the online *holdings* read did
not make the tenant database unnecessary and must not be read as though it
had: Phase 3 narrowed *which table the copies come from* and nothing else.

## Two halves, two dimensions: `all` is the loose cards (pd-bbv7)

The zone ships both holdings tables (``pkdump_db::outbox::SOURCE_TABLES``) and
the read-back now stages both — ``collection`` into :data:`ZONE_HOLDINGS`,
``sealed_collection`` into :data:`ZONE_SEALED_HOLDINGS`. This job values each
against its own feed and writes them as **separate dimensions**:

===============  ====================  ===========================  ============
dimension        holdings              prices                       card_count
===============  ====================  ===========================  ============
``all``          :data:`ZONE_HOLDINGS` ``catalog.prices``           owned copies
``sealed``       sealed staging table  ``catalog.sealed_prices``    owned UNITS
===============  ====================  ===========================  ============

``all`` keeps meaning the loose cards and nothing else. Every row ever written
under it was computed over cards alone, and widening it would restate months of
chart silently. There is no combined total written here either — the API adds
the two series at read time, because a stored total is a third number that can
disagree with the two it is made of.

The sealed row's arithmetic is `value_history.rs`'s, transliterated the same
way the card one is: ``SUM(price × quantity)``, ``SUM(purchase_price ×
quantity)`` (a lot's price is per unit), ``SUM(quantity)`` for the count, and
**no condition multiplier** — nothing in the app prices a box off its
condition. A lot whose product nobody quotes is skipped by the ``SUM`` and
still counted by ``SUM(quantity)``: valuing it at zero would be
indistinguishable from a box that is worthless.

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

Run it against a data directory the app is not currently writing, after the
zone has been shipped and read back::

    pkdump-ship run --data-dir /data && pkdump-ship holdings --data-dir /data
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

from pyiceberg.exceptions import NoSuchTableError
from pyiceberg.expressions import And, EqualTo, LessThanOrEqual

from .catalog import DEFAULT_REF, REF_ENV, catalog, head_hash

#: The lake table this job reads. Its grain is (product, sub_type, price_type,
#: observed_date) — see :mod:`pkdump_lake.prices`.
DEFAULT_TABLE = "catalog.prices"

#: The second table it reads (pd-bbv7): the sealed series, at the grain
#: (product, price_type, observed_date). Written by the same run of the same
#: job that writes :data:`DEFAULT_TABLE`, so the two cannot be different
#: nights' prices.
DEFAULT_SEALED_TABLE = "catalog.sealed_prices"

#: The one price type a collection is valued at, matching the collection page
#: and ``value_history.rs``.
PRICE_TYPE = "market"

#: …and the one a SEALED lot falls back to. TCGCSV quotes plenty of sealed
#: products a mid and no market, so `latest_sealed_prices` is read through
#: ``COALESCE(market_price, mid_price)`` everywhere in the app — the /sealed
#: page, and `prices.rs::sealed_market_price_expr_from!`. This job resolves the
#: same pair, off the same single observation.
SEALED_FALLBACK_PRICE_TYPE = "mid"

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

#: The staging table this job reads — the tenant zone, brought back by
#: ``pkdump-ship holdings``. Spelled the same in
#: ``pkdump_db::outbox::ZONE_HOLDINGS_TABLE`` and in ``pkdump_ship::zone`` —
#: three places, because this job is not a Rust program, and
#: ``tests/lake/phase3.sh`` is what holds them together.
#:
#: It is a constant rather than an argument (pd-i08u). While the online read
#: existed this was one of two table names a caller chose between; with that
#: read deleted there is nothing to choose, and a parameter that can only take
#: one value is a seam for a second value to come back through.
ZONE_HOLDINGS = "zone_holdings"
#: The sealed half of the same read-back (pd-bbv7), spelled in the same three
#: places — ``pkdump_db::outbox::ZONE_SEALED_HOLDINGS_TABLE``,
#: ``pkdump_ship::zone::SEALED_HOLDINGS_TABLE``, and here.
ZONE_SEALED_HOLDINGS = "zone_sealed_holdings"
#: The provenance of both, written by the same command.
ZONE_HOLDINGS_RUN = "zone_holdings_run"

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
#: The holdings table is :data:`ZONE_HOLDINGS`, named once. pd-szh2 made it a
#: ``{holdings}`` substitution so the same arithmetic could be run over the
#: online table and the zone and the results diffed; that comparison came back
#: clean and pd-i08u deleted the online side of it, so there is one table here
#: and no substitution to make.
OWNED_SQL = f"""
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
  FROM {ZONE_HOLDINGS} c
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

#: The per-lot sealed projection, one row per owned sealed lot, carrying its
#: quantity, per-unit purchase price and current market price.
#:
#: A transliteration of ``OWNED_SEALED_TODAY_SQL`` in
#: ``crates/pkdump-db/src/value_history.rs``, with the same one substitution
#: the card projection makes: ``latest_sealed_prices`` becomes
#: ``_lake_sealed_prices``, staged from Iceberg below. The ``COALESCE(market,
#: mid)`` the Rust expression spells is resolved during that staging — see
#: :func:`sealed_market_prices` — so one price reaches the SQL, off one
#: observation.
#:
#: It joins **nothing**, and that is the trap this dimension exists beside:
#: sealed product ids are not in ``shared.tcgcsv_products`` (which holds
#: single-card products), so joining a sealed holding through it drops every
#: row and reports a collection with no sealed product in it. The dimension is
#: one bucket, so it needs no catalog attribute at all; where one is ever
#: wanted it comes from ``shared.sealed_products``, keyed by ``product_id``.
OWNED_SEALED_SQL = f"""
CREATE TEMP TABLE _snap_sealed AS
SELECT sc.id,
       sc.quantity,
       sc.purchase_price,
       (SELECT lsp.price FROM _lake_sealed_prices lsp
          WHERE lsp.tcgplayer_product_id = sc.product_id LIMIT 1) AS market_price
  FROM {ZONE_SEALED_HOLDINGS} sc
 WHERE sc.status = 'owned';
"""

#: The four aggregates, in the order ``value_history::insert_dimensions``
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
    # Sealed. `HAVING COUNT(*) > 0` so a tenant who owns no sealed product
    # gets no sealed row at all — the bucket exists when something is in it,
    # exactly as `set` and `binder` do — and a collection that has never held
    # any is left with precisely the rows it has always had.
    """
    INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
    SELECT ?, 'sealed', NULL,
           COALESCE(SUM(market_price * quantity), 0.0),
           COALESCE(SUM(purchase_price * quantity), 0.0),
           COALESCE(SUM(quantity), 0)
      FROM _snap_sealed
     HAVING COUNT(*) > 0
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


def sealed_market_prices(identifier: str, date: dt.date, lake_ref: str) -> dict[int, float]:
    """The newest sealed price per product at or before ``date``, resolved.

    Resolved, because ``COALESCE(market_price, mid_price)`` is a choice between
    two quotes of ONE observation — the app reads it off a single
    ``latest_sealed_prices`` row, and taking the market from one day and the
    mid from another would invent a price nobody published. So this keeps, per
    product, the newest ``observed_date`` seen and the price types quoted on
    it, and picks between them at the end.

    A product quoted neither market nor mid on its newest day is **absent**
    from the result, not zero: the aggregate skips it and still counts its
    units. Falling back to an older day's quote instead would be a different
    rule from the one every page in the app uses.

    Read in batches for the reason :func:`market_prices` is: the scan grows
    with the lake's age, the result with the product count.
    """
    try:
        table = catalog(lake_ref).load_table(identifier)
    except NoSuchTableError as exc:
        raise TransformError(
            f"no {identifier} at {lake_ref} — it is built by the same run that builds "
            "catalog.prices, so a lake that has one and not the other was built by a "
            "`pkdump-lake-build-prices` that predates pd-bbv7. Re-run it for this date."
        ) from exc
    # Both price types are wanted, so the filter is the date alone and the
    # pair is separated below. `price_type` is not a partition column and the
    # rows are two per product; filtering it here would buy nothing.
    scan = table.scan(
        row_filter=LessThanOrEqual("observed_date", date.isoformat()),
        selected_fields=("tcgplayer_product_id", "price_type", "price", "observed_date"),
    )

    newest: dict[int, tuple[dt.date, dict[str, float]]] = {}
    batches = 0
    for batch in scan.to_arrow_batch_reader():
        batches += 1
        columns = batch.to_pydict()
        for product_id, price_type, price, observed in zip(
            columns["tcgplayer_product_id"],
            columns["price_type"],
            columns["price"],
            columns["observed_date"],
        ):
            # The date is taken from EVERY row, including the price types this
            # rule does not spend. `latest_sealed_prices` picks the product's
            # newest observation and then coalesces within it, so a day that
            # quoted only a low is still that product's newest day — and the
            # answer is "no price", not an older day's market. Skipping such
            # rows here would quietly resolve to a stale quote the /sealed page
            # does not show.
            seen = newest.get(product_id)
            if seen is None or observed > seen[0]:
                seen = (observed, {})
                newest[product_id] = seen
            if observed == seen[0] and price_type in (
                PRICE_TYPE,
                SEALED_FALLBACK_PRICE_TYPE,
            ):
                seen[1][price_type] = price

    resolved = {
        product_id: quotes[PRICE_TYPE]
        if PRICE_TYPE in quotes
        else quotes[SEALED_FALLBACK_PRICE_TYPE]
        for product_id, (_, quotes) in newest.items()
        if PRICE_TYPE in quotes or SEALED_FALLBACK_PRICE_TYPE in quotes
    }
    print(f"    {len(resolved)} sealed product price(s) from {batches} batch(es)")
    return resolved


def stage_prices(
    conn: sqlite3.Connection,
    prices: dict[tuple[int, str], float],
) -> int:
    """Stage the prices this tenant can actually use into ``_lake_prices``.

    Restricted to the products the tenant owns a copy of — a collection touches
    a few thousand products out of the catalog's hundreds of thousands, and the
    join in :data:`OWNED_SQL` cannot tell the difference.

    "Owns" is read from :data:`ZONE_HOLDINGS`, the same table
    :data:`OWNED_SQL` values. Staging from one table and valuing from another
    would price the products in one collection and count the copies in
    another.
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
        f"  FROM {ZONE_HOLDINGS} c "
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


def stage_sealed_prices(conn: sqlite3.Connection, prices: dict[int, float]) -> int:
    """The same, for the sealed products this tenant actually holds.

    Restricted to :data:`ZONE_SEALED_HOLDINGS` for the reason the card side is
    restricted to :data:`ZONE_HOLDINGS`: staging from one table and valuing
    from another prices the lots in one collection and counts the units in
    another. No join to a catalog table — a lot's ``product_id`` IS the
    TCGplayer product id the sealed feed is keyed on.
    """
    conn.execute("DROP TABLE IF EXISTS temp._lake_sealed_prices")
    conn.execute(
        "CREATE TEMP TABLE _lake_sealed_prices ("
        "  tcgplayer_product_id INTEGER PRIMARY KEY,"
        "  price                REAL NOT NULL"
        ")"
    )
    owned = conn.execute(
        f"SELECT DISTINCT product_id FROM {ZONE_SEALED_HOLDINGS} WHERE status = 'owned'"
    ).fetchall()
    staged = [
        (product_id, prices[product_id]) for (product_id,) in owned if product_id in prices
    ]
    conn.executemany("INSERT INTO _lake_sealed_prices VALUES (?, ?)", staged)
    return len(staged)


def require_zone_holdings(conn: sqlite3.Connection, tenant: Tenant) -> str:
    """Refuse a tenant whose staged zone read is missing or behind the zone.

    Two refusals, and the second is the one worth having. An **absent**
    staging table is obvious. A **stale** one is not: it values today's
    collection at the holdings of whenever somebody last ran the reader, and
    every number it produces looks entirely reasonable. The shipper's own
    cursor is what makes it detectable — ``shipped_thru`` is the highest seq
    in the zone, ``max_seq`` is the highest seq that was read out of it, and
    the second being behind the first means the read predates the last ship.

    A tenant whose OUTBOX is ahead of the zone is *not* refused, and that stays
    true now that this is the only path (pd-i08u). It means the shipper skipped
    that tenant, and the shipper is what says so — ``deploy/ship.sh`` names them
    in the same nightly run and pushes its own PARTIAL warning. Refusing here
    would report one shipping failure twice and, worse, would withhold a
    valuation of holdings that are genuinely what the offline side was told.
    What this function refuses is the case nothing else can see: a read-back
    that never happened, or one that happened too early.

    Returns the ``read_at`` of the materialisation, for the log line.
    """
    # Both staging tables, because a read-back that produced only the first is
    # one from before sealed had a table of its own — and valuing a collection
    # against a missing sealed table would report every tenant's sealed
    # holdings as nothing owned. That is the under-reporting this bead exists
    # to end, arriving through the back door.
    missing = [
        table
        for table in (ZONE_HOLDINGS, ZONE_SEALED_HOLDINGS)
        if not conn.execute(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            (table,),
        ).fetchone()[0]
    ]
    if missing:
        raise TransformError(
            f"{tenant.handle} has no {', '.join(missing)} — run `pkdump-ship holdings "
            f"--tenant {tenant.database_id}` with a build that carries pd-bbv7 "
            "(deploy/TENANT_ZONE.md)."
        )

    row = conn.execute(
        "SELECT max_seq, read_at, parts, rows, sealed_rows FROM "
        + ZONE_HOLDINGS_RUN
        + " WHERE dataset = ?",
        ("holdings",),
    ).fetchone()
    if row is None:
        raise TransformError(
            f"{tenant.handle} has no staged zone holdings — run `pkdump-ship holdings "
            f"--tenant {tenant.database_id}` first (deploy/TENANT_ZONE.md). Valuing from a "
            "missing table would be valuing from nothing; valuing from a stale one would be "
            "worse, so neither is guessed at."
        )
    max_seq, read_at, parts, rows, sealed_rows = row

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
    # A third staleness, and the only one a version rollback can produce: an
    # older `pkdump-ship holdings` drops and rebuilds `zone_holdings` while
    # leaving `zone_sealed_holdings` exactly as it found it, and rewrites the
    # run row without a `sealed_rows` to put in it. The staging table is then
    # some earlier read's sealed lots beside this read's cards, which is the
    # same "plausible wrong number" the max_seq check exists for — and this is
    # the one thing that can see it, because the two counts are written in ONE
    # transaction by any build that materialises both.
    staged_sealed = conn.execute(f"SELECT COUNT(*) FROM {ZONE_SEALED_HOLDINGS}").fetchone()[0]
    if staged_sealed != sealed_rows:
        raise TransformError(
            f"{tenant.handle}'s {ZONE_SEALED_HOLDINGS} holds {staged_sealed} lot(s) but the "
            f"read that filled {ZONE_HOLDINGS} recorded {sealed_rows} — the two were not "
            "materialised together, so these sealed lots belong to an earlier read of the "
            f"zone. Re-run `pkdump-ship holdings --tenant {tenant.database_id}`."
        )

    print(
        f"    zone: {rows} card(s) + {sealed_rows} sealed lot(s) from {parts} part(s) "
        f"through seq {max_seq}, read {read_at}"
    )
    return read_at


def snapshot(
    tenant: Tenant,
    shared: Path,
    prices: dict[tuple[int, str], float],
    sealed_prices: dict[int, float],
    date: dt.date,
    *,
    identifier: str,
    lake_ref: str,
    dry_run: bool = False,
) -> int:
    """Compute one tenant's snapshot for ``date``. Returns the rows written.

    Writes them unless ``dry_run``.

    Raises on anything that makes this tenant unsnapshottable — a missing file,
    a database another process holds, a schema that predates the provenance
    table, a zone read that never happened. The caller logs it and moves to the
    next tenant.
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
        require_zone_holdings(conn, tenant)

        conn.execute("BEGIN IMMEDIATE")
        staged = stage_prices(conn, prices)
        staged_sealed = stage_sealed_prices(conn, sealed_prices)
        conn.execute("DROP TABLE IF EXISTS temp._snap_owned")
        conn.execute("DROP TABLE IF EXISTS temp._snap_sealed")
        # `execute`, not `executescript`: each is one statement, and
        # executescript commits whatever transaction is open before it runs.
        conn.execute(OWNED_SQL)
        conn.execute(OWNED_SEALED_SQL)
        copies = conn.execute("SELECT COUNT(*) FROM _snap_owned").fetchone()[0]
        lots, units, unpriced = conn.execute(
            "SELECT COUNT(*), COALESCE(SUM(quantity), 0), "
            "       COUNT(*) FILTER (WHERE market_price IS NULL) FROM _snap_sealed"
        ).fetchone()

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
            print(
                f"    dry-run: {written} row(s) from {copies} owned cop(ies) "
                f"in {ZONE_HOLDINGS} and {lots} sealed lot(s) ({units} unit(s)), not written"
            )
            return written
        conn.execute("COMMIT")
        print(
            f"    {written} row(s) from {copies} owned cop(ies) in {ZONE_HOLDINGS} "
            f"and {lots} sealed lot(s) ({units} unit(s)), "
            f"{staged} price(s) + {staged_sealed} sealed price(s) staged"
        )
        # Named rather than silently skipped: a lot nobody quotes contributes
        # nothing to the value and every unit to the count, and the operator
        # is entitled to know the number is short and by how many products.
        if unpriced:
            print(
                f"    {unpriced} owned sealed lot(s) have no price at all and were NOT "
                "valued — their units are still counted, never valued at zero"
            )
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
    sealed_identifier: str = DEFAULT_SEALED_TABLE,
    ref: str | None = None,
    only: list[str] | None = None,
    dry_run: bool = False,
) -> list[Outcome]:
    """Snapshot every registered tenant for ``date``. One tenant's failure is not the run's."""
    registered, shared, prices, sealed_prices, lake_ref = _prepare(
        date, root, identifier, sealed_identifier, ref, only
    )

    outcomes = []
    for tenant in registered:
        print(f"==> {tenant.handle} ({tenant.database_id})")
        try:
            rows = snapshot(
                tenant,
                shared,
                prices,
                sealed_prices,
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


def _prepare(
    date: dt.date,
    root: Path,
    identifier: str,
    sealed_identifier: str,
    ref: str | None,
    only: list[str] | None,
) -> tuple[list[Tenant], Path, dict[tuple[int, str], float], dict[int, float], str]:
    """Everything the run needs before it touches a tenant.

    Its own function because every tenant must be valued from ONE pinned
    catalog commit and one price map — see :func:`pin` — and that is as true
    of the two feeds against each other as it is of the tenants: the cards and
    the sealed product are read at the same commit, so a collection's two
    halves are never priced from two catalogs.
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
    print(f"==> lake {identifier} + {sealed_identifier} at {lake_ref}")
    print(f"==> data directory {root}, {len(registered)} active tenant(s)")
    prices = market_prices(identifier, date, lake_ref)
    if not prices:
        raise TransformError(
            f"{identifier} holds no {PRICE_TYPE} price at or before {date} at {lake_ref} — "
            "there is nothing to value a collection with"
        )
    # Not refused when empty, unlike the card feed above. A lake can honestly
    # hold no sealed price for a date — every collection's sealed dimension is
    # then simply not written — where a lake with no CARD price for a date is
    # a lake that cannot value anything and is the run's problem, not a
    # tenant's. The table being absent altogether is still a refusal; see
    # :func:`sealed_market_prices`.
    sealed_prices = sealed_market_prices(sealed_identifier, date, lake_ref)
    return registered, shared, prices, sealed_prices, lake_ref


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
        "--sealed-table",
        default=DEFAULT_SEALED_TABLE,
        help=f"the sealed price feed (default {DEFAULT_SEALED_TABLE})",
    )
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

    # There is deliberately no flag for where the holdings come from. pd-szh2
    # had `--holdings {collection,zone}` and a `--compare` that ran both;
    # pd-i08u deleted the online read once that comparison was clean, so the
    # answer is `zone_holdings` and there is nothing to ask.

    date = dt.date.fromisoformat(args.date)
    try:
        outcomes = run(
            date,
            root=data_dir(args.data_dir),
            identifier=args.table,
            sealed_identifier=args.sealed_table,
            ref=args.ref,
            only=args.tenant,
            dry_run=args.dry_run,
        )
    except TransformError as exc:
        print(f"!! {exc}", file=sys.stderr)
        return EXIT_FAILED
    return report(outcomes, date)


if __name__ == "__main__":
    sys.exit(main())
