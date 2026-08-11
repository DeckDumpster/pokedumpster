"""PokeDumpster offline lakehouse jobs.

Iceberg tables in S3, versioned by a Nessie catalog, read and written with
PyIceberg. Offline only — nothing here is on the serving path, and per the
standing decision in the design, **no tenant data ever enters the lake**.
"""
