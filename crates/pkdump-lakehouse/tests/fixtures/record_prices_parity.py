"""Records tests/fixtures/prices_parity.json from the Python lake's own functions.

    python3 -I crates/pkdump-lakehouse/tests/fixtures/record_prices_parity.py > \
        crates/pkdump-lakehouse/tests/fixtures/prices_parity.json

Imports lake/src/pkdump_lake/{prices,raw}.py with pyarrow / pyiceberg / requests stubbed:
every function recorded here is pure (bytes in, rows out) and touches none of them. The
Rust port (src/prices_core.rs) must reproduce this file; the file is the oracle, so it is
re-recorded only when the Python changes on purpose.
"""
import datetime as dt
import importlib.util
import json
import sys
import types
from pathlib import Path

root = Path(__file__).resolve().parents[4]
for name in ("pyarrow", "pyarrow.fs", "pyiceberg", "pyiceberg.exceptions", "pyiceberg.expressions",
             "pyiceberg.partitioning", "pyiceberg.schema", "pyiceberg.transforms",
             "pyiceberg.types", "pyiceberg.catalog", "pyiceberg.catalog.rest", "requests"):
    m = types.ModuleType(name)
    sys.modules[name] = m
sys.modules["pyarrow"].fs = sys.modules["pyarrow.fs"]
sys.modules["pyiceberg.exceptions"].NoSuchTableError = type("NoSuchTableError", (Exception,), {})
for n in ("EqualTo",):
    setattr(sys.modules["pyiceberg.expressions"], n, object)
for n in ("PartitionField", "PartitionSpec"):
    setattr(sys.modules["pyiceberg.partitioning"], n, lambda *a, **k: None)
sys.modules["pyiceberg.schema"].Schema = lambda *a, **k: None
sys.modules["pyiceberg.transforms"].IdentityTransform = lambda *a, **k: None
for n in ("DateType", "DoubleType", "LongType", "NestedField", "StringType"):
    setattr(sys.modules["pyiceberg.types"], n, lambda *a, **k: None)
sys.modules["pyiceberg.catalog.rest"].RestCatalog = object

pkg = types.ModuleType("pkdump_lake")
pkg.__path__ = [str(root / "lake/src/pkdump_lake")]
sys.modules["pkdump_lake"] = pkg
cat = types.ModuleType("pkdump_lake.catalog")
cat.catalog = cat.head_hash = None
sys.modules["pkdump_lake.catalog"] = cat
for mod in ("raw", "prices"):
    spec = importlib.util.spec_from_file_location(
        f"pkdump_lake.{mod}", root / f"lake/src/pkdump_lake/{mod}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[f"pkdump_lake.{mod}"] = module
    spec.loader.exec_module(module)
raw = sys.modules["pkdump_lake.raw"]
prices = sys.modules["pkdump_lake.prices"]

DAY = dt.date(2026, 8, 11)


def env(results):
    return json.dumps({"success": True, "results": results})


def strip(rows):
    return [{k: v for k, v in r.items() if k != "observed_date"} for r in rows]


# -- price rows: sub-type default, nulls, ints, empty subtype, zero ------------------------
price_bodies = {
    "plain": env([
        {"productId": 1, "lowPrice": 0.25, "midPrice": 0.5, "highPrice": 3, "marketPrice": 0.4,
         "directLowPrice": None, "subTypeName": "Holofoil"},
        {"productId": 2, "lowPrice": None, "midPrice": None, "highPrice": None,
         "marketPrice": 12.5, "directLowPrice": 11.0},
        {"productId": 3, "subTypeName": "", "marketPrice": 0},
        {"productId": 4},
    ]),
    "empty": env([]),
}
price_cases = [
    {"name": n, "body": b, "expected": strip(prices.price_rows(prices.results_of(b.encode()), DAY))}
    for n, b in price_bodies.items()
]

# -- sealed: first entry wins whole, only sealed ids, null fields skipped -------------------
sealed_body = env([
    {"productId": 100, "subTypeName": "Normal", "lowPrice": 1.0, "midPrice": 2.0, "marketPrice": 1.5},
    {"productId": 100, "subTypeName": "Foil", "lowPrice": 9.0, "midPrice": 9.0, "highPrice": 9.0,
     "marketPrice": 9.0},
    {"productId": 5, "marketPrice": 4.0},
    {"productId": 101, "marketPrice": None},
    {"productId": 102, "lowPrice": 7.5, "directLowPrice": 7.0},
])
sealed_ids = {100, 101, 102}
sealed_cases = [{
    "body": sealed_body,
    "sealed_ids": sorted(sealed_ids),
    "expected": [[pid, strip(rows)] for pid, rows in prices.sealed_rows_by_product(
        prices.results_of(sealed_body.encode()), DAY, sealed_ids).items()],
}]

# -- is_sealed / category_of ----------------------------------------------------------------
products = [
    ({"productId": 1, "extendedData": [{"name": "Number", "value": "1/102"}]}, 3),
    ({"productId": 2, "extendedData": [{"name": "number", "value": "x"}]}, 3),
    ({"productId": 3, "extendedData": [{"name": "Rarity", "value": "Rare"}]}, 3),
    ({"productId": 4, "extendedData": []}, 3),
    ({"productId": 5}, 3),
    ({"productId": 6, "extendedData": None}, 3),
    ({"productId": 7, "extendedData": [{"name": "Number", "value": "1"}]}, 85),
    ({"productId": 8, "extendedData": [{"name": "CardType", "value": "Pokemon"}]}, 85),
    ({"productId": 9, "extendedData": [{"name": "CardType", "value": "Pokemon"}]}, 3),
    ({"productId": 10, "extendedData": ["junk", {"name": "Number"}]}, 3),
]
is_sealed = [{"product": p, "category": c, "expected": prices.is_sealed(p, c)} for p, c in products]

urls = [
    "https://tcgcsv.com/tcgplayer/3/1234/products",
    "https://tcgcsv.com/tcgplayer/85/17/products/",
    "https://tcgcsv.com/tcgplayer/3/1234/products?x=1",
    "https://tcgcsv.com/tcgplayer/3/1234/prices",
    "products",
    "a/b/products",
    "https://tcgcsv.com/tcgplayer/three/1234/products",
]
category = []
for u in urls:
    try:
        category.append({"url": u, "expected": prices.category_of(u, "k")})
    except prices.BuildError:
        category.append({"url": u, "expected": None})

# -- read_date over runs: within-run first wins, across-run later wins, order --------------
def run(run_id, complete, parts):
    return {"run_id": run_id, "complete": complete,
            "parts": [{"key": f"raw/{run_id}/part-{i:04d}", "url": u, "body": b}
                      for i, (u, b) in enumerate(parts)]}

runs = [
    run("01A", True, [("u/3/1/prices", env([
        {"productId": 1, "subTypeName": "Normal", "marketPrice": 1.0, "lowPrice": 0.5},
        {"productId": 2, "marketPrice": 2.0},
        {"productId": 100, "subTypeName": "Normal", "marketPrice": 50.0},
    ]))]),
    run("01B", False, [
        ("u/3/1/prices", env([
            {"productId": 2, "marketPrice": 2.5},
            {"productId": 2, "marketPrice": 99.0},
            {"productId": 3, "marketPrice": 3.0},
            {"productId": 100, "subTypeName": "Normal", "marketPrice": 60.0, "lowPrice": 55.0},
            {"productId": 100, "subTypeName": "Foil", "marketPrice": 70.0},
        ])),
        ("u/3/2/prices", env([{"productId": 4, "marketPrice": 4.0, "midPrice": 4.5}])),
    ]),
]


class FakeZone:
    def __init__(self, runs):
        self.map = {p["key"]: p["body"].encode() for r in runs for p in r["parts"]}
        self.runs_ = [raw.Run(r["run_id"], "raw/" + r["run_id"] + "/", r["complete"], None,
                              tuple(raw.Part(p["key"], p["url"], 200, 0, "") for p in r["parts"]),
                              ()) for r in runs]

    def runs(self, *_):
        return self.runs_

    def payload(self, part):
        return self.map[part.key]


def read(runs_in, allow_incomplete):
    zone = FakeZone(runs_in)
    rows, sealed, prov = prices.read_date(zone, "2026-08-11", DAY, sealed_ids={100},
                                          allow_incomplete=allow_incomplete)
    return {"runs": runs_in, "allow_incomplete": allow_incomplete, "sealed_ids": [100],
            "rows": strip(rows), "sealed": strip(sealed), "provenance": prov}


import contextlib, io
with contextlib.redirect_stdout(io.StringIO()):
    incomplete_pair = [dict(runs[0], complete=False), runs[1]]
    read_dates = [read(runs, True), read([runs[0]], False), read(runs[1:], True),
                  read(incomplete_pair, True)]

    # select_runs outcomes
    sel = []
    for rs, allow in ((runs, False), (runs[1:], False), (runs[1:], True), ([], True), ([], False)):
        zone = FakeZone(rs)
        try:
            chosen = [r.run_id for r in raw.select_runs(zone.runs(), allow_incomplete=allow)]
        except raw.RawError:
            chosen = None
        sel.append({"runs": [{"run_id": r["run_id"], "complete": r["complete"]} for r in rs],
                    "allow_incomplete": allow, "chosen": chosen})

json.dump({"price_cases": price_cases, "sealed_cases": sealed_cases, "is_sealed": is_sealed,
           "category_of": category, "read_date": read_dates, "select_runs": sel},
          sys.stdout, indent=1, sort_keys=True)
print()
