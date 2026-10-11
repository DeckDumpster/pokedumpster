"""Nessie's version store, made durable (sp-87nj2v).

Nessie's version store is the only thing that points at the Iceberg metadata
files. Lose it and every table in every lake that shares the instance is
stranded: Parquet and metadata intact in S3, no catalog entry. This module is
the half of the answer that is not a shell script:

* ``put``        verify a ``nessie-server-admin export`` and ship it off-box
                 with a manifest that inventories every table;
* ``check``      the age alarm. Fails when the newest backup is older than the
                 window **or cannot be verified** — it downloads the export and
                 re-hashes it, because a check that only lists the bucket is
                 watching telemetry, not testing the backup;
* ``drill``      against a SCRATCH Nessie restored from that export: every
                 table in the manifest must load, point at the same metadata
                 and return the same row count. Non-zero if any does not;
* ``reregister`` the documented fallback when no backup is usable: re-register
                 every table from its newest ``metadata.json`` in S3.

Layout, in the backup store (never the lake bucket's ``raw/``, ``lake/`` or
``tenant/`` prefixes)::

    nessie/<backup-id>/export.zip
    nessie/<backup-id>/manifest.json     written LAST: a backup with no
                                         manifest is incomplete and ignored

``<backup-id>`` is a UTC timestamp, ``20261011T071500Z``. Nothing here reads the
clock: the scheduler names the instant (``--now`` / ``--backup-id``), the same
rule ``--ingest-date`` and ``--as-of`` follow elsewhere in this package.

Exit statuses (``check`` and ``drill``): 0 good, 3 stale or absent, 4 corrupt,
5 a table could not be read after restore, 1 the question could not be asked.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import io
import json
import os
import re
import sys
import zipfile
from pathlib import Path
from typing import Any, Protocol

PREFIX = "nessie"
EXPORT = "export.zip"
MANIFEST = "manifest.json"
MANIFEST_FORMAT = 1

EXIT_OK = 0
EXIT_STALE = 3
EXIT_CORRUPT = 4
EXIT_UNREADABLE = 5

#: ~26h: one nightly backup plus two hours of slack for a long chain.
DEFAULT_MAX_AGE_HOURS = 26
#: Daily backups kept. Two weeks.
DEFAULT_KEEP = 14

_ID_RE = re.compile(r"^\d{8}T\d{6}Z$")


class BackupError(Exception):
    """A backup that is not what it claims to be."""


# --- storage ---------------------------------------------------------------


class Store(Protocol):
    def put(self, key: str, data: bytes) -> None: ...
    def get(self, key: str) -> bytes: ...
    def keys(self, prefix: str) -> list[str]: ...
    def delete(self, key: str) -> None: ...


class LocalStore:
    """A directory. The hermetic stand-in for a bucket."""

    def __init__(self, root: str | Path):
        self.root = Path(root)

    def put(self, key: str, data: bytes) -> None:
        p = self.root / key
        p.parent.mkdir(parents=True, exist_ok=True)
        tmp = p.with_name(p.name + ".part")
        tmp.write_bytes(data)
        tmp.replace(p)

    def get(self, key: str) -> bytes:
        return (self.root / key).read_bytes()

    def keys(self, prefix: str) -> list[str]:
        base = self.root / prefix
        if not base.exists():
            return []
        return sorted(
            str(p.relative_to(self.root)) for p in base.rglob("*") if p.is_file()
        )

    def delete(self, key: str) -> None:
        (self.root / key).unlink()


class S3Store:
    """``s3://bucket/prefix`` through fsspec/s3fs (already in the lake image)."""

    def __init__(self, url: str):
        import fsspec

        m = re.match(r"^s3://([^/]+)/?(.*)$", url)
        if not m:
            raise BackupError(f"not an s3:// URL: {url!r}")
        self.bucket, self.base = m.group(1), m.group(2).strip("/")
        kw: dict[str, Any] = {}
        endpoint = os.environ.get("PKDUMP_NESSIE_BACKUP_S3_ENDPOINT")
        if endpoint:
            kw["client_kwargs"] = {"endpoint_url": endpoint}
        self.fs = fsspec.filesystem("s3", **kw)

    def _path(self, key: str) -> str:
        return "/".join(p for p in (self.bucket, self.base, key) if p)

    def put(self, key: str, data: bytes) -> None:
        self.fs.pipe_file(self._path(key), data)

    def get(self, key: str) -> bytes:
        return self.fs.cat_file(self._path(key))

    def keys(self, prefix: str) -> list[str]:
        root = self._path("")
        out = []
        for p in self.fs.find(self._path(prefix)):
            out.append(p[len(root) :].lstrip("/") if root else p)
        return sorted(out)

    def delete(self, key: str) -> None:
        self.fs.rm_file(self._path(key))


def open_store(url: str) -> Store:
    if url.startswith("file://"):
        return LocalStore(url[len("file://") :])
    if url.startswith("s3://"):
        return S3Store(url)
    raise BackupError(f"unsupported store URL {url!r} (want s3://… or file://…)")


# --- the artifact ----------------------------------------------------------


def verify_export(data: bytes) -> int:
    """The export is a readable, non-empty zip. Returns its entry count."""
    if not data:
        raise BackupError("export is empty")
    if not zipfile.is_zipfile(io.BytesIO(data)):
        raise BackupError("export is not a zip archive")
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        bad = z.testzip()
        if bad is not None:
            raise BackupError(f"export entry {bad!r} fails its CRC")
        names = z.namelist()
    if not names:
        raise BackupError("export archive has no entries")
    return len(names)


def inventory(catalog: Any) -> list[dict[str, Any]]:
    """Every table the live catalog holds: where its metadata is, and how big.

    Row counts come from the snapshot summary — metadata only, so the backup of
    a large lake does not scan it. A table with a snapshot and no count is an
    error rather than a null: the drill would have nothing to hold it to.
    """
    out: list[dict[str, Any]] = []
    for ns in catalog.list_namespaces():
        for ident in catalog.list_tables(ns):
            tbl = catalog.load_table(ident)
            snap = tbl.current_snapshot()
            rows = 0
            snap_id = None
            if snap is not None:
                snap_id = snap.snapshot_id
                total = (snap.summary or {}).get("total-records")
                if total is None:
                    raise BackupError(
                        f"{'.'.join(ident)}: snapshot {snap_id} carries no total-records"
                    )
                rows = int(total)
            out.append(
                {
                    "identifier": ".".join(ident),
                    "metadata_location": tbl.metadata_location,
                    "snapshot_id": snap_id,
                    "rows": rows,
                }
            )
    return sorted(out, key=lambda t: t["identifier"])


def _fmt(t: dt.datetime) -> str:
    return t.astimezone(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def _parse_id(backup_id: str) -> dt.datetime:
    if not _ID_RE.match(backup_id):
        raise BackupError(f"bad backup id {backup_id!r}")
    return dt.datetime.strptime(backup_id, "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc)


def put_backup(
    export: bytes,
    store: Store,
    tables: list[dict[str, Any]],
    *,
    backup_id: str,
    nessie_version: str,
    keep: int = DEFAULT_KEEP,
) -> dict[str, Any]:
    """Verify, upload, and only then publish the manifest; then prune."""
    if keep < 1:
        raise BackupError("--keep must be at least 1")
    _parse_id(backup_id)
    verify_export(export)
    base = f"{PREFIX}/{backup_id}"
    manifest = {
        "format": MANIFEST_FORMAT,
        "id": backup_id,
        "nessie_version": nessie_version,
        "export": {
            "key": f"{base}/{EXPORT}",
            "bytes": len(export),
            "sha256": hashlib.sha256(export).hexdigest(),
        },
        "tables": tables,
    }
    store.put(f"{base}/{EXPORT}", export)
    # Read it back before blessing it: a PUT that returned is not an object that
    # is there.
    if hashlib.sha256(store.get(f"{base}/{EXPORT}")).hexdigest() != manifest["export"]["sha256"]:
        raise BackupError("uploaded export does not read back identically")
    store.put(f"{base}/{MANIFEST}", json.dumps(manifest, indent=1, sort_keys=True).encode())
    prune(store, keep)
    return manifest


def backup_ids(store: Store) -> list[str]:
    """Complete backups (those with a manifest), oldest first."""
    ids = set()
    for k in store.keys(PREFIX):
        parts = k.split("/")
        if len(parts) == 3 and parts[2] == MANIFEST and _ID_RE.match(parts[1]):
            ids.add(parts[1])
    return sorted(ids)


def prune(store: Store, keep: int) -> list[str]:
    """Delete all but the newest ``keep`` complete backups, and any incomplete
    one older than the newest complete backup. Returns the ids removed."""
    ids = backup_ids(store)
    doomed = set(ids[:-keep]) if len(ids) > keep else set()
    newest = ids[-1] if ids else None
    for k in store.keys(PREFIX):
        parts = k.split("/")
        if len(parts) != 3 or not _ID_RE.match(parts[1]):
            continue  # never touch what we do not recognise
        incomplete = parts[1] not in ids and newest is not None and parts[1] < newest
        if parts[1] in doomed or incomplete:
            store.delete(k)
            doomed.add(parts[1])
    return sorted(doomed)


def load_manifest(store: Store, backup_id: str) -> dict[str, Any]:
    m = json.loads(store.get(f"{PREFIX}/{backup_id}/{MANIFEST}"))
    if m.get("format") != MANIFEST_FORMAT or m.get("id") != backup_id:
        raise BackupError(f"manifest of {backup_id} is not format {MANIFEST_FORMAT} for that id")
    return m


def verify_backup(store: Store, backup_id: str) -> tuple[dict[str, Any], bytes]:
    """Restore-grade verification short of running Nessie: fetch the export and
    hold it to its manifest."""
    m = load_manifest(store, backup_id)
    data = store.get(m["export"]["key"])
    if len(data) != m["export"]["bytes"]:
        raise BackupError(f"{backup_id}: export is {len(data)} bytes, manifest says {m['export']['bytes']}")
    if hashlib.sha256(data).hexdigest() != m["export"]["sha256"]:
        raise BackupError(f"{backup_id}: export sha256 does not match its manifest")
    verify_export(data)
    if not m["tables"]:
        raise BackupError(f"{backup_id}: manifest inventories no tables")
    return m, data


def check(store: Store, now: dt.datetime, max_age_hours: float) -> tuple[int, str]:
    """The alarm. Returns (exit status, one line saying why)."""
    ids = backup_ids(store)
    if not ids:
        return EXIT_STALE, "no Nessie backup exists"
    newest = ids[-1]
    try:
        verify_backup(store, newest)
    except (BackupError, KeyError, ValueError, OSError) as e:
        return EXIT_CORRUPT, f"newest backup {newest} cannot be verified: {e}"
    age = now - _parse_id(newest)
    hours = age.total_seconds() / 3600
    if hours > max_age_hours:
        return EXIT_STALE, f"newest backup {newest} is {hours:.1f}h old (limit {max_age_hours}h)"
    return EXIT_OK, f"newest backup {newest} is {hours:.1f}h old and verifies"


# --- the drill -------------------------------------------------------------


def drill(
    catalog: Any,
    manifest: dict[str, Any],
    require_namespaces: list[str],
) -> list[str]:
    """Hold a restored catalog to the manifest. Returns the failures (empty =
    every table read back)."""
    tables = manifest["tables"]
    failures: list[str] = []
    if not tables:
        return ["manifest inventories no tables: a drill over nothing proves nothing"]
    present = {t["identifier"].rsplit(".", 1)[0] for t in tables}
    for ns in require_namespaces:
        if ns not in present:
            failures.append(f"namespace {ns!r} has no table in the backup")
    for t in tables:
        ident = t["identifier"]
        try:
            tbl = catalog.load_table(tuple(ident.split(".")))
            if tbl.metadata_location != t["metadata_location"]:
                failures.append(
                    f"{ident}: restored catalog points at {tbl.metadata_location}, "
                    f"backup recorded {t['metadata_location']}"
                )
                continue
            if tbl.current_snapshot() is None:
                rows = 0
            else:
                col = tbl.schema().fields[0].name
                rows = tbl.scan(selected_fields=(col,)).to_arrow().num_rows
            if rows != t["rows"]:
                failures.append(f"{ident}: read {rows} rows, backup recorded {t['rows']}")
        except Exception as e:  # noqa: BLE001 — any failure to read IS the finding
            failures.append(f"{ident}: cannot be read: {type(e).__name__}: {e}")
    return failures


# --- the fallback ----------------------------------------------------------

_META_RE = re.compile(
    r"^(?P<dir>(?:.+/)?(?P<ns>[^/]+)/(?P<tbl>[^/]+)_[0-9a-f]{32})/metadata/"
    r"(?P<ver>\d+)-[0-9a-f-]+\.metadata\.json$"
)


def discover_tables(keys: list[str]) -> dict[tuple[str, str], str]:
    """(namespace, table) -> key of its newest metadata.json, from a listing of
    the warehouse. Layout: ``<ns>/<table>_<uuid32>/metadata/NNNNN-<uuid>.metadata.json``."""
    best: dict[tuple[str, str], tuple[int, str]] = {}
    for k in keys:
        m = _META_RE.match(k)
        if not m:
            continue
        ident = (m["ns"], m["tbl"])
        v = int(m["ver"])
        if ident not in best or v > best[ident][0]:
            best[ident] = (v, k)
    return {i: k for i, (_, k) in best.items()}


def reregister(catalog: Any, found: dict[tuple[str, str], str], location_root: str) -> list[str]:
    done = []
    for (ns, name), key in sorted(found.items()):
        try:
            catalog.create_namespace((ns,))
        except Exception:  # noqa: BLE001 — already exists is the usual case
            pass
        catalog.register_table((ns, name), f"{location_root.rstrip('/')}/{key}")
        done.append(f"{ns}.{name}")
    return done


# --- CLI -------------------------------------------------------------------


def _store_arg(p: argparse.ArgumentParser) -> None:
    p.add_argument(
        "--store",
        default=os.environ.get("PKDUMP_NESSIE_BACKUP_URL"),
        help="s3://bucket/prefix (default $PKDUMP_NESSIE_BACKUP_URL; no fallback)",
    )


def _need_store(args: argparse.Namespace) -> Store:
    if not args.store:
        raise BackupError("PKDUMP_NESSIE_BACKUP_URL is not set and --store was not given")
    return open_store(args.store)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="pkdump-lake-nessie-backup")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("put", help="verify an export and ship it with a table inventory")
    _store_arg(p)
    p.add_argument("--export", required=True, type=Path)
    p.add_argument("--backup-id", required=True, help="UTC YYYYMMDDTHHMMSSZ, named by the scheduler")
    p.add_argument("--nessie-version", required=True)
    p.add_argument("--inventory", type=Path, required=True, help="inventory JSON written by `inventory`")
    p.add_argument("--keep", type=int, default=DEFAULT_KEEP)

    p = sub.add_parser("inventory", help="write the live catalog's table inventory as JSON")
    p.add_argument("--out", required=True, type=Path)

    p = sub.add_parser("check", help="age + verify alarm: 0 ok, 3 stale/absent, 4 corrupt")
    _store_arg(p)
    p.add_argument("--now", required=True, help="ISO-8601 UTC instant, named by the scheduler")
    p.add_argument("--max-age-hours", type=float, default=DEFAULT_MAX_AGE_HOURS)

    p = sub.add_parser("fetch", help="download+verify the newest backup for a restore")
    _store_arg(p)
    p.add_argument("--out-dir", required=True, type=Path)

    p = sub.add_parser("drill", help="hold a restored catalog to a manifest: 0 ok, 5 on any failure")
    p.add_argument("--manifest", required=True, type=Path)
    p.add_argument("--require-namespace", action="append", default=[])

    p = sub.add_parser("reregister", help="fallback: re-register tables from metadata.json in S3")
    p.add_argument("--warehouse", required=True, help="s3://bucket/prefix of the Iceberg warehouse")

    args = ap.parse_args(argv)
    try:
        if args.cmd == "inventory":
            from .catalog import catalog

            args.out.write_text(json.dumps(inventory(catalog()), indent=1))
            return 0
        if args.cmd == "put":
            m = put_backup(
                args.export.read_bytes(),
                _need_store(args),
                json.loads(args.inventory.read_text()),
                backup_id=args.backup_id,
                nessie_version=args.nessie_version,
                keep=args.keep,
            )
            print(f"nessie-backup: {m['id']} stored, {len(m['tables'])} table(s), {m['export']['bytes']} bytes")
            return 0
        if args.cmd == "check":
            now = dt.datetime.fromisoformat(args.now.replace("Z", "+00:00"))
            code, why = check(_need_store(args), now, args.max_age_hours)
            print(f"nessie-backup: {'OK' if code == 0 else 'FAILED'} — {why}", file=sys.stderr if code else sys.stdout)
            return code
        if args.cmd == "fetch":
            store = _need_store(args)
            ids = backup_ids(store)
            if not ids:
                print("nessie-backup: no backup exists", file=sys.stderr)
                return EXIT_STALE
            m, data = verify_backup(store, ids[-1])
            args.out_dir.mkdir(parents=True, exist_ok=True)
            (args.out_dir / EXPORT).write_bytes(data)
            (args.out_dir / MANIFEST).write_text(json.dumps(m, indent=1))
            print(f"nessie-backup: fetched {m['id']}")
            return 0
        if args.cmd == "drill":
            from .catalog import catalog

            m = json.loads(args.manifest.read_text())
            fails = drill(catalog(), m, args.require_namespace)
            for f in fails:
                print(f"drill: FAIL {f}", file=sys.stderr)
            if fails:
                return EXIT_UNREADABLE
            print(f"drill: OK — {len(m['tables'])} table(s) read back through the restored catalog")
            return 0
        if args.cmd == "reregister":
            from .catalog import catalog

            store = open_store(args.warehouse)
            found = discover_tables(store.keys(""))
            if not found:
                print("reregister: no metadata.json found under the warehouse", file=sys.stderr)
                return 1
            for t in reregister(catalog(), found, args.warehouse):
                print(f"reregistered {t}")
            return 0
    except BackupError as e:
        print(f"nessie-backup: {e}", file=sys.stderr)
        return EXIT_CORRUPT if args.cmd in ("put", "fetch") else 1
    return 1


if __name__ == "__main__":
    sys.exit(main())
