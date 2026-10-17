"""Hermetic test for pkdump_lake.verify's sealed comparison (sp-gc251u).

Reproduces the class prod hit on 2026-10-16: sealed_prices rows SQLite holds
and the lake lacks, because shared.sealed_products accumulates across nights
and the lake classifies from one night's listing. Those must be reported and
pass; a row for a product the night DOES list as sealed must still fail.
"""
import datetime as dt
import io
import sys
import types
import unittest
from contextlib import redirect_stdout
from pathlib import Path

# pyiceberg is the image's, not the host's: stub the modules verify pulls it in through.
pyiceberg = types.ModuleType("pyiceberg")
expressions = types.ModuleType("pyiceberg.expressions")
expressions.EqualTo = lambda *a: None
sys.modules.update({"pyiceberg": pyiceberg, "pyiceberg.expressions": expressions})

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lake" / "src"))
pkg = types.ModuleType("pkdump_lake")
pkg.__path__ = [str(Path(__file__).resolve().parents[2] / "lake" / "src" / "pkdump_lake")]
sys.modules["pkdump_lake"] = pkg
for name, attrs in {
    "catalog": {"catalog": None},
    "raw": {"open_raw": None},
    "prices": {
        "DEFAULT_TABLE": "catalog.prices",
        "SEALED_TABLE": "catalog.sealed_prices",
        "PRICE_TYPES": [("low", "lowPrice"), ("mid", "midPrice"), ("high", "highPrice"),
                        ("market", "marketPrice"), ("directLow", "directLowPrice")],
        "sealed_product_ids": None,
    },
}.items():
    m = types.ModuleType(f"pkdump_lake.{name}")
    m.__dict__.update(attrs)
    sys.modules[f"pkdump_lake.{name}"] = m
from pkdump_lake import verify as v  # noqa: E402

DAY = dt.date(2026, 10, 16)
LISTED = {(1, "market"), (1, "low")}


def run(sqlite_sealed, lake_sealed, night_ids):
    # catalog.prices holds every product's prices, sealed or not.
    v.lake_rows = lambda *a: {(pid, "Normal", pt): val for (pid, pt), val in sqlite_sealed.items()}
    v.lake_sealed_rows = lambda *a: lake_sealed
    v.sqlite_sides = lambda *a: ({}, sqlite_sealed, {pid for pid, _ in sqlite_sealed})
    out = io.StringIO()
    with redirect_stdout(out):
        rc = v.verify("t", "x", DAY, night_sealed_ids=night_ids)
    return rc, out.getvalue()


class SealedComparison(unittest.TestCase):
    def test_history_only_rows_are_reported_not_failed(self):
        sqlite = {k: 1.0 for k in LISTED} | {(900, "low"): 2.0, (900, "mid"): 3.0}
        rc, out = run(sqlite, {k: 1.0 for k in LISTED}, {1})
        self.assertEqual(rc, 0, out)
        self.assertIn("2 sealed row(s) over 1 product(s) exist only in sqlite", out)

    def test_a_row_for_a_listed_sealed_product_still_fails(self):
        sqlite = {k: 1.0 for k in LISTED}
        rc, out = run(sqlite, {(1, "market"): 1.0}, {1})
        self.assertEqual(rc, 1, out)
        self.assertIn("(1, 'low') in sqlite, not in the lake", out)

    def test_without_a_classification_everything_fails_as_before(self):
        rc, out = run({(900, "low"): 2.0}, {}, None)
        self.assertEqual(rc, 1, out)


if __name__ == "__main__":
    unittest.main()
