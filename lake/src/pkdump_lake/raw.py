"""Reading the raw landing zone — the other half of ``crates/pkdump-lake``.

That crate writes ``raw/`` and never reads it; this module reads it and never
writes. The two halves share nothing but the key layout and the manifest
shape, which is exactly why both are spelled out here rather than inferred:
a change on the writing side has to break something on this side loudly.

::

    raw/source=<source>/dataset=<dataset>/ingest_date=YYYY-MM-DD/run=<ULID>/
        part-NNNN.<ext>.zst
        _manifest.json

**Nothing here parses a payload.** It lists runs, reads manifests, and hands
back the bytes a part held — verified against the SHA-256 the manifest
recorded, because verifying the landing zone offline is the entire reason
that field exists. What those bytes *mean* is the caller's business
(``prices.py`` is the first caller).

Which run to read is a real decision, so it is a named one — see
:func:`select_runs`.
"""

from __future__ import annotations

import hashlib
import json
import os
import posixpath
from dataclasses import dataclass

import pyarrow as pa
from pyarrow import fs as pafs

#: The root of the landing zone inside the bucket. Mirrors
#: ``crates/pkdump-lake/src/keys.rs``; a change here orphans every object
#: already landed.
RAW_ROOT = "raw"

#: The manifest's file name, uncompressed and sitting beside its parts.
MANIFEST_NAME = "_manifest.json"

# The destination, named exactly as `crates/pkdump-lake/src/config.rs` names
# it. One `~/.config/pkdump/lake.env` configures both the writer and this
# reader, so the two must not invent separate spellings of the same fact.
BUCKET_ENV = "PKDUMP_LAKE_S3_BUCKET"
REGION_ENV = "PKDUMP_LAKE_S3_REGION"
PREFIX_ENV = "PKDUMP_LAKE_S3_PREFIX"
ENDPOINT_ENV = "PKDUMP_LAKE_S3_ENDPOINT"
DIR_ENV = "PKDUMP_LAKE_DIR"

# Test-substrate credentials, the same names tests/lake/ already passes to the
# catalog. Real credentials come from the AWS SDK chain — a profile whose role
# the SDK assumes and refreshes — never from long-lived keys in a job's
# environment.
#: The role to assume for lake access. PyArrow uses the AWS **C++** SDK, whose
#: credential chain is env vars -> config files -> EC2 IMDS and which does NOT
#: follow a profile's `source_profile` role-assumption chain (botocore does, which
#: is why PyIceberg's writes worked while our reads did not). So the role has to be
#: named explicitly. PyArrow then assumes it and refreshes the temporary
#: credentials every `load_frequency` seconds, so a long job cannot outlive its
#: STS session. Base credentials still come from the C++ chain, which means
#: AWS_PROFILE must point at a profile that holds real keys (`bootstrap`), not at
#: one that only carries `role_arn`.
ROLE_ARN_ENV = "PKDUMP_LAKE_S3_ROLE_ARN"
#: The profile holding the *base* keys pyarrow assumes the role with. It differs
#: from AWS_PROFILE on purpose: Nessie overrides `py-io-impl` to FsspecFileIO in
#: its REST config, so the table's writes go through s3fs/botocore, which follows
#: a profile's `source_profile` chain and therefore needs AWS_PROFILE pointed at
#: the ROLE profile. PyArrow's C++ SDK does not follow that chain and needs a
#: profile with real keys. One env var cannot be both, so this one is applied only
#: while the S3FileSystem is constructed.
BASE_PROFILE_ENV = "PKDUMP_LAKE_S3_BASE_PROFILE"
ROLE_SESSION_ENV = "PKDUMP_LAKE_S3_ROLE_SESSION_NAME"
ACCESS_KEY_ENV = "PKDUMP_LAKE_S3_ACCESS_KEY_ID"
SECRET_KEY_ENV = "PKDUMP_LAKE_S3_SECRET_ACCESS_KEY"


class RawError(RuntimeError):
    """The landing zone is missing, short, or does not match its manifest."""


@dataclass(frozen=True)
class Part:
    """One landed payload, as the manifest describes it."""

    key: str
    url: str
    status: int
    bytes: int
    sha256: str


@dataclass(frozen=True)
class Run:
    """One ``run=<ULID>`` prefix and the manifest that describes it."""

    run_id: str
    prefix: str
    complete: bool
    error: str | None
    parts: tuple[Part, ...]
    failures: tuple[dict, ...]

    def describe(self) -> str:
        state = "complete" if self.complete else f"INCOMPLETE ({self.error or 'no reason given'})"
        return f"run={self.run_id} {len(self.parts)} part(s), {state}"


def run_prefix(source: str, dataset: str, ingest_date: str, run_id: str) -> str:
    """The prefix every object of one run shares. ``keys.rs::run_prefix``."""
    return (
        f"{RAW_ROOT}/source={source}/dataset={dataset}/"
        f"ingest_date={ingest_date}/run={run_id}/"
    )


def dataset_prefix(source: str, dataset: str, ingest_date: str) -> str:
    """The prefix holding every run of one ``(source, dataset, date)``."""
    return f"{RAW_ROOT}/source={source}/dataset={dataset}/ingest_date={ingest_date}/"


class RawZone:
    """A landing zone, opened for reading.

    ``root`` is the path every key hangs off: ``bucket/prefix`` for S3, an
    absolute directory for the local backend. The manifest records keys
    *without* the configured prefix — the writer's store adds it at PUT time —
    so this is the one place the two are joined.
    """

    def __init__(self, filesystem: pafs.FileSystem, root: str, description: str) -> None:
        self._fs = filesystem
        self._root = root.rstrip("/")
        self.description = description

    # -- discovery ----------------------------------------------------------

    def ingest_dates(self, source: str, dataset: str) -> list[str]:
        """Every ``ingest_date`` this dataset has landed, oldest first."""
        base = f"{RAW_ROOT}/source={source}/dataset={dataset}/"
        return sorted(name.removeprefix("ingest_date=") for name in self._child_dirs(base))

    def runs(self, source: str, dataset: str, ingest_date: str) -> list[Run]:
        """Every run landed for a date, oldest first (ULIDs sort that way).

        A run whose manifest is missing is still returned, with no parts and
        ``complete=False``: the process died before it could say what it had,
        and pretending the prefix is not there would be the same lie the
        manifest exists to prevent.
        """
        base = dataset_prefix(source, dataset, ingest_date)
        out = []
        for name in sorted(self._child_dirs(base)):
            run_id = name.removeprefix("run=")
            out.append(self._read_run(base + name + "/", run_id))
        return out

    def _read_run(self, prefix: str, run_id: str) -> Run:
        try:
            raw = self._read(prefix + MANIFEST_NAME)
        except FileNotFoundError:
            return Run(run_id, prefix, False, "no _manifest.json — the run never finalized", (), ())
        manifest = json.loads(raw)
        parts = tuple(
            Part(p["key"], p["url"], int(p["status"]), int(p["bytes"]), p["sha256"])
            for p in manifest.get("parts", [])
        )
        return Run(
            run_id=run_id,
            prefix=prefix,
            complete=bool(manifest.get("complete")),
            error=manifest.get("error"),
            parts=parts,
            failures=tuple(manifest.get("failures", [])),
        )

    # -- reading ------------------------------------------------------------

    def payload(self, part: Part) -> bytes:
        """The bytes a part held, decompressed and checked against the manifest.

        Both the length and the digest are the *uncompressed* ones, which is
        what the writer recorded. A mismatch is fatal: a lake that hands back
        something other than what it says it stored is worse than one that is
        simply empty.
        """
        body = self._read_zst(part.key)
        if len(body) != part.bytes:
            raise RawError(
                f"{part.key}: manifest says {part.bytes} bytes, the object holds {len(body)}"
            )
        digest = hashlib.sha256(body).hexdigest()
        if digest != part.sha256:
            raise RawError(
                f"{part.key}: manifest says sha256 {part.sha256}, the object hashes to {digest}"
            )
        return body

    # -- filesystem ---------------------------------------------------------

    def _path(self, key: str) -> str:
        return posixpath.join(self._root, key)

    def _child_dirs(self, key_prefix: str) -> list[str]:
        selector = pafs.FileSelector(self._path(key_prefix), recursive=False, allow_not_found=True)
        return [
            posixpath.basename(info.path.rstrip("/"))
            for info in self._fs.get_file_info(selector)
            if info.type == pafs.FileType.Directory
        ]

    # `compression=None` on both, deliberately. PyArrow's default is `"detect"`,
    # which reads the codec off the file *extension* — so `part-0000.json.zst`
    # would be decompressed once by the stream and once again here. What a key
    # is named must not decide how its bytes are handled.
    def _read(self, key: str) -> bytes:
        with self._fs.open_input_stream(self._path(key), compression=None) as handle:
            return handle.read()

    def _read_zst(self, key: str) -> bytes:
        with self._fs.open_input_stream(self._path(key), compression=None) as handle:
            with pa.CompressedInputStream(handle, "zstd") as body:
                return body.read()


def open_raw() -> RawZone:
    """Open the landing zone this host is configured for.

    ``PKDUMP_LAKE_DIR`` wins when set — the directory backend the writer uses
    for hermetic gates, so both sides of a test agree without a bucket.
    Otherwise the bucket, which has no default and never will: an
    unconfigured lake refuses and names the file to write.
    """
    directory = os.environ.get(DIR_ENV, "").strip()
    if directory:
        return RawZone(pafs.LocalFileSystem(), os.path.abspath(directory), f"dir {directory}")

    bucket = os.environ.get(BUCKET_ENV, "").strip()
    region = os.environ.get(REGION_ENV, "").strip()
    if not bucket or not region:
        raise RawError(
            f"the lake is not configured: set {BUCKET_ENV} and {REGION_ENV} "
            f"(~/.config/pkdump/lake.env), or {DIR_ENV} for a local landing zone.\n"
            "There is no default bucket — the lake is deliberately a separate bucket from "
            "the Litestream backup bucket, and guessing one is how a lifecycle rule written "
            "for the lake reaches the backups."
        )

    options: dict[str, object] = {"region": region}
    endpoint = os.environ.get(ENDPOINT_ENV, "").strip()
    if endpoint:
        # PyArrow wants the host and the scheme separately — an
        # `endpoint_override` carrying "http://" would be read as a hostname
        # and the connection would go out over TLS to a plaintext MinIO.
        scheme, _, host = endpoint.rpartition("://")
        options["endpoint_override"] = host
        options["scheme"] = scheme or "https"
    access_key = os.environ.get(ACCESS_KEY_ENV, "").strip()
    secret_key = os.environ.get(SECRET_KEY_ENV, "").strip()
    role_arn = os.environ.get(ROLE_ARN_ENV, "").strip()
    if access_key and secret_key:
        # Static keys win when both are given — that is how a MinIO test instance
        # is pointed at its own credentials.
        options["access_key"] = access_key
        options["secret_key"] = secret_key
    elif role_arn:
        options["role_arn"] = role_arn
        options["session_name"] = (
            os.environ.get(ROLE_SESSION_ENV, "").strip() or "pkdump-lake"
        )

    prefix = os.environ.get(PREFIX_ENV, "").strip().strip("/")
    root = f"{bucket}/{prefix}" if prefix else bucket

    base_profile = os.environ.get(BASE_PROFILE_ENV, "").strip()
    if role_arn and base_profile:
        # Scoped to the constructor: the C++ SDK reads AWS_PROFILE when the
        # filesystem is built, and botocore reads it later for its own clients.
        previous = os.environ.get("AWS_PROFILE")
        os.environ["AWS_PROFILE"] = base_profile
        try:
            filesystem = pafs.S3FileSystem(**options)
        finally:
            if previous is None:
                os.environ.pop("AWS_PROFILE", None)
            else:
                os.environ["AWS_PROFILE"] = previous
    else:
        filesystem = pafs.S3FileSystem(**options)

    return RawZone(filesystem, root, f"s3://{root}")


def select_runs(runs: list[Run], *, allow_incomplete: bool = False) -> list[Run]:
    """Which of a date's runs a rebuild should read.

    The landing zone can hold several runs for one date, because a retry after
    a partial failure lands *beside* the first attempt rather than on it. That
    is the point of ``run=<ULID>`` — and it makes "rebuild this date" a
    question with more than one answer.

    The answer taken here:

    * **The newest complete run wins, alone.** ``complete: true`` means every
      fetch that prefix was going to get arrived, so it is by definition all
      of that date's data. Reading an earlier run as well could only add
      staler copies of the same rows.
    * **With no complete run, refuse** — unless the caller opts in, in which
      case every run on the date is read and the result is explicitly not a
      whole day. There is no way to tell from a manifest whether two
      incomplete runs *together* cover the day: the writer never learns how
      many parts a dataset was owed. Quietly stitching them would produce a
      table that looks like a day and is not one, which is the exact failure
      the manifest's ``complete`` flag exists to prevent.
    """
    complete = [run for run in runs if run.complete]
    if complete:
        return [complete[-1]]
    if not runs:
        raise RawError("no runs landed for that date")
    if not allow_incomplete:
        detail = "; ".join(run.describe() for run in runs)
        raise RawError(
            f"no complete run for that date — {detail}.\n"
            "An incomplete run's parts are real bytes, but nothing in the landing zone can "
            "say whether they add up to the whole day. Re-run the fetch, or pass "
            "--allow-incomplete to build from what is there and have the table say so."
        )
    return list(runs)
