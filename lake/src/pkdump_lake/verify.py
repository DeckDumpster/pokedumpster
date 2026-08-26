"""Check ``catalog.prices`` against the ``shared.sqlite`` built from the same bytes.

pd-1ojt asks for this and says why: ``shared.sqlite`` already holds this data,
which makes the new table checkable rather than a matter of faith. A mismatch
is a finding about **whichever side is wrong**, and the bead is explicit that
a defect in the SQLite parser would be the more valuable discovery of the two.

The two sides are not the same shape, and the difference is not an accident:

* ``shared.prices`` is the narrow time series for **single cards only**, at
  the same grain this table uses.
* ``shared.sealed_prices`` is a **wide** row per sealed product per day —
  five price columns, and ``UNIQUE(product, observed_at)``, so it has no
  ``sub_type_name`` at all.
* ``catalog.prices`` holds every product's prices, narrow, because a price
  payload does not say which kind of product it describes.

So the check has three halves. Restricted to products the catalog calls single
cards, ``catalog.prices`` and ``shared.prices`` must agree **exactly** — same
rows, same values. Restricted to sealed products, every price SQLite kept must
be present in ``catalog.prices``; the lake may hold more, and where it does,
that is SQLite dropping data rather than the lake inventing it.

And ``catalog.sealed_prices`` (pd-bbv7) is checked **exactly**, in both
directions, because it is built to be the same thing ``shared.sealed_prices``
is: one row per (product, price_type, day), resolved by the same
first-entry-wins-per-product rule. That is the strictest comparison here, and
deliberately so — it is the table a collection's sealed half is valued from,
and the one thing that would make a wrong number look reasonable is a sealed
feed that is nearly right.

Run it against a copy of a real catalog::

    pkdump-lake-verify-prices --sqlite /path/shared.sqlite --observed-date 2026-08-11

A **copy**: the connection is `query_only` and issues nothing but `SELECT`, but
SQLite still opens a WAL database for writing at the file level, and pointing
any tool at a catalog a live instance is serving is not a thing to do casually.
"""

from __future__ import annotations

import argparse
import datetime as dt
import sqlite3
import sys

from pyiceberg.expressions import EqualTo

from .catalog import catalog
from .prices import DEFAULT_TABLE, PRICE_TYPES, SEALED_TABLE

#: How many matched rows to print as evidence. The bead asks for "a sampled
#: set of prices", so the sample is shown rather than merely counted.
DEFAULT_SAMPLE = 10


class Mismatch(RuntimeError):
    """The lake and the SQLite catalog disagree."""


def lake_rows(identifier: str, observed_date: dt.date) -> dict[tuple[int, str, str], float]:
    """``catalog.prices`` for one day, keyed at the grain."""
    table = catalog().load_table(identifier)
    scan = table.scan(row_filter=EqualTo("observed_date", observed_date.isoformat()))
    batch = scan.to_arrow().to_pylist()
    return {
        (row["tcgplayer_product_id"], row["sub_type_name"], row["price_type"]): row["price"]
        for row in batch
    }


def lake_sealed_rows(identifier: str, observed_date: dt.date) -> dict[tuple[int, str], float]:
    """``catalog.sealed_prices`` for one day, keyed at its grain."""
    table = catalog().load_table(identifier)
    scan = table.scan(row_filter=EqualTo("observed_date", observed_date.isoformat()))
    return {
        (row["tcgplayer_product_id"], row["price_type"]): row["price"]
        for row in scan.to_arrow().to_pylist()
    }


def sqlite_sides(
    path: str, observed_date: dt.date
) -> tuple[dict[tuple[int, str, str], float], dict[tuple[int, str], float], set[int]]:
    """``prices``, ``sealed_prices`` unpivoted, and the sealed product ids."""
    day = observed_date.isoformat()
    # Not `mode=ro`: these databases are opened WAL (`open_shared`), and SQLite
    # cannot read a WAL database through a read-only connection at all — it
    # needs to manage the `-shm`. `query_only` is the honest equivalent: the
    # connection refuses to change anything, and SQLite keeps its machinery.
    conn = sqlite3.connect(path)
    conn.execute("PRAGMA query_only = ON")
    try:
        singles = {
            (int(pid), sub, price_type): float(price)
            for pid, sub, price_type, price in conn.execute(
                "SELECT tcgplayer_product_id, sub_type_name, price_type, price "
                "FROM prices WHERE observed_at = ?",
                (day,),
            )
        }
        sealed: dict[tuple[int, str], float] = {}
        columns = ", ".join(
            {
                "low": "low_price",
                "mid": "mid_price",
                "high": "high_price",
                "market": "market_price",
                "directLow": "direct_low_price",
            }[price_type]
            for price_type, _ in PRICE_TYPES
        )
        for row in conn.execute(
            f"SELECT tcgplayer_product_id, {columns} FROM sealed_prices WHERE observed_at = ?",
            (day,),
        ):
            for (price_type, _), value in zip(PRICE_TYPES, row[1:]):
                if value is not None:
                    sealed[(int(row[0]), price_type)] = float(value)
        sealed_ids = {int(r[0]) for r in conn.execute("SELECT product_id FROM sealed_products")}
    finally:
        conn.close()
    return singles, sealed, sealed_ids


def report(differences: list[str], label: str, limit: int = 10) -> None:
    print(f"    {len(differences)} {label}")
    for line in differences[:limit]:
        print(f"      {line}")
    if len(differences) > limit:
        print(f"      … and {len(differences) - limit} more")


def verify(
    identifier: str,
    sqlite_path: str,
    observed_date: dt.date,
    sample: int = DEFAULT_SAMPLE,
    sealed_identifier: str = SEALED_TABLE,
) -> int:
    lake = lake_rows(identifier, observed_date)
    singles, sealed, sealed_ids = sqlite_sides(sqlite_path, observed_date)

    print(f"==> observed_date={observed_date}")
    print(f"    lake  {identifier:<24} {len(lake):>9} rows")
    print(f"    sqlite prices                     {len(singles):>9} rows")
    print(f"    sqlite sealed_prices (unpivoted)  {len(sealed):>9} rows")

    lake_singles = {key: value for key, value in lake.items() if key[0] not in sealed_ids}
    lake_sealed = {key: value for key, value in lake.items() if key[0] in sealed_ids}
    print(f"    of which single-card              {len(lake_singles):>9} rows")
    print(f"    of which sealed                   {len(lake_sealed):>9} rows")

    failures = 0

    print("==> single cards: the two must agree exactly")
    missing = [f"{key} in sqlite, not in the lake" for key in singles.keys() - lake_singles.keys()]
    extra = [f"{key} in the lake, not in sqlite" for key in lake_singles.keys() - singles.keys()]
    differing = [
        f"{key}: sqlite {singles[key]!r} vs lake {lake_singles[key]!r}"
        for key in singles.keys() & lake_singles.keys()
        if singles[key] != lake_singles[key]
    ]
    if missing or extra or differing:
        failures += 1
        report(missing, "row(s) only in sqlite")
        report(extra, "row(s) only in the lake")
        report(differing, "row(s) whose price differs")
    else:
        print(f"    ok   {len(singles)} rows identical, value for value")

    print("==> sealed products: every price sqlite kept must be in the lake")
    # SQLite's sealed row has no sub_type, so a sealed product quoted under
    # two sub-types collapses to one row there and stays two here. The lake
    # holding more is the expected direction; the lake holding less is not.
    by_product_type: dict[tuple[int, str], set[float]] = {}
    for (pid, _sub, price_type), value in lake_sealed.items():
        by_product_type.setdefault((pid, price_type), set()).add(value)
    lost = [
        f"{key}: sqlite {value!r}, lake has {sorted(by_product_type.get(key, ()))}"
        for key, value in sealed.items()
        if value not in by_product_type.get(key, ())
    ]
    if lost:
        failures += 1
        report(lost, "sealed price(s) sqlite holds and the lake does not")
    else:
        print(f"    ok   all {len(sealed)} sealed prices present in the lake")
    collapsed = len(lake_sealed) - len(sealed)
    if collapsed > 0:
        print(
            f"    note {collapsed} sealed price row(s) exist only in the lake — "
            "sealed_prices is UNIQUE(product, observed_at) and has no sub_type_name, so a "
            "sealed product quoted under more than one sub-type loses all but the first in "
            "shared.sqlite. The lake keeps them."
        )

    print(f"==> {sealed_identifier}: the two must agree exactly, both ways")
    # Not "the lake may hold more", as above. This table is built to be what
    # `shared.sealed_prices` is, so a difference in EITHER direction is a
    # defect in one of them — which is the whole reason it can be valued from.
    lake_sealed_table = lake_sealed_rows(sealed_identifier, observed_date)
    print(f"    lake  {sealed_identifier:<24} {len(lake_sealed_table):>9} rows")
    missing = [
        f"{key} in sqlite, not in the lake" for key in sealed.keys() - lake_sealed_table.keys()
    ]
    extra = [
        f"{key} in the lake, not in sqlite" for key in lake_sealed_table.keys() - sealed.keys()
    ]
    differing = [
        f"{key}: sqlite {sealed[key]!r} vs lake {lake_sealed_table[key]!r}"
        for key in sealed.keys() & lake_sealed_table.keys()
        if sealed[key] != lake_sealed_table[key]
    ]
    if missing or extra or differing:
        failures += 1
        report(missing, "row(s) only in sqlite")
        report(extra, "row(s) only in the lake")
        report(differing, "row(s) whose price differs")
    else:
        print(f"    ok   {len(sealed)} sealed rows identical, value for value")

    print(f"==> sample of {sample} matched rows")
    for key in sorted(singles)[:sample]:
        print(f"    {key} price {singles[key]!r} (both sides)")

    if failures:
        print("==> MISMATCH")
        return 1
    print(f"==> {identifier}, {sealed_identifier} and shared.sqlite agree")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="pkdump-lake-verify-prices",
        description="Compare catalog.prices and catalog.sealed_prices against a shared.sqlite "
        "for one observed_date.",
    )
    parser.add_argument("--sqlite", required=True, help="path to a shared.sqlite (opened read-only)")
    parser.add_argument("--observed-date", required=True, help="YYYY-MM-DD")
    parser.add_argument("--table", default=DEFAULT_TABLE)
    parser.add_argument("--sealed-table", default=SEALED_TABLE)
    parser.add_argument("--sample", type=int, default=DEFAULT_SAMPLE)
    args = parser.parse_args(argv)
    return verify(
        args.table,
        args.sqlite,
        dt.date.fromisoformat(args.observed_date),
        sample=args.sample,
        sealed_identifier=args.sealed_table,
    )


if __name__ == "__main__":
    sys.exit(main())
