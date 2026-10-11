"""Hermetic tests for pkdump_lake.nessie_backup (sp-87nj2v). Stdlib only: a
directory stands in for the bucket and a fake object for the catalog."""
import datetime as dt
import io
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import SimpleNamespace as NS

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lake" / "src"))
from pkdump_lake import nessie_backup as nb  # noqa: E402

NOW = dt.datetime(2026, 10, 12, 8, 0, tzinfo=dt.timezone.utc)
TABLES = [
    {"identifier": "catalog.prices", "metadata_location": "s3://b/m1.json", "snapshot_id": 1, "rows": 10},
    {"identifier": "deckdumpster.x", "metadata_location": "s3://b/m2.json", "snapshot_id": 2, "rows": 3},
]


def make_zip(entries=None):
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w") as z:
        for n, d in (entries or {"export-metadata": b"x", "commits": b"y" * 100}).items():
            z.writestr(n, d)
    return b.getvalue()


class FakeTable:
    def __init__(self, loc, rows, snap=True, boom=False):
        self.metadata_location, self._rows, self._snap, self._boom = loc, rows, snap, boom

    def current_snapshot(self):
        return object() if self._snap else None

    def schema(self):
        return NS(fields=[NS(name="c")])

    def scan(self, selected_fields):
        if self._boom:
            raise OSError("metadata file missing")
        return NS(to_arrow=lambda: NS(num_rows=self._rows))


class FakeCatalog:
    def __init__(self, tables):
        self.t = tables

    def load_table(self, ident):
        return self.t[".".join(ident)]


def good_catalog():
    return FakeCatalog(
        {"catalog.prices": FakeTable("s3://b/m1.json", 10), "deckdumpster.x": FakeTable("s3://b/m2.json", 3)}
    )


class Backup(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.TemporaryDirectory()
        self.store = nb.LocalStore(self.d.name)

    def tearDown(self):
        self.d.cleanup()

    def put(self, bid="20261012T070000Z", z=None, **kw):
        return nb.put_backup(make_zip() if z is None else z, self.store, TABLES, backup_id=bid, nessie_version="0.104.3", **kw)

    def test_a_good_backup_checks_ok(self):
        self.put()
        code, why = nb.check(self.store, NOW, 26)
        self.assertEqual(code, nb.EXIT_OK, why)

    def test_no_backup_is_stale(self):
        self.assertEqual(nb.check(self.store, NOW, 26)[0], nb.EXIT_STALE)

    def test_old_backup_is_stale(self):  # positive control for the age alarm
        self.put("20261010T070000Z")
        self.assertEqual(nb.check(self.store, NOW, 26)[0], nb.EXIT_STALE)

    def test_a_corrupted_export_is_corrupt_even_when_fresh(self):
        m = self.put()
        k = m["export"]["key"]
        data = bytearray(self.store.get(k))
        data[len(data) // 2] ^= 0xFF
        self.store.put(k, bytes(data))
        self.assertEqual(nb.check(self.store, NOW, 26)[0], nb.EXIT_CORRUPT)

    def test_a_truncated_export_is_corrupt(self):
        m = self.put()
        self.store.put(m["export"]["key"], self.store.get(m["export"]["key"])[:50])
        self.assertEqual(nb.check(self.store, NOW, 26)[0], nb.EXIT_CORRUPT)

    def test_a_missing_export_is_corrupt_not_a_crash(self):
        m = self.put()
        self.store.delete(m["export"]["key"])
        self.assertEqual(nb.check(self.store, NOW, 26)[0], nb.EXIT_CORRUPT)

    def test_garbage_is_refused_before_upload(self):
        with self.assertRaises(nb.BackupError):
            self.put(z=b"not a zip")
        with self.assertRaises(nb.BackupError):
            self.put(z=b"")
        self.assertEqual(nb.backup_ids(self.store), [])

    def test_manifest_is_last_so_an_interrupted_put_is_invisible(self):
        self.put("20261011T070000Z")
        self.store.put("nessie/20261012T070000Z/export.zip", make_zip())  # no manifest
        self.assertEqual(nb.backup_ids(self.store), ["20261011T070000Z"])

    def test_retention_keeps_newest_n_and_ignores_strangers(self):
        for d in range(1, 6):
            self.put(f"2026100{d}T070000Z", keep=3)
        self.store.put("nessie/README", b"mine")
        self.assertEqual(nb.backup_ids(self.store), ["20261003T070000Z", "20261004T070000Z", "20261005T070000Z"])
        self.assertEqual(self.store.get("nessie/README"), b"mine")

    def test_an_old_manifest_does_not_hide_a_newer_corrupt_one(self):
        self.put("20261011T070000Z")
        m = self.put("20261012T070000Z")
        self.store.put(m["export"]["key"], b"x")
        self.assertEqual(nb.check(self.store, NOW, 26)[0], nb.EXIT_CORRUPT)


class Drill(unittest.TestCase):
    M = {"tables": TABLES}

    def test_all_tables_read_back(self):
        self.assertEqual(nb.drill(good_catalog(), self.M, ["catalog", "deckdumpster"]), [])

    def test_unreadable_table_fails(self):
        c = good_catalog()
        c.t["catalog.prices"] = FakeTable("s3://b/m1.json", 10, boom=True)
        f = nb.drill(c, self.M, [])
        self.assertEqual(len(f), 1)
        self.assertIn("catalog.prices", f[0])

    def test_missing_table_fails(self):
        c = good_catalog()
        del c.t["deckdumpster.x"]
        self.assertEqual(len(nb.drill(c, self.M, [])), 1)

    def test_row_count_mismatch_fails(self):
        c = good_catalog()
        c.t["catalog.prices"] = FakeTable("s3://b/m1.json", 9)
        self.assertIn("read 9 rows", nb.drill(c, self.M, [])[0])

    def test_pointer_mismatch_fails(self):
        c = good_catalog()
        c.t["catalog.prices"] = FakeTable("s3://b/other.json", 10)
        self.assertEqual(len(nb.drill(c, self.M, [])), 1)

    def test_vacuous_drill_fails(self):
        self.assertTrue(nb.drill(good_catalog(), {"tables": []}, []))

    def test_required_namespace_absent_fails(self):
        self.assertTrue(nb.drill(good_catalog(), {"tables": TABLES[:1]}, ["deckdumpster"]))


class Fallback(unittest.TestCase):
    U = "0123456789abcdef0123456789abcdef"

    def test_newest_metadata_wins(self):
        keys = [
            f"lake/catalog/prices_{self.U}/metadata/00009-aaaa-bbbb.metadata.json",
            f"lake/catalog/prices_{self.U}/metadata/00010-cccc-dddd.metadata.json",
            f"lake/catalog/prices_{self.U}/metadata/snap-1.avro",
            f"lake/catalog/prices_{self.U}/data/x.parquet",
        ]
        f = nb.discover_tables(keys)
        self.assertEqual(f, {("catalog", "prices"): keys[1]})

    def test_reregister(self):
        calls = []
        cat = NS(create_namespace=lambda n: None, register_table=lambda i, loc: calls.append((i, loc)))
        nb.reregister(cat, {("catalog", "prices"): "k.json"}, "s3://b/lake/")
        self.assertEqual(calls, [(("catalog", "prices"), "s3://b/lake/k.json")])


if __name__ == "__main__":
    unittest.main()
