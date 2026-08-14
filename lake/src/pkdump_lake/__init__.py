"""PokeDumpster offline lakehouse jobs.

Iceberg tables in S3, versioned by a Nessie catalog, read and written with
PyIceberg. Offline only — nothing here is on the serving path, and per the
standing decision in the design, **no tenant data ever enters the catalog**.

That decision is about the CATALOG ZONE — ``raw/`` and the Iceberg warehouse
beside it, which is everything in this package. The same bucket also holds a
**tenant zone** under ``tenant/`` (``crates/pkdump-lake/src/tenant.rs``):
holdings and valuations, always tenant-keyed, retained 90 days, reached with
credentials that reach nothing else. Nothing here writes to it, and nothing
there is Iceberg.

That last sentence is enforced, not merely stated: ``tests/lake/
tenant_isolation_test.sh`` asserts no Iceberg field is tenant-identifying and
that the write-path modules here import no ``sqlite3`` at all. ``value_snapshots
.py`` is the deliberate exception — it opens every tenant's database, and its
half of the rule is that it only ever READS the lake.
"""
