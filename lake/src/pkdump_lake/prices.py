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

Two tables, one job (pd-bbv7)
-----------------------------

The same run also writes :data:`SEALED_TABLE` — ``catalog.sealed_prices`` —
and it is the same job rather than a second one for a reason that is not
tidiness: the two tables MUST come from the same night. Building them
separately makes "the same ``run=`` ULID" a thing to check afterwards; building
them from one :func:`read_date` makes it a thing that cannot be otherwise, and
both tables carry the identical ``pkdump.raw-runs`` property to say so.

``catalog.prices`` is **unchanged** by this. It still holds every product's
prices, sealed and single alike, exactly as described above — nothing is moved
out of it, so nothing downstream of it can move either. ``catalog.sealed_prices``
is the *resolved* sealed series beside it: one row per (product, price_type,
date), at the grain ``shared.sealed_prices`` is keyed on.

Three things about that second table are decisions:

* **No ``sub_type_name``.** ``shared.sealed_prices`` is
  ``UNIQUE(product, observed_at)`` and has no such column — a sealed product is
  one product, where a sub-type is a card's Normal/Holofoil printing. So a
  product quoted under two sub-types collapses to the FIRST entry within a run,
  whole, which is exactly what ``INSERT OR IGNORE`` on that constraint does in
  ``tcgcsv.rs::import_prices``. The rows that collapse are not lost — they are
  in ``catalog.prices``, which keeps everything.
* **What counts as sealed is read from the SAME partition's products
  dataset**, never from a catalog database — see
  :func:`sealed_product_ids`. This table would otherwise depend on
  ``shared.sqlite`` to be correct, which is the coupling the landing zone
  exists to break.
* **A missing products partition is fatal**, not an empty sealed table.
  Classifying nothing as sealed produces a table that says every tenant owns
  no sealed product, and under-reporting a collection's worth is the whole
  defect this bead is fixing.
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

#: The second table the same run writes: the sealed half, resolved. See the
#: module docs.
SEALED_TABLE = "catalog.sealed_prices"

#: The landing-zone coordinates it reads. Japanese groups (TCGCSV category
#: 85) land under the same ``source``/``dataset`` as English ones, so both are
#: picked up without a special case — which is also true of the SQLite
#: importer this table is checked against.
SOURCE = "tcgcsv"
DATASET = "prices"

#: Where "is this product sealed?" is answered from — the same ``ingest_date``
#: of the same source, so the classification and the prices are the same
#: night's bytes.
PRODUCTS_DATASET = "products"

#: TCGplayer's category for Pokémon Japan. It is the one category whose single
#: cards are NOT discriminated by an extended-data ``Number`` — ~2.4k vintage
#: Japanese products carry none — so ``japan.rs::is_card`` reads ``CardType``
#: instead, and this must too or 450 groups' worth of Japanese singles are
#: classified as sealed product.
CATEGORY_POKEMON_JAPAN = 85

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

#: ``catalog.sealed_prices``. The same shape one column narrower — see the
#: module docs for why ``sub_type_name`` is absent rather than nullable.
SEALED_SCHEMA = Schema(
    NestedField(1, "tcgplayer_product_id", LongType(), required=True),
    NestedField(2, "price_type", StringType(), required=True),
    NestedField(3, "price", DoubleType(), required=True),
    NestedField(4, "observed_date", DateType(), required=True),
)

#: The same partitioning, for the same reason.
SEALED_PARTITION_SPEC = PartitionSpec(
    PartitionField(source_id=4, field_id=1000, transform=IdentityTransform(), name="observed_date")
)


class BuildError(RuntimeError):
    """The raw bytes for a date are not what a price payload should be."""


def results_of(body: bytes, key: str = "<part>") -> list[dict]:
    """The ``results`` array of one landed TCGCSV envelope. Pure, no IO.

    Split out from :func:`parse_envelope` so that one part is JSON-decoded
    once and reduced twice — into the rows of both tables this job writes.
    """
    try:
        envelope = json.loads(body)
    except json.JSONDecodeError as exc:
        raise BuildError(f"{key}: not JSON ({exc})") from exc
    results = envelope.get("results")
    if not isinstance(results, list):
        raise BuildError(f"{key}: TCGCSV envelope missing 'results'")
    return results


def price_rows(results: list[dict], observed_date: dt.date) -> list[dict]:
    """``results`` into rows at ``catalog.prices``'s grain."""
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


def sealed_rows_by_product(
    results: list[dict], observed_date: dt.date, sealed_ids: set[int]
) -> dict[int, list[dict]]:
    """``results`` into ``catalog.sealed_prices`` rows, keyed by product.

    Keyed by product rather than flattened because the collapse this table
    performs is at the PRODUCT level: the first entry quoting a sealed product
    wins **whole**, which is what ``INSERT OR IGNORE`` on
    ``UNIQUE(product, observed_at)`` does to the second one in
    ``tcgcsv.rs::import_prices``. Reducing per (product, price_type) instead
    would take the market from one sub-type's entry and the mid from another's
    — a price pair nobody quoted.
    """
    out: dict[int, list[dict]] = {}
    for entry in results:
        product_id = int(entry["productId"])
        if product_id not in sealed_ids or product_id in out:
            continue
        out[product_id] = [
            {
                "tcgplayer_product_id": product_id,
                "price_type": price_type,
                "price": float(entry[field]),
                "observed_date": observed_date,
            }
            for price_type, field in PRICE_TYPES
            if entry.get(field) is not None
        ]
    return out


def parse_envelope(body: bytes, observed_date: dt.date, *, key: str = "<part>") -> list[dict]:
    """One landed TCGCSV price response into rows at the table's grain.

    Pure: bytes in, rows out, no IO. It is the only place raw JSON becomes a
    price, so it is the only place that has to match ``import_prices``.
    """
    return price_rows(results_of(body, key), observed_date)


def is_sealed(product: dict, category_id: int) -> bool:
    """Whether a landed TCGCSV product is sealed product rather than a card.

    The transliteration of the two Rust importers, and it needs the category
    because they do not agree on the discriminator:

    * category 3 (English) — ``tcgcsv.rs::is_single_card``: a card is a product
      with an extended-data ``Number``.
    * category 85 (Japan) — ``japan.rs::is_card``: a card is a product with an
      extended-data ``CardType``. ~40% of Japanese vintage products carry no
      number at all, so the English rule would file every one of them as
      sealed product.

    A product that is not a card is sealed, in both. That is the same
    disjunction ``import_sealed_products`` makes, so the set this produces is
    ``shared.sealed_products`` without needing to open it.
    """
    field = "CardType" if category_id == CATEGORY_POKEMON_JAPAN else "Number"
    return not any(
        isinstance(e, dict) and str(e.get("name", "")).lower() == field.lower()
        for e in product.get("extendedData") or ()
    )


def category_of(url: str, key: str) -> int:
    """The TCGplayer category a landed products URL was fetched from.

    ``{base}/{category}/{group}/products`` — ``tcgcsv.rs::TcgcsvClient::get``
    builds every URL that way, for both categories, and the manifest records
    it. Read rather than guessed, because guessing wrong misclassifies a whole
    category; refused rather than defaulted, because the default would be the
    English rule silently applied to Japanese cards.
    """
    parts = url.split("?")[0].rstrip("/").split("/")
    if len(parts) < 3 or parts[-1] != PRODUCTS_DATASET:
        raise BuildError(f"{key}: {url!r} is not a TCGCSV products URL")
    try:
        return int(parts[-3])
    except ValueError as exc:
        raise BuildError(f"{key}: no TCGplayer category in {url!r}") from exc


def sealed_product_ids(
    zone: RawZone, ingest_date: str, *, allow_incomplete: bool = False
) -> tuple[set[int], list[str]]:
    """Every sealed product id in one ``ingest_date``'s products dataset.

    Returns the ids and the run ULIDs they were read from. The SAME partition
    the prices come from, so a rebuild of an old date classifies it by the
    catalog as it stood that night rather than as it stands now.

    Refuses an empty partition. An empty sealed set is not "no sealed product
    was quoted" — it is "nothing here can be recognised as sealed", and it
    would produce a table saying every tenant's sealed holdings are worth
    nothing.
    """
    runs = select_runs(
        zone.runs(SOURCE, PRODUCTS_DATASET, ingest_date), allow_incomplete=allow_incomplete
    )
    ids: set[int] = set()
    parts = 0
    for run in runs:
        for part in run.parts:
            parts += 1
            category = category_of(part.url, part.key)
            for product in results_of(zone.payload(part), part.key):
                if is_sealed(product, category):
                    ids.add(int(product["productId"]))
    if not parts:
        raise BuildError(
            f"ingest_date={ingest_date} landed no {PRODUCTS_DATASET} parts, so nothing can "
            f"say which products are sealed. {SEALED_TABLE} is not built from a guess: an "
            "empty sealed table reads as 'every tenant owns no sealed product'."
        )
    print(f"    {len(ids)} sealed product id(s) from {parts} {PRODUCTS_DATASET} part(s)")
    return ids, [run.run_id for run in runs]


def grain(row: dict) -> tuple[int, str, str]:
    """A row's key within one ``observed_date``."""
    return (row["tcgplayer_product_id"], row["sub_type_name"], row["price_type"])


def read_date(
    zone: RawZone,
    ingest_date: str,
    observed_date: dt.date,
    *,
    sealed_ids: set[int] | None = None,
    allow_incomplete: bool = False,
) -> tuple[list[dict], list[dict], dict[str, str]]:
    """Every price landed for one ``ingest_date``, plus how it was assembled.

    Returns ``(price rows, sealed price rows, provenance)`` — both tables from
    ONE pass over the parts, which is what makes them the same night's bytes
    by construction rather than by inspection. ``sealed_ids`` is what
    :func:`sealed_product_ids` read out of the same partition; ``None`` means
    the caller does not want the sealed half and the second list is empty.

    Two collisions are possible at the grain, and they resolve in opposite
    directions on purpose:

    * **Within one run** — the same product quoted from two groups — the
      *first* wins, because that is what ``INSERT OR IGNORE`` does on the
      SQLite side and the two must not disagree by tie-break.
    * **Across runs**, read oldest first, the *later* wins: a retry exists to
      supersede the attempt that failed.

    Both rules apply to the sealed side too, one level up: there the unit that
    collapses is the whole product, not one of its prices.

    The last return value goes into the Iceberg snapshot's properties of BOTH
    tables: a table nobody can trace back to the objects it came from is a
    table you have to trust rather than check, and two tables that cannot be
    traced to the same objects are two tables that can silently come from
    different nights.
    """
    runs = select_runs(zone.runs(SOURCE, DATASET, ingest_date), allow_incomplete=allow_incomplete)

    merged: dict[tuple[int, str, str], dict] = {}
    sealed_merged: dict[int, list[dict]] = {}
    parts: list[Part] = []
    seen = 0
    for run in runs:
        print(f"    {run.describe()}")
        parts.extend(run.parts)
        within_run: dict[tuple[int, str, str], dict] = {}
        within_run_sealed: dict[int, list[dict]] = {}
        for part in run.parts:
            results = results_of(zone.payload(part), part.key)
            for row in price_rows(results, observed_date):
                seen += 1
                within_run.setdefault(grain(row), row)
            if sealed_ids is not None:
                for product_id, rows_of in sealed_rows_by_product(
                    results, observed_date, sealed_ids
                ).items():
                    within_run_sealed.setdefault(product_id, rows_of)
        merged.update(within_run)
        sealed_merged.update(within_run_sealed)
    if not parts:
        raise BuildError(f"ingest_date={ingest_date} landed no price parts")

    rows = list(merged.values())
    if len(rows) != seen:
        print(f"    {seen - len(rows)} row(s) collapsed at the grain")
    sealed = [row for rows_of in sealed_merged.values() for row in rows_of]

    provenance = {
        "pkdump.built-from": "raw",
        "pkdump.raw-source": SOURCE,
        "pkdump.raw-dataset": DATASET,
        "pkdump.raw-ingest-date": ingest_date,
        "pkdump.raw-runs": ",".join(run.run_id for run in runs),
        "pkdump.raw-parts": str(len(parts)),
        "pkdump.raw-complete": "true" if all(run.complete for run in runs) else "false",
    }
    return rows, sealed, provenance


def load_or_create(cat, identifier: str, schema: Schema, partition_spec: PartitionSpec):
    """The table, created on first build with the schema and partitioning given."""
    try:
        return cat.load_table(identifier)
    except NoSuchTableError:
        namespace = identifier.rsplit(".", 1)[0]
        cat.create_namespace_if_not_exists(namespace)
        print(f"==> creating {identifier}")
        return cat.create_table(identifier, schema=schema, partition_spec=partition_spec)


def _write_day(cat, identifier, schema, partition_spec, rows, observed_date, provenance) -> None:
    """One day of one table, replaced. Shared by both tables this job writes."""
    table = load_or_create(cat, identifier, schema, partition_spec)
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


def build(
    identifier: str,
    ingest_date: str,
    observed_date: dt.date,
    *,
    sealed_identifier: str = SEALED_TABLE,
    allow_incomplete: bool = False,
    dry_run: bool = False,
) -> tuple[int, int]:
    """Rebuild one ``observed_date`` of both price tables from ``raw/``.

    Returns ``(catalog.prices rows, catalog.sealed_prices rows)``.
    """
    zone = open_raw()
    print(f"==> raw landing zone: {zone.description}")
    print(f"==> reading source={SOURCE} dataset={PRODUCTS_DATASET} ingest_date={ingest_date}")
    sealed_ids, product_runs = sealed_product_ids(
        zone, ingest_date, allow_incomplete=allow_incomplete
    )
    print(f"==> reading source={SOURCE} dataset={DATASET} ingest_date={ingest_date}")
    rows, sealed, provenance = read_date(
        zone,
        ingest_date,
        observed_date,
        sealed_ids=sealed_ids,
        allow_incomplete=allow_incomplete,
    )
    print(f"    {len(rows)} price rows for observed_date={observed_date}")
    print(
        f"    {len(sealed)} sealed price row(s) for {sealed_identifier} — resolved to one row "
        "per product/price type, which is not the same count as the sealed rows in "
        f"{identifier} (a sealed product quoted under two sub-types keeps both there)"
    )

    if dry_run:
        print("==> --dry-run: nothing written")
        return len(rows), len(sealed)

    cat = catalog()
    branch = cat.properties.get("prefix") or "main"
    if "@" in branch:
        raise BuildError(f"PKDUMP_LAKE_REF must be a branch to write, got {branch!r}")

    _write_day(cat, identifier, SCHEMA, PARTITION_SPEC, rows, observed_date, provenance)
    # The SAME provenance, plus which runs of the products dataset said what
    # was sealed. Identical `pkdump.raw-runs` on both tables is the property
    # that makes "these two came from the same night" checkable.
    _write_day(
        cat,
        sealed_identifier,
        SEALED_SCHEMA,
        SEALED_PARTITION_SPEC,
        sealed,
        observed_date,
        {**provenance, "pkdump.raw-products-runs": ",".join(product_runs)},
    )
    print(f"    nessie commit {head_hash(branch)}   (ref {branch})")
    print(f"    provenance     {provenance}")
    return len(rows), len(sealed)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="pkdump-lake-build-prices",
        description="Build catalog.prices and catalog.sealed_prices for one date from the raw "
        "landing zone. No upstream.",
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
        "--sealed-table",
        default=SEALED_TABLE,
        help=f"the sealed half, written by the same run (default {SEALED_TABLE})",
    )
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
        sealed_identifier=args.sealed_table,
        allow_incomplete=args.allow_incomplete,
        dry_run=args.dry_run,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
