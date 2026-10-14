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

SITE_TOML=crates/pkdump-site/Cargo.toml
SITE_RS=crates/pkdump-site/src/lib.rs
CATALOG_PY=lake/src/pkdump_lake/catalog.py
PRICES_PY=lake/src/pkdump_lake/prices.py
VS_PY=lake/src/pkdump_lake/value_snapshots.py
SHIP_RS=crates/pkdump-ship/src/lib.rs
SHIP_BIN=crates/pkdump-ship/src/bin/pkdump-ship.rs
ERASE_RS=crates/pkdump-erase/src/lib.rs
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
add "$SITE_TOML" 'rusqlite.workspace = true'
expect_red "the site's lake declarations linking SQLite are refused" \
	"it depends on no SQLite crate and not pkdump-db"
restore "$SITE_TOML"

add "$SITE_RS" 'pub fn oops(registry: &str) -> String { registry.into() }'
expect_red "the site's lake root naming the registry is refused" \
	"the site's lake root resolves no tenant identity"
restore "$SITE_RS"

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

log "3. the shipper and the eraser stay inside the tenant zone (pd-dxn3)"
add "$SHIP_RS" 'pub const OOPS: &str = "raw/source=tcgcsv/holdings.parquet";'
expect_red "the shipper naming a catalog prefix is refused" \
	"no catalog-zone prefix appears in the shipper"
restore "$SHIP_RS"

add "$ERASE_RS" 'pub const OOPS: &str = "lake/warehouse/prices";'
expect_red "the eraser naming a catalog prefix is refused" \
	"no catalog-zone prefix appears in the eraser"
restore "$ERASE_RS"

add "$SHIP_RS" 'pub fn oops() -> Option<String> { std::env::var("AWS_PROFILE").ok() }'
expect_red "the shipper reaching for the catalog credential is refused" \
	"the shipper names no catalog credential"
restore "$SHIP_RS"

add "$SHIP_RS" 'pub fn oops() { let _ = dumpster_kit_lake::open; }'
expect_red "the shipper opening the catalog landing zone is refused" \
	"the shipper calls no catalog-zone entry point"
restore "$SHIP_RS"

add "$SHIP_RS" 'pub fn oops(d: &str) -> String { dumpster_kit_lake::range_part_key(d) }'
expect_red "the shipper building an object key by hand is refused" \
	"the shipper builds no object key of its own"
restore "$SHIP_RS"

# Vacuity, from the other side: the containment assertions above are worth
# nothing if the shipper stopped writing to the zone altogether, or stopped
# using the kit's shipper to do it.
sed -i 's/open_tenant_zone/open_somewhere_else/g' "${TREE}/${SHIP_BIN}"
expect_red "a shipper that no longer opens the tenant zone is refused" \
	"the shipper opens the tenant zone through its own entry point"
restore "$SHIP_BIN"

sed -i 's/dumpster_kit_ship::/somewhere_else::/g' "${TREE}/${SHIP_RS}" "${TREE}/${SHIP_BIN}"
expect_red "a shipper that no longer ships with the kit's shipper is refused" \
	"and ships with the kit's shipper"
restore "$SHIP_RS"
restore "$SHIP_BIN"

sed -i 's/open_tenant_zone_purge/open_somewhere_else/g' "${TREE}/crates/pkdump-erase/src/bin/pkdump-erase.rs"
expect_red "an eraser that no longer opens the purge handle is refused" \
	"the eraser opens the tenant zone through its purge entry point"
restore crates/pkdump-erase/src/bin/pkdump-erase.rs

log "5. the online path (the outbox's premise, pd-5m54)"
add "$DB_TOML" 'dumpster-kit-lake.workspace = true'
expect_red "the outbox's crate linking the lake is refused" \
	"no online crate depends on a lake crate or the S3 SDK"
restore "$DB_TOML"

add crates/pkdump-server/Cargo.toml 'aws-sdk-s3.workspace = true'
expect_red "the request-serving crate holding an S3 client is refused" \
	"no online crate depends on a lake crate or the S3 SDK"
restore crates/pkdump-server/Cargo.toml

add "$DB_TOML" 'dumpster-kit-keys.workspace = true'
expect_red "the outbox's crate linking key custody is refused" \
	"no online crate depends on a lake crate or the S3 SDK"
restore "$DB_TOML"

log "6. the zones do not reach each other"
add crates/pkdump-derive/Cargo.toml 'dumpster-kit-ship.workspace = true'
expect_red "the catalog build linking the shipper is refused" \
	"no catalog-build crate depends on key custody, the shipper or the eraser"
restore crates/pkdump-derive/Cargo.toml

add crates/pkdump-lakehouse/Cargo.toml 'pkdump-erase.workspace = true'
expect_red "the offline derive linking the eraser is refused" \
	"no catalog-build crate depends on key custody, the shipper or the eraser"
restore crates/pkdump-lakehouse/Cargo.toml

add crates/pkdump-lakehouse/src/partition.rs 'pub fn oops() { let _ = open_tenant_zone; }'
expect_red "the catalog build naming a tenant-zone entry point is refused" \
	"the catalog-build path reaches no tenant-zone entry point"
restore crates/pkdump-lakehouse/src/partition.rs

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

rm -rf "${TREE}/crates/pkdump-erase/src"
expect_red "the eraser's corpus disappearing is refused" \
	"pkdump-erase has Rust sources"
build_tree

rm -rf "${TREE}/crates/pkdump-site/src"
expect_red "the site's declarations disappearing is refused" \
	"pkdump-site has Rust sources"
build_tree

rm -f "${TREE}/lake/src/pkdump_lake/raw.py"
expect_red "a write-path module that has moved away is refused" \
	"every write-path python module was found"
build_tree

# The one the classification introduces, and the reason it is a list that must
# cover the directory rather than a list of exceptions.
printf 'pub fn oops(registry: &str) -> &str { registry }\n' \
	>"${TREE}/crates/pkdump-site/src/unclassified.rs"
expect_red "a new site-crate source in no list is refused" \
	"every site-crate source is classified"
build_tree

printf 'def oops(registry):\n    return registry\n' \
	>"${TREE}/lake/src/pkdump_lake/unclassified.py"
expect_red "a new lake python module in no zone is refused" \
	"every lake python module is classified into exactly one zone"
build_tree

mkdir -p "${TREE}/crates/pkdump-site/src/nested"
printf 'pub fn oops(registry: &str) -> &str { registry }\n' \
	>"${TREE}/crates/pkdump-site/src/nested/mod.rs"
expect_red "a site-crate source hidden in a subdirectory is refused" \
	"the site crate's sources are all at the top of src/"
build_tree

log "8. and what must NOT fire: the zones doing their jobs"
# The whole point of the re-cut. Every one of these is a zone being legitimately
# what it is FOR. A guard that reddened here would be one whose first contact
# with the epic's real code is a false positive -- and false positives are how a
# rule acquires an exemption list.

# The site declaring another upstream is an ordinary thing for it to do.
add "$SITE_RS" 'pub const SELFTEST_UPSTREAM: Source = Source::new("selftest-upstream");'
expect_green "a new upstream declared by the site is fine"
restore "$SITE_RS"

# The shipper opens every tenant's database on purpose and says database_id all
# over its source. Nothing here may object to that.
add "$SHIP_RS" 'pub fn selftest_reads_a_tenant(database_id: &str) -> String { format!("tenants/{database_id}.sqlite") }'
expect_green "the shipper naming a database_id is fine"
restore "$SHIP_RS"

# The holdings part deliberately carries no database_id column today -- the
# partition does. A tenant-keyed column in a TENANT-zone schema is a decision
# for the tenant zone and this guard has no opinion about it. It would have one
# if the same column appeared on a CATALOG table, which is the first case above.
add "$SHIP_RS" 'pub const SELFTEST_SCHEMA: &str = "
message valuation {
  required binary database_id (UTF8);
  required binary tenant_handle (UTF8);
  required int64 value_cents;
}
";'
expect_green "a tenant-keyed column in a TENANT-zone Parquet schema is fine"
restore "$SHIP_RS"

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
