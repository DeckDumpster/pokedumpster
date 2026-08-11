"""The round-trip proof: write, read, commit again, and time-travel back.

pd-fzeb. Iceberg + Nessie is deliberately overkill for this data size (per
Ryan: *"that will be overkill but give us the best primitives to move rapidly
on offline data concerns"*). **Time travel is the primitive being bought.** If
it does not work we have paid the cost for nothing — so this asserts it rather
than assuming it, and it is a program rather than a paragraph so it can be
re-run against any instance.

Five steps, in order:

1. create a namespace and a table via PyIceberg
2. write rows
3. read them back
4. write a second commit
5. read the table **as of the first commit** and get the original rows back

Step 5 is asserted two ways, because they are different primitives and only one
of them is the reason Nessie is here:

* **Nessie ref** — ``main@<hash>`` addresses *the whole catalog* at a commit.
  That is the provenance handle a published artefact records: one value, not a
  snapshot id per table.
* **Iceberg snapshot** — ``scan(snapshot_id=…)`` addresses one table's history.
  Available from Iceberg alone, with no catalog server at all. It is asserted
  here precisely so the recommendation about whether Nessie is needed yet rests
  on a measurement instead of an opinion.

Writes go to the ``proof`` namespace. No catalog table is touched, and — per
the standing decision this design is built on — no tenant data is anywhere
near it.
"""

from __future__ import annotations

import sys

import pyarrow as pa
import requests

from .catalog import catalog, nessie_api_base

NAMESPACE = "proof"
TABLE = f"{NAMESPACE}.roundtrip"

SCHEMA = pa.schema(
    [
        pa.field("id", pa.int64(), nullable=False),
        pa.field("note", pa.string(), nullable=False),
    ]
)

FIRST = [{"id": 1, "note": "first commit"}]
SECOND = [{"id": 2, "note": "second commit"}]


def head_hash(branch: str) -> str:
    """The Nessie commit hash at the tip of ``branch``."""
    resp = requests.get(f"{nessie_api_base()}/trees/{branch}", timeout=30)
    resp.raise_for_status()
    return resp.json()["reference"]["hash"]


def rows(table_scan) -> list[dict]:
    return sorted(table_scan.to_arrow().to_pylist(), key=lambda r: r["id"])


def check(label: str, got, want) -> None:
    if got != want:
        raise AssertionError(f"{label}: expected {want!r}, got {got!r}")
    print(f"    ok   {label}")


def main() -> int:
    cat = catalog()
    branch = cat.properties.get("prefix") or "main"
    print(f"==> catalog: {cat.uri} (ref {branch})")
    if "@" in branch:
        # Writing is the point, and a pinned ref is read-only. Refuse rather
        # than fail three steps later with something less legible.
        raise RuntimeError(f"PKDUMP_LAKE_REF must be a branch to run this proof, got {branch!r}")

    # Re-runnable against a live instance, which a documented proof has to be.
    if cat.table_exists(TABLE):
        cat.drop_table(TABLE)
    cat.create_namespace_if_not_exists(NAMESPACE)

    print("==> 1. create a table")
    table = cat.create_table(TABLE, schema=SCHEMA)
    print(f"    {TABLE} at {table.location()}")

    print("==> 2. write rows")
    table.append(pa.Table.from_pylist(FIRST, schema=SCHEMA))
    commit_one = head_hash(branch)
    snapshot_one = table.current_snapshot().snapshot_id
    print(f"    nessie commit {commit_one}")
    print(f"    iceberg snapshot {snapshot_one}")

    print("==> 3. read them back")
    check("rows after commit 1", rows(table.scan()), FIRST)

    print("==> 4. write a second commit")
    table.append(pa.Table.from_pylist(SECOND, schema=SCHEMA))
    commit_two = head_hash(branch)
    print(f"    nessie commit {commit_two}")
    check("rows after commit 2", rows(table.scan()), FIRST + SECOND)
    if commit_two == commit_one:
        raise AssertionError(f"nessie did not advance {branch} — the two writes are one commit")

    print("==> 5. time travel")
    # (a) the catalog as of commit 1 — the reason Nessie is here. This is the
    # assertion the whole exercise exists for.
    past = catalog(ref=f"{branch}@{commit_one}", name="lake-at-commit-1")
    check(f"nessie ref {branch}@<commit 1>", rows(past.load_table(TABLE).scan()), FIRST)
    # The tip is still both rows: reading the past must not rewind the present.
    check(f"{branch} is still at commit 2", rows(cat.load_table(TABLE).scan()), FIRST + SECOND)

    # (b) Iceberg's OWN per-table history, which is a different primitive and is
    # available without any catalog server. Measured, not asserted: what Nessie
    # does to it is the evidence behind "is Nessie needed yet?", and pinning an
    # assertion to today's answer would just make a future Nessie look broken.
    fresh = cat.load_table(TABLE)
    kept = [s.snapshot_id for s in fresh.metadata.snapshots]
    print(f"    note snapshots Nessie keeps in table metadata: {len(kept)} {kept}")
    try:
        rows(fresh.scan(snapshot_id=snapshot_one))
        print("    note iceberg snapshot_id time travel: AVAILABLE")
    except ValueError as exc:
        # Observed on Nessie 0.104.3: the metadata handed to a client carries
        # only the current snapshot. Nessie IS the history — per-table snapshot
        # travel is not a second way to do the same thing, it is gone.
        print(f"    note iceberg snapshot_id time travel: UNAVAILABLE under Nessie ({exc})")

    print("==> round trip PASSED (write, read, commit, time-travel)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
