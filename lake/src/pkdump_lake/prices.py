"""Build ``catalog.prices`` from the raw landing zone. Nothing else.

pd-1ojt. The assertion this job exists to make good on is a negative one:
**it never calls an upstream.** Everything it needs is in ``raw/``, which is
the whole premise of the landing zone — if a table could only be rebuilt by
re-fetching, ``raw/`` would be decorative and the design would be wrong.
``tests/lake/prices.sh`` runs it on a podman ``--internal`` network, where
there is no route off the host to take.

Grain, one row per price actually quoted::

    (tcgplayer_product_id, sub_type_name, price_type, observed_date)

partitioned by ``observed_date``.

Two things about the shape are worth stating, because both are places where
this table and ``shared.sqlite`` deliberately differ:

* **Every product's prices are here, sealed and single alike.** A TCGCSV
  price payload does not say which a product is; that is a fact about the
  *catalog*, and joining it in would make this table depend on a second
  dataset to be correct. ``shared.sqlite`` splits the same bytes into
  ``prices`` and ``sealed_prices`` at import time, so a comparison has to
  account for the split — ``verify.py`` does, and the split is exactly where
  it found something.
* **``price`` is a double, not a decimal.** TCGCSV quotes JSON numbers and
  ``shared.sqlite`` stores REAL, so a double is the value that round-trips
  both. A decimal would round, and "the sampled prices match" would stop
  meaning what it says.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys

import pyarrow as pa
from pyiceberg.exceptions import NoSuchTableError
from pyiceberg.expressions import EqualTo
from pyiceberg.partitioning import PartitionField, PartitionSpec
from pyiceberg.schema import Schema
from pyiceberg.transforms import IdentityTransform
from pyiceberg.types import DateType, DoubleType, LongType, NestedField, StringType

from .catalog import catalog, head_hash
from .raw import Part, RawZone, open_raw, select_runs

#: The table this job owns.
DEFAULT_TABLE = "catalog.prices"

#: The landing-zone coordinates it reads. Japanese groups (TCGCSV category
#: 85) land under the same ``source``/``dataset`` as English ones, so both are
#: picked up without a special case — which is also true of the SQLite
#: importer this table is checked against.
SOURCE = "tcgcsv"
DATASET = "prices"

#: A TCGCSV price object with no ``subTypeName``. The same default the SQLite
#: importer applies (``tcgcsv.rs::import_prices``); the two must agree or the
#: grain does not line up.
DEFAULT_SUB_TYPE = "Normal"

#: ``price_type`` values and the JSON fields they come from, in the order the
#: SQLite importer writes them. A null is not a price and produces no row.
PRICE_TYPES: tuple[tuple[str, str], ...] = (
    ("low", "lowPrice"),
    ("mid", "midPrice"),
    ("high", "highPrice"),
    ("market", "marketPrice"),
    ("directLow", "directLowPrice"),
)

SCHEMA = Schema(
    NestedField(1, "tcgplayer_product_id", LongType(), required=True),
    NestedField(2, "sub_type_name", StringType(), required=True),
    NestedField(3, "price_type", StringType(), required=True),
    NestedField(4, "price", DoubleType(), required=True),
    NestedField(5, "observed_date", DateType(), required=True),
)

# Identity on the date: a rebuild replaces one day and leaves every other day
# untouched, which is what makes rebuilding an older date safe.
PARTITION_SPEC = PartitionSpec(
    PartitionField(source_id=5, field_id=1000, transform=IdentityTransform(), name="observed_date")
)


class BuildError(RuntimeError):
    """The raw bytes for a date are not what a price payload should be."""


def parse_envelope(body: bytes, observed_date: dt.date, *, key: str = "<part>") -> list[dict]:
    """One landed TCGCSV price response into rows at the table's grain.

    Pure: bytes in, rows out, no IO. It is the only place raw JSON becomes a
    price, so it is the only place that has to match ``import_prices``.
    """
    try:
        envelope = json.loads(body)
    except json.JSONDecodeError as exc:
        raise BuildError(f"{key}: not JSON ({exc})") from exc
    results = envelope.get("results")
    if not isinstance(results, list):
        raise BuildError(f"{key}: TCGCSV envelope missing 'results'")

    rows = []
    for entry in results:
        product_id = entry["productId"]
        sub_type = entry.get("subTypeName") or DEFAULT_SUB_TYPE
        for price_type, field in PRICE_TYPES:
            value = entry.get(field)
            if value is None:
                continue
            rows.append(
                {
                    "tcgplayer_product_id": int(product_id),
                    "sub_type_name": sub_type,
                    "price_type": price_type,
                    "price": float(value),
                    "observed_date": observed_date,
                }
            )
    return rows


def grain(row: dict) -> tuple[int, str, str]:
    """A row's key within one ``observed_date``."""
    return (row["tcgplayer_product_id"], row["sub_type_name"], row["price_type"])


def read_date(
    zone: RawZone,
    ingest_date: str,
    observed_date: dt.date,
    *,
    allow_incomplete: bool = False,
) -> tuple[list[dict], dict[str, str]]:
    """Every price landed for one ``ingest_date``, plus how it was assembled.

    Two collisions are possible at the grain, and they resolve in opposite
    directions on purpose:

    * **Within one run** — the same product quoted from two groups — the
      *first* wins, because that is what ``INSERT OR IGNORE`` does on the
      SQLite side and the two must not disagree by tie-break.
    * **Across runs**, read oldest first, the *later* wins: a retry exists to
      supersede the attempt that failed.

    The second return value goes into the Iceberg snapshot's properties: a
    table nobody can trace back to the objects it came from is a table you
    have to trust rather than check.
    """
    runs = select_runs(zone.runs(SOURCE, DATASET, ingest_date), allow_incomplete=allow_incomplete)

    merged: dict[tuple[int, str, str], dict] = {}
    parts: list[Part] = []
    seen = 0
    for run in runs:
        print(f"    {run.describe()}")
        parts.extend(run.parts)
        within_run: dict[tuple[int, str, str], dict] = {}
        for part in run.parts:
            for row in parse_envelope(zone.payload(part), observed_date, key=part.key):
                seen += 1
                within_run.setdefault(grain(row), row)
        merged.update(within_run)
    if not parts:
        raise BuildError(f"ingest_date={ingest_date} landed no price parts")

    rows = list(merged.values())
    if len(rows) != seen:
        print(f"    {seen - len(rows)} row(s) collapsed at the grain")

    provenance = {
        "pkdump.built-from": "raw",
        "pkdump.raw-source": SOURCE,
        "pkdump.raw-dataset": DATASET,
        "pkdump.raw-ingest-date": ingest_date,
        "pkdump.raw-runs": ",".join(run.run_id for run in runs),
        "pkdump.raw-parts": str(len(parts)),
        "pkdump.raw-complete": "true" if all(run.complete for run in runs) else "false",
    }
    return rows, provenance


def load_or_create(cat, identifier: str):
    """The table, created on first build with the schema and partitioning above."""
    try:
        return cat.load_table(identifier)
    except NoSuchTableError:
        namespace = identifier.rsplit(".", 1)[0]
        cat.create_namespace_if_not_exists(namespace)
        print(f"==> creating {identifier}")
        return cat.create_table(identifier, schema=SCHEMA, partition_spec=PARTITION_SPEC)


def build(
    identifier: str,
    ingest_date: str,
    observed_date: dt.date,
    *,
    allow_incomplete: bool = False,
    dry_run: bool = False,
) -> int:
    """Rebuild one ``observed_date`` of ``identifier`` from ``raw/``."""
    zone = open_raw()
    print(f"==> raw landing zone: {zone.description}")
    print(f"==> reading source={SOURCE} dataset={DATASET} ingest_date={ingest_date}")
    rows, provenance = read_date(
        zone, ingest_date, observed_date, allow_incomplete=allow_incomplete
    )
    print(f"    {len(rows)} price rows for observed_date={observed_date}")

    if dry_run:
        print("==> --dry-run: nothing written")
        return len(rows)

    cat = catalog()
    branch = cat.properties.get("prefix") or "main"
    if "@" in branch:
        raise BuildError(f"PKDUMP_LAKE_REF must be a branch to write, got {branch!r}")
    table = load_or_create(cat, identifier)

    arrow = pa.Table.from_pylist(rows, schema=table.schema().as_arrow())
    # One commit that removes the day and writes it again: re-running is a
    # replacement rather than a doubling, and no other day is in the filter's
    # reach.
    table.overwrite(
        arrow,
        overwrite_filter=EqualTo("observed_date", observed_date.isoformat()),
        snapshot_properties=provenance,
    )
    print(f"==> wrote {len(rows)} rows to {identifier}")
    print(f"    nessie commit {head_hash(branch)}   (ref {branch})")
    print(f"    provenance     {provenance}")
    return len(rows)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="pkdump-lake-build-prices",
        description="Build catalog.prices for one date from the raw landing zone. No upstream.",
    )
    parser.add_argument(
        "--ingest-date",
        required=True,
        help="the raw/ partition to read, YYYY-MM-DD. Required, and never defaulted from the "
        "clock: rebuilding an older date is the same operation as building today's.",
    )
    parser.add_argument(
        "--observed-date",
        help="the observed_date to write (default: --ingest-date). They differ only when a "
        "fetch crossed UTC midnight.",
    )
    parser.add_argument("--table", default=DEFAULT_TABLE, help=f"default {DEFAULT_TABLE}")
    parser.add_argument(
        "--allow-incomplete",
        action="store_true",
        help="build from runs whose manifest says they did not finish. The result is not a "
        "whole day and the snapshot records that.",
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="read and parse raw, write nothing"
    )
    args = parser.parse_args(argv)

    ingest_date = dt.date.fromisoformat(args.ingest_date)
    observed = dt.date.fromisoformat(args.observed_date) if args.observed_date else ingest_date
    build(
        args.table,
        ingest_date.isoformat(),
        observed,
        allow_incomplete=args.allow_incomplete,
        dry_run=args.dry_run,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
