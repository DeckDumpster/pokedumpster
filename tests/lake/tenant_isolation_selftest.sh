#!/usr/bin/env bash
# The guard's own red proof (pd-7x83).
#
# tests/lake/tenant_isolation_test.sh asserts that tenant identity stays out of
# the catalog zone. This file asserts that IT WORKS — by breaking the property
# on purpose, one violation at a time, and requiring the guard to notice.
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
# The inbound-leg design says it in one line: "a guard never seen red is not
# known to work — that is three-for-three on this repo today." Three gates
# shipped green in a single day while asserting nothing, each for a different
# reason: a corpus that had moved, a subject that never ran, a check wired to a
# path nothing took. All three passed every run.
#
# The guard being extended here is more exposed to that than most, because it
# is a grep over file lists. Every one of its sections turns green if its
# corpus is empty, and the re-cut this proof accompanies (pd-7x83) ADDED four
# sections about a zone whose code was written weeks after the guard was — so
# "passes vacuously" is not a hypothetical here, it is the specific way these
# sections would be wrong.
#
# ── HOW ─────────────────────────────────────────────────────────────────────
# The guard resolves the tree it reads from its OWN location, so a copy of it
# in a copy of the tree reads that copy. No flag, no environment override, no
# argument — the guard stays un-parameterised, because a guard that can be
# pointed somewhere else is a guard that can be pointed somewhere harmless.
#
# Each case mutates ONE file in the copy, runs the guard, and requires both a
# non-zero exit and the SPECIFIC assertion to be the one that failed. The
# second half matters: a mutation that reddened some unrelated check would
# "prove" a section that is still asleep.
#
# The green cases are the other half of the claim, and the reason this file is
# not just "break it and see". The tenant zone is SUPPOSED to be tenant-keyed:
# a guard that fired on `database_id` in the tenant zone's own key layout, or
# on a tenant column in the holdings schema, would be a guard whose first
# encounter with real work is a false positive — and a false positive is how a
# rule like this gets an exemption list bolted onto it and stops meaning
# anything.
#
# Hermetic: copies source trees to a temp dir, mutates the copy, runs bash.
# No podman, no network, no build. Lint tier, beside the guard itself.
#
#   bash tests/lake/tenant_isolation_selftest.sh
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
GUARD_REL="tests/lake/tenant_isolation_test.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/pkdump-tenant-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
ok() {
	echo "  PASS  $1"
	pass=$((pass + 1))
}
bad() {
	echo "  FAIL  $1"
	[[ -n "${2:-}" ]] && printf '          %s\n' "$2"
	fail=$((fail + 1))
}
log() { printf '\n=== %s ===\n' "$*"; }

# ── the copy the guard will read ────────────────────────────────────────────
# Everything the guard's corpora resolve to, and nothing else. Kept as a
# pristine reference alongside the working copy so a case can restore exactly
# the file it touched without re-copying the tree.
PRISTINE="${WORK}/pristine"
TREE="${WORK}/tree"
build_tree() {
	rm -rf "$TREE"
	mkdir -p "$TREE/tests/lake" "$TREE/lake"
	cp -r "${REPO_DIR}/crates" "$TREE/crates"
	cp -r "${REPO_DIR}/lake/src" "$TREE/lake/src"
	cp "${REPO_DIR}/${GUARD_REL}" "$TREE/${GUARD_REL}"
}
build_tree
cp -r "$TREE" "$PRISTINE"

# Restore one file from the pristine copy.
restore() { # restore <repo-relative-path>
	rm -f "${TREE:?}/$1"
	cp "${PRISTINE}/$1" "${TREE}/$1"
}

guard() { bash "${TREE}/${GUARD_REL}" 2>&1; }

# ── the cases ───────────────────────────────────────────────────────────────
# expect_red <label> <assertion-substring> — the guard must exit non-zero AND
# the named assertion must be among the ones that failed.
expect_red() { # expect_red <label> <assertion-substring>
	local out rc
	out="$(guard)"
	rc=$?
	if [[ $rc -eq 0 ]]; then
		bad "$1" "the guard passed; the violation was invisible to it"
		return
	fi
	if ! grep -qF "  FAIL  $2" <<<"$out"; then
		bad "$1" "the guard failed, but not on \"$2\" — so that check is still asleep"
		printf '          %s\n' "$(grep -F '  FAIL  ' <<<"$out" | head -5)"
		return
	fi
	ok "$1"
}

# expect_green <label> — a legitimate change must not fire anything.
expect_green() { # expect_green <label>
	local out rc
	out="$(guard)"
	rc=$?
	if [[ $rc -ne 0 ]]; then
		bad "$1" "$(grep -F '  FAIL  ' <<<"$out" | head -5)"
		return
	fi
	ok "$1"
}

# Append a line to a file in the copy.
add() { # add <repo-relative-path> <text>
	printf '%s\n' "$2" >>"${TREE}/$1"
}

LAKE_TOML=crates/pkdump-lake/Cargo.toml
KEYS_RS=crates/pkdump-lake/src/keys.rs
TENANT_RS=crates/pkdump-lake/src/tenant.rs
CATALOG_PY=lake/src/pkdump_lake/catalog.py
PRICES_PY=lake/src/pkdump_lake/prices.py
VS_PY=lake/src/pkdump_lake/value_snapshots.py
SHIP_RUN=crates/pkdump-ship/src/run.rs
SHIP_ENCODE=crates/pkdump-ship/src/encode.rs
DB_TOML=crates/pkdump-db/Cargo.toml

log "0. the control: an unmutated copy passes"
# If this fails, every red below proves nothing — the mutation would not be
# what reddened the guard.
expect_green "the pristine copy is green"

log "1. THE ONE THE DESIGN NAMES: a tenant column on a CATALOG table"
# "add a tenant-keyed column to a catalog table in a test fixture, assert the
# gate goes RED" — the requirement this file exists for, first because it is
# the literal half of the epic brief's assertion 3.
add "$PRICES_PY" '# selftest: NestedField(99, "database_id", StringType(), required=False)'
expect_red "a database_id column on catalog.prices is refused" \
	"every Iceberg schema field is catalog-scoped"
restore "$PRICES_PY"

add "$CATALOG_PY" '# selftest: pa.field("tenant_handle", pa.string())'
expect_red "a tenant_handle column anywhere in the catalog schema is refused" \
	"every Iceberg schema field is catalog-scoped"
restore "$CATALOG_PY"

log "2. the catalog zone's other rules still bite"
add "$LAKE_TOML" 'rusqlite.workspace = true'
expect_red "the lake crate linking SQLite is refused" \
	"it depends on no SQLite crate and not pkdump-db"
restore "$LAKE_TOML"

add "$KEYS_RS" 'pub fn oops(registry: &str) -> String { registry.into() }'
expect_red "a catalog-zone module naming the registry is refused" \
	"no catalog-zone source names a tenant or a SQLite handle"
restore "$KEYS_RS"

# An identifier, not a string: the scan reads comment- AND string-stripped
# source, deliberately, so `env::var("PKDUMP_USER")` is invisible to it and the
# structural sections (§1, §4) are what close that. A violation written as a
# string would "prove" a check that cannot see it.
add crates/pkdump-lakehouse/src/main.rs \
	'fn selftest_oops(registry: &str) -> &str { registry }'
expect_red "the offline derive naming the registry is refused" \
	"no pkdump-lakehouse source names a tenant or the registry"
restore crates/pkdump-lakehouse/src/main.rs

add "$CATALOG_PY" 'import sqlite3'
expect_red "a write-path module importing sqlite3 is refused" \
	"no write-path module imports sqlite3"
restore "$CATALOG_PY"

add "$VS_PY" 'def selftest_oops(cat): return cat.create_table("x", schema=None)'
expect_red "the transform tier writing to Iceberg is refused" \
	"it makes no Iceberg write call"
restore "$VS_PY"

# The reader half. freshness.py is the module that made this a hole rather
# than a hypothetical: it landed after the guard, in none of its lists, and
# was scanned by nothing.
add lake/src/pkdump_lake/freshness.py 'def selftest_oops(registry): return registry'
expect_red "a catalog-zone reader naming the registry is refused" \
	"no catalog-zone reader reaches the registry or the tenants dir"
restore lake/src/pkdump_lake/freshness.py

log "3. the tenant zone's own rules — the inverted half (pd-uz8q)"
# A key builder that needs no tenant id builds an object outside every
# tenant's deletion prefix. This is the failure the partition order exists to
# prevent, so it is the one the section has to catch.
add "$TENANT_RS" \
	'pub fn every_tenants_index(as_of: &str) -> Result<String> { Ok(as_of.into()) }'
expect_red "a tenant-zone key that is not keyed by a tenant is refused" \
	"every tenant-zone key builder takes a database_id"
restore "$TENANT_RS"

add "$TENANT_RS" \
	'pub fn whoami(registry: &str) -> &str { registry }'
expect_red "the tenant zone looking a tenant up is refused" \
	"the tenant zone resolves no tenant identity"
restore "$TENANT_RS"

# The containment failure the section is really about: a tenant zone moved
# INSIDE the catalog's prefix is governed by the catalog's lifecycle rule and
# reachable by the catalog's credential, while every other line in the tree
# still reads as though the zones were separate.
sed -i 's|TENANT_ROOT: &str = "tenant/"|TENANT_ROOT: \&str = "lake/tenant/"|' \
	"${TREE}/${TENANT_RS}"
expect_red "a tenant root nested inside a catalog root is refused" \
	"no zone root contains another"
restore "$TENANT_RS"

log "4. the shipper stays inside the tenant zone (pd-dxn3)"
add "$SHIP_RUN" 'pub const OOPS: &str = "raw/source=tcgcsv/holdings.parquet";'
expect_red "the shipper naming a catalog prefix is refused" \
	"no catalog-zone prefix appears in the shipper"
restore "$SHIP_RUN"

add "$SHIP_RUN" 'pub fn oops() -> Option<String> { std::env::var("AWS_PROFILE").ok() }'
expect_red "the shipper reaching for the catalog credential is refused" \
	"the shipper names no catalog credential"
restore "$SHIP_RUN"

add "$SHIP_RUN" 'pub fn oops() -> pkdump_lake::Result<pkdump_lake::RawLanding> { pkdump_lake::open() }'
expect_red "the shipper opening the catalog landing zone is refused" \
	"the shipper calls no catalog-zone entry point"
restore "$SHIP_RUN"

# Vacuity, from the other side: the containment assertions above are worth
# nothing if the shipper stopped writing to the zone altogether.
sed -i 's/open_tenant_zone/open_somewhere_else/g' "${TREE}/crates/pkdump-ship/src/bin/pkdump-ship.rs"
expect_red "a shipper that no longer opens the tenant zone is refused" \
	"the shipper opens the tenant zone through its own entry point"
restore crates/pkdump-ship/src/bin/pkdump-ship.rs

log "5. the online path (the outbox's premise, pd-5m54)"
add "$DB_TOML" 'pkdump-lake.workspace = true'
expect_red "the outbox's crate linking the lake is refused" \
	"no online crate depends on a lake crate or the S3 SDK"
restore "$DB_TOML"

add crates/pkdump-server/Cargo.toml 'aws-sdk-s3.workspace = true'
expect_red "the request-serving crate holding an S3 client is refused" \
	"no online crate depends on a lake crate or the S3 SDK"
restore crates/pkdump-server/Cargo.toml

log "6. the zones do not reach each other"
add "$KEYS_RS" 'pub fn oops(id: &str) -> String { format!("{}{id}", crate::TENANT_ROOT) }'
expect_red "catalog-zone code building a tenant key is refused" \
	"no catalog-zone module reaches the tenant zone"
restore "$KEYS_RS"

add "$TENANT_RS" 'pub fn oops() -> String { crate::keys::run_prefix().into() }'
expect_red "tenant-zone code building a catalog key is refused" \
	"the tenant zone builds no catalog key"
restore "$TENANT_RS"

log "7. fail closed — the failure mode that has cost this repo three gates"
# Every corpus disappearing is the shape all three of those gates had: nothing
# to check, so nothing failed. Each of these leaves the property TRUE and the
# guard blind, which is exactly why they have to be red.
rm -rf "${TREE}/crates/pkdump-lakehouse/src"
expect_red "a corpus that has moved away is refused" \
	"pkdump-lakehouse has Rust sources"
build_tree

rm -rf "${TREE}/crates/pkdump-ship/src"
expect_red "the shipper's corpus disappearing is refused" \
	"pkdump-ship has Rust sources"
build_tree

rm -f "${TREE}/lake/src/pkdump_lake/raw.py"
expect_red "a write-path module that has moved away is refused" \
	"every write-path python module was found"
build_tree

# The one the zone split introduces, and the reason the split is three lists
# that must cover the directory rather than one list of exceptions.
printf 'pub fn oops(registry: &str) -> &str { registry }\n' \
	>"${TREE}/crates/pkdump-lake/src/unclassified.rs"
expect_red "a new lake-crate source in no zone is refused" \
	"every lake-crate source is classified into exactly one zone"
build_tree

printf 'def oops(registry):\n    return registry\n' \
	>"${TREE}/lake/src/pkdump_lake/unclassified.py"
expect_red "a new lake python module in no zone is refused" \
	"every lake python module is classified into exactly one zone"
build_tree

mkdir -p "${TREE}/crates/pkdump-lake/src/nested"
printf 'pub fn oops(registry: &str) -> &str { registry }\n' \
	>"${TREE}/crates/pkdump-lake/src/nested/mod.rs"
expect_red "a lake-crate source hidden in a subdirectory is refused" \
	"the lake crate's sources are all at the top of src/"
build_tree

log "8. and what must NOT fire: the tenant zone doing its job"
# The whole point of the re-cut. Every one of these is the tenant zone being
# legitimately tenant-keyed, which is what it is FOR. A guard that reddened
# here would be one whose first contact with the epic's real code is a false
# positive — and false positives are how a rule acquires an exemption list.
add "$TENANT_RS" \
	'pub fn tenant_manifest_key(database_id: &str, as_of: &str) -> Result<String> {
    Ok(format!("{}manifest-{as_of}.json", tenant_prefix(database_id)?))
}'
expect_green "a new tenant-zone key builder, keyed by database_id, is fine"
restore "$TENANT_RS"

# The holdings part deliberately carries no database_id column today — the
# partition does. If item 7's valuations ever need one, that is a decision in
# the tenant zone and this guard has no opinion about it. It would have one if
# the same column appeared on a CATALOG table, which is §1 above.
add "$SHIP_ENCODE" 'pub const SELFTEST_SCHEMA: &str = "
message valuation {
  required binary database_id (UTF8);
  required binary tenant_handle (UTF8);
  required int64 value_cents;
}
";'
expect_green "a tenant-keyed column in a TENANT-zone Parquet schema is fine"
restore "$SHIP_ENCODE"

# A new dataset under `tenant/` is an ordinary thing for this epic to add.
add "$TENANT_RS" '// selftest: pub const WISHLISTS: &str = "wishlists";'
expect_green "a new dataset in the tenant zone is fine"
restore "$TENANT_RS"

# The transform tier opens every tenant database on purpose, and says so all
# over its own source. Nothing here may object to that.
add "$VS_PY" 'def selftest_reads_a_tenant(database_id): return f"tenants/{database_id}.sqlite"'
expect_green "the transform tier opening a tenant database is fine"
restore "$VS_PY"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — every section of the guard has been seen RED for the violation"
echo "         it exists to catch, and green for the tenant zone doing its job."
