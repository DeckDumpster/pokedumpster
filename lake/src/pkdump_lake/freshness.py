"""Is ``catalog.prices`` still moving?

pd-up36. The nightly build (:mod:`pkdump_lake.prices`) writes one
``observed_date`` partition per night. The transform tier
(:mod:`pkdump_lake.value_snapshots`) then values every tenant's collection from
the newest quote at or before its ``--date`` — so a lake whose price build has
quietly stopped keeps producing **correct arithmetic over stale prices**,
advancing nightly, with nothing anywhere saying the numbers stopped moving.
That is the failure this job exists to detect, and it is detected by the
**age of the newest partition** and nothing else.

## Why age, and not per-day completeness

The build refuses a day that holds no *complete* run, and ``complete`` is
conservative across datasets (``deploy/LAKE.md`` §1): one landing invocation
carries one ``run=``, so a pokemontcg.io tail that died marks the *prices*
manifest incomplete even though every price fetch succeeded. On a flaky night —
the normal night — that refusal fires while the price bytes are perfectly fine.

A nightly job that failed and paged on that would page most nights, and a pager
that cries wolf gets ignored; this project has already paid a day for exactly
that shape (pd-me6h, where a static-by-design database was judged on freshness).
So the nightly build passes ``--allow-incomplete`` and records
``pkdump.raw-complete=false`` in the snapshot, and the alarm is moved here,
onto the property that actually matters: **has a new day of prices landed in
the table recently.** One incomplete-but-recent day is not an alarm. Three days
of nothing is.

## What it reads

Table *metadata*, never data files. ``inspect.partitions()`` lists the live
partitions of the table, which under this table's identity partitioning on
``observed_date`` (see :data:`pkdump_lake.prices.PARTITION_SPEC`) is exactly
the list of days it holds. A scan would grow with the lake's whole history to
answer a question about one date.

Run it against the lake the build writes to::

    pkdump-lake-prices-age --as-of 2026-08-13 --max-age-days 2

``--as-of`` is **required and never defaulted from the clock**, the same rule
``--ingest-date`` and ``--date`` follow: the scheduler is the one component
allowed to know what day it is, and a job that reads the clock has two
behaviours where it should have one. ``deploy/prices.sh`` names today.
"""

from __future__ import annotations

import argparse
import datetime as dt
import sys

from pyiceberg.exceptions import NoSuchTableError

from .catalog import DEFAULT_REF, catalog

#: The table whose age is judged.
DEFAULT_TABLE = "catalog.prices"

#: The partition column, and the field name it therefore carries inside
#: ``inspect.partitions()``' ``partition`` struct.
PARTITION_FIELD = "observed_date"

#: How far behind ``--as-of`` the newest partition may fall before this is an
#: alarm. Two days, so a single missed night — a failed landing, a box that was
#: off, a build that hit a transient S3 error — is absorbed silently and a
#: build that has actually stopped is caught on its third morning.
DEFAULT_MAX_AGE_DAYS = 2

#: The newest partition is within the window.
EXIT_FRESH = 0
#: The question could not be asked: no catalog reachable, no ref, a table
#: partitioned some other way. Not being able to ask is not the same answer as
#: "fine" — the same rule ``deploy/backup-check.sh`` is built on. Nothing
#: *returns* this: an error propagates and the interpreter exits 1 with the
#: traceback, which is the project's "let it crash visibly" rule. The constant
#: names what the caller will see.
EXIT_ERROR = 1
#: The newest partition is older than the window — or there is no table at all.
#: Its own code, distinct from :data:`EXIT_ERROR`, so a caller can tell "prices
#: have stopped" from "I could not look". 2 is skipped deliberately: argparse
#: exits 2 on a usage error and would be indistinguishable from a verdict.
EXIT_STALE = 3


class FreshnessError(RuntimeError):
    """The table's partition metadata is not the shape this job requires."""


def newest_partition(identifier: str, ref: str | None = None) -> dt.date | None:
    """The newest ``observed_date`` the table actually holds, or ``None``.

    ``None`` means the table exists and holds no live partition — every day was
    deleted, or nothing was ever committed. The caller treats that exactly as
    it treats a missing table: prices are not arriving.

    :raises NoSuchTableError: the table has never been created.
    """
    table = catalog(ref).load_table(identifier)
    partitions = table.inspect.partitions()

    days: list[dt.date] = []
    for row in partitions.to_pylist():
        partition = row.get("partition")
        if partition is None or PARTITION_FIELD not in partition:
            raise FreshnessError(
                f"{identifier} is not partitioned by {PARTITION_FIELD} — "
                f"inspect.partitions() gave {sorted(partition or {})}. This job judges the "
                "age of a day, so a table partitioned some other way cannot be judged here."
            )
        # A partition whose live files hold no rows is not a day of prices. It
        # is not the normal outcome of a rebuild — the build replaces a day in
        # one commit — but counting it would make an empty table look fresh.
        if not row.get("record_count"):
            continue
        days.append(partition[PARTITION_FIELD])

    return max(days) if days else None


def judge(
    identifier: str,
    as_of: dt.date,
    *,
    max_age_days: int = DEFAULT_MAX_AGE_DAYS,
    ref: str | None = None,
) -> int:
    """Report the table's age against ``as_of`` and return the exit status."""
    resolved = ref or DEFAULT_REF
    print(f"==> {identifier} at {resolved}: newest partition vs {as_of.isoformat()}")

    try:
        newest = newest_partition(identifier, ref)
    except NoSuchTableError:
        print(
            f"prices-age: STALE — there is no {identifier} at {resolved}. Nothing has ever "
            "built it: bash deploy/prices.sh <instance> (deploy/LAKE.md §6)",
            file=sys.stderr,
        )
        return EXIT_STALE

    if newest is None:
        print(
            f"prices-age: STALE — {identifier} at {resolved} holds no price partition at all",
            file=sys.stderr,
        )
        return EXIT_STALE

    age = (as_of - newest).days
    print(f"    newest observed_date={newest.isoformat()} ({age} day(s) behind {as_of})")

    if age > max_age_days:
        print(
            f"prices-age: STALE — the newest {identifier} partition is {newest.isoformat()}, "
            f"{age} days behind {as_of} (limit {max_age_days}). Collections are still being "
            "valued nightly, from prices that stopped arriving.",
            file=sys.stderr,
        )
        return EXIT_STALE

    # A negative age is a partition dated after --as-of: a backfill of a future
    # date, or a clock disagreement. Not this job's alarm — the table is
    # emphatically not stale — but worth saying rather than printing "-1 days".
    if age < 0:
        print(
            f"prices-age: FRESH — {identifier} holds {newest.isoformat()}, which is AFTER "
            f"{as_of}. Nothing is stale; something dated a partition ahead of the caller."
        )
        return EXIT_FRESH

    print(f"prices-age: FRESH — {identifier} is {age} day(s) behind (limit {max_age_days})")
    return EXIT_FRESH


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="pkdump-lake-prices-age",
        description="How far behind is the newest catalog.prices partition? "
        "Exit 0 fresh, 3 stale, 1 could not ask.",
    )
    parser.add_argument(
        "--as-of",
        required=True,
        help="the date to measure the newest partition against, YYYY-MM-DD. Required, and "
        "never defaulted from the clock: the scheduler is the component allowed to know "
        "what day it is.",
    )
    parser.add_argument(
        "--max-age-days",
        type=int,
        default=DEFAULT_MAX_AGE_DAYS,
        help=f"days behind --as-of the newest partition may be (default {DEFAULT_MAX_AGE_DAYS})",
    )
    parser.add_argument("--table", default=DEFAULT_TABLE, help=f"default {DEFAULT_TABLE}")
    parser.add_argument("--ref", help="Nessie ref to read (default $PKDUMP_LAKE_REF, then main)")
    args = parser.parse_args(argv)

    if args.max_age_days < 0:
        parser.error("--max-age-days cannot be negative")

    return judge(
        args.table,
        dt.date.fromisoformat(args.as_of),
        max_age_days=args.max_age_days,
        ref=args.ref,
    )


if __name__ == "__main__":
    sys.exit(main())
