"""Connecting to the lake.

One place that turns environment into a PyIceberg catalog, so every lake job
(pd-1ojt's table build, the transform tier) opens the lake the same way and a
change of catalog is one edit rather than a grep.

Nessie speaks the Iceberg REST catalog spec at ``/iceberg/``, so the client is
PyIceberg's stock ``RestCatalog`` — there is no Nessie SDK in our jobs and no
JVM anywhere near them.

**The server owns storage configuration, not the client.** Nessie's
``/iceberg/v1/config`` vends the warehouse location, the S3 endpoint and (when
configured) short-lived credentials, and those win over anything a client
passes. That is deliberate: a job needs one URL and nothing else, and the
bucket cannot be redirected by a job's environment. ``PKDUMP_LAKE_S3_*`` below
exists only for the throwaway MinIO in ``tests/lake/`` — against a Nessie that
vends its own storage config it is ignored.
"""

from __future__ import annotations

import os

import requests
from pyiceberg.catalog.rest import RestCatalog

#: Nessie's Iceberg REST endpoint, e.g. ``http://nessie:19120/iceberg/``.
URI_ENV = "PKDUMP_LAKE_NESSIE_URI"

#: The Nessie ref to read and write. A branch name (``main``), a tag, or a
#: branch pinned to a commit (``main@<hash>``) — which is time travel, and the
#: primitive the whole catalog was bought for. Nessie advertises the shape as
#: ``{ref}|{warehouse}`` in its REST config; PyIceberg passes it as ``prefix``.
REF_ENV = "PKDUMP_LAKE_REF"

DEFAULT_REF = "main"


def catalog(ref: str | None = None, name: str = "lake") -> RestCatalog:
    """Open the lake at ``ref`` (default: ``$PKDUMP_LAKE_REF``, then ``main``).

    Pass ``main@<commit-hash>`` to read the catalog exactly as it stood at that
    commit — every table at once, which is the thing a per-table snapshot id
    cannot express.
    """
    uri = os.environ.get(URI_ENV)
    if not uri:
        raise RuntimeError(
            f"{URI_ENV} is not set — it must be Nessie's Iceberg REST endpoint, "
            "e.g. http://nessie:19120/iceberg/"
        )

    props: dict[str, str] = {
        "uri": uri,
        "prefix": ref or os.environ.get(REF_ENV) or DEFAULT_REF,
    }

    # Test-substrate only; see the module docstring. Real S3 credentials come
    # from the AWS SDK chain (AWS_PROFILE=pkdump assuming the role), never from
    # long-lived keys in a job's environment.
    for prop, env in (
        ("s3.endpoint", "PKDUMP_LAKE_S3_ENDPOINT"),
        ("s3.access-key-id", "PKDUMP_LAKE_S3_ACCESS_KEY_ID"),
        ("s3.secret-access-key", "PKDUMP_LAKE_S3_SECRET_ACCESS_KEY"),
        ("s3.region", "PKDUMP_LAKE_S3_REGION"),
    ):
        value = os.environ.get(env)
        if value:
            props[prop] = value
    if os.environ.get("PKDUMP_LAKE_S3_PATH_STYLE"):
        props["s3.path-style-access"] = "true"

    return RestCatalog(name, **props)


def nessie_api_base(uri: str | None = None) -> str:
    """Nessie's own v2 API base, derived from the Iceberg REST endpoint.

    Reading a commit hash is a Nessie concern, not an Iceberg one — the REST
    catalog spec has no notion of a catalog-wide commit. This is the one place
    that crosses over, so the derivation lives here rather than in each job.
    """
    uri = uri or os.environ.get(URI_ENV) or ""
    base = uri.rstrip("/")
    if not base.endswith("/iceberg"):
        raise RuntimeError(f"expected an Iceberg REST endpoint ending in /iceberg, got {uri!r}")
    return base[: -len("/iceberg")] + "/api/v2"


def head_hash(branch: str = DEFAULT_REF) -> str:
    """The Nessie commit hash at the tip of ``branch``.

    The provenance handle a job records: one value that addresses **the whole
    catalog** at a commit, which a per-table snapshot id cannot express. Lives
    here beside :func:`nessie_api_base` because it is the same crossing from
    Iceberg's REST surface into Nessie's own.
    """
    resp = requests.get(f"{nessie_api_base()}/trees/{branch}", timeout=30)
    resp.raise_for_status()
    return resp.json()["reference"]["hash"]
