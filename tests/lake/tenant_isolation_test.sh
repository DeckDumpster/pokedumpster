#!/usr/bin/env bash
# Mechanical guard: tenant data stays out of the lake (pd-cgi9).
#
# Assertion 3 of the lakehouse-end-to-end epic brief, verbatim:
#
#   "No Iceberg table carries a tenant-identifying column, and no lake write
#    path opens a tenant database. Assert it mechanically: a rule nobody can
#    grep for is a rule that erodes."
#
# The property held when this was written. What did not exist was anything that
# would notice it stopping. Every statement of the rule in the tree was a
# comment — tests/lake/prices.sh:31, tests/lake/run.sh:35, lake/src/pkdump_lake/
# __init__.py:5, roundtrip.py:30, deploy/README.md:379 — so adding a tenant_id
# column to an Iceberg schema, or opening a tenant SQLite in a lake job, broke
# nothing and failed nothing.
#
# Contrast the sibling rule that IS mechanical: "images are never landed" is
# held by the closed `Source` enum in crates/pkdump-lake/src/keys.rs — there is
# no images variant, so landing one takes a code change that shows up in
# review. This file is the missing equivalent for "no tenant data".
#
# ── THE SHAPE OF THE RULE ────────────────────────────────────────────────────
# "Nothing under lake/ may mention a tenant" is the WRONG rule, and asserting it
# would have to be deleted the first time it ran. Three tiers, three different
# permissions:
#
#   * the LAKE WRITE PATH (crates/pkdump-lake, crates/pkdump-lakehouse, and
#     lake/src/pkdump_lake/{catalog,prices,raw,roundtrip}.py) may not go near a
#     tenant at all;
#   * VERIFY (verify.py) opens one database — the shared catalog — read-only,
#     on a verification path, and must not reach the registry or tenants/;
#   * the TRANSFORM TIER (value_snapshots.py) opens EVERY tenant's database on
#     purpose: it is the job that walks the registry and writes each tenant's
#     value snapshot back to that tenant's own SQLite. Its rule is the other
#     direction — it reads the lake and never writes to it.
#
# So the guard is per-tier, and prefers STRUCTURAL assertions to greps wherever
# the structure will carry them:
#
#   §1 crates/pkdump-lake depends on no SQLite crate at all. It cannot open a
#      tenant database because it cannot open a database. Strongest form here,
#      and the direct analogue of the closed Source enum.
#   §2 crates/pkdump-lakehouse DOES link pkdump-db — it writes the shared
#      catalog — so the structural guard is unavailable and this is a grep, over
#      comment-stripped source, for the tenant-resolving half of that API.
#   §3 the Python write path imports no sqlite3. Structural again: a module that
#      cannot open a database cannot open a tenant's, whatever path string it
#      builds.
#   §4 the sqlite3 importers are EXACTLY the allowlist. A new one is a decision,
#      not an accident.
#   §5 no Iceberg schema field name is tenant-identifying — the literal first
#      half of the assertion.
#   §6 the transform tier never writes to Iceberg.
#   §7 fail closed: every corpus above was non-empty. A guard whose file glob
#      silently matches nothing passes forever.
#
# Comments are stripped before grepping, because every one of these files
# DISCUSSES tenants at length and a naive grep would match the prose explaining
# the rule. Strings are stripped with them on the identifier checks; §1/§3 are
# what stop a tenant path from being reached through a string literal.
#
# Deliberately hermetic — no podman, no network, no MinIO, no Nessie, no build —
# so deploy/ci.sh runs it in the sub-second lint tier beside
# tests/container/base_images_test.sh, and NOT in the two-minute lake tier it is
# named after. A docs-only PR runs it too. python3 is used as a parser (Rust
# comment stripping, Python tokenization); it is already a hard requirement of
# lake/ itself.
#
#   bash tests/lake/tenant_isolation_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		echo "          expected: $2"
		echo "          actual:   $3"
		fail=$((fail + 1))
	fi
}
# "Something mentions a tenant" is useless without saying where.
none() { # none <label> <lines>
	if [[ -z "$2" ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		printf '          %s\n' "$2"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# ── what "tenant-identifying" means ─────────────────────────────────────────
# Two different questions, so two different patterns.
#
# IDENT_RX — source identifiers that reach a tenant. `registry` and `tenants`
# are the layout (`pkdump_db::paths`), `database_id` is the opaque id a handle
# resolves to, `PKDUMP_USER` selects one.
IDENT_RX='tenant|database_id|user_db|registry|collection\.sqlite|PKDUMP_USER'
# FIELD_RX — column names that would key a row by tenant. Wider and blunter,
# because an Iceberg schema is small, reviewed rarely, and forever: a column is
# the thing that actually makes a table tenant-scoped.
FIELD_RX='tenant|handle|database_id|user|owner|account|profile|collection|registry'

# ── parsers ─────────────────────────────────────────────────────────────────
# Rust with comments removed. A state machine rather than `sed 's|//.*||'`,
# which cannot tell a comment from `"https://…"` and would strip the rest of
# any line holding a URL — hiding a violation that follows it on that line.
# Line numbers are preserved so a failure names a real line.
rust_code() { # rust_code <file>
	python3 - "$1" <<-'PY'
		import sys
		src = open(sys.argv[1], encoding="utf-8").read()
		out, i, n = [], 0, len(src)
		state = "code"  # code | line | block | string | raw | char
		hashes = 0
		while i < n:
		    c = src[i]
		    nxt = src[i + 1] if i + 1 < n else ""
		    if state == "code":
		        if c == "/" and nxt == "/":
		            state, i = "line", i + 2
		            continue
		        if c == "/" and nxt == "*":
		            state, i = "block", i + 2
		            continue
		        if c == "r" and nxt in ('"', "#"):
		            j = i + 1
		            h = 0
		            while j < n and src[j] == "#":
		                h += 1
		                j += 1
		            if j < n and src[j] == '"':
		                hashes, state, i = h, "raw", j + 1
		                out.append(" ")
		                continue
		        if c == '"':
		            state, i = "string", i + 1
		            out.append(" ")
		            continue
		        out.append(c)
		        i += 1
		        continue
		    if state == "line":
		        if c == "\n":
		            state = "code"
		            out.append("\n")
		        i += 1
		        continue
		    if state == "block":
		        if c == "*" and nxt == "/":
		            state, i = "code", i + 2
		            continue
		        if c == "\n":
		            out.append("\n")
		        i += 1
		        continue
		    if state == "string":
		        if c == "\\":
		            i += 2
		            continue
		        if c == '"':
		            state = "code"
		        elif c == "\n":
		            out.append("\n")
		        i += 1
		        continue
		    if state == "raw":
		        if c == '"' and src[i + 1 : i + 1 + hashes] == "#" * hashes:
		            state, i = "code", i + 1 + hashes
		            continue
		        if c == "\n":
		            out.append("\n")
		        i += 1
		        continue
		for lineno, line in enumerate("".join(out).split("\n"), 1):
		    print(f"{lineno}:{line}")
	PY
}

# Python as tokens, with COMMENT and STRING dropped — which is what removes
# every docstring, and the docstrings are where this codebase states the rule.
# One token per line, prefixed with its real line number.
py_code() { # py_code <file>
	python3 - "$1" <<-'PY'
		import sys, tokenize
		path = sys.argv[1]
		try:
		    with open(path, "rb") as fh:
		        for tok in tokenize.tokenize(fh.readline):
		            if tok.type in (tokenize.COMMENT, tokenize.STRING):
		                continue
		            if tok.string.strip():
		                print(f"{tok.start[0]}:{tok.string}")
		except (tokenize.TokenError, IndentationError, SyntaxError) as exc:
		    print(f"TOKENIZE-FAILED: {exc}", file=sys.stderr)
		    sys.exit(3)
	PY
}

# Every Iceberg field name declared in the tree, `<file>:<line>:<name>`. Two
# spellings, because both are used: pyiceberg's NestedField(id, "name", …) and
# pyarrow's pa.field("name", …).
iceberg_fields() {
	grep -rnoE 'NestedField\([0-9]+,[[:space:]]*"[^"]+"|pa\.field\([[:space:]]*"[^"]+"' \
		"${REPO_DIR}/lake/src" |
		sed -E 's/.*"([^"]+)"$/\0/' |
		sed -E 's/(NestedField\([0-9]+,[[:space:]]*|pa\.field\([[:space:]]*)"([^"]+)"/\2/'
}

# ── the corpora ─────────────────────────────────────────────────────────────
LAKE_CRATE="${REPO_DIR}/crates/pkdump-lake"
LAKEHOUSE_CRATE="${REPO_DIR}/crates/pkdump-lakehouse"
PYLAKE="${REPO_DIR}/lake/src/pkdump_lake"

# The Python modules that make up the lake write path. verify.py and
# value_snapshots.py are deliberately absent — they have their own sections.
WRITE_PATH_PY=(catalog.py prices.py raw.py roundtrip.py __init__.py)
# The only modules allowed to import sqlite3, and why.
SQLITE_ALLOWED=(value_snapshots.py verify.py)

rust_sources() { # rust_sources <crate-dir>
	find "$1/src" -name '*.rs' -type f 2>/dev/null | sort
}

log "1. the raw-landing crate cannot open a database at all (structural)"
# pkdump-lake writes raw/ and nothing else. It links no SQLite: not rusqlite,
# not pkdump-db, not libsqlite3-sys. That is why "no lake write path opens a
# tenant database" is true of it by construction rather than by discipline —
# and adding one of these to its Cargo.toml is the code change that has to show
# up in review, exactly like adding a variant to Source.
LAKE_TOML="${LAKE_CRATE}/Cargo.toml"
check "crates/pkdump-lake/Cargo.toml exists" "yes" \
	"$([[ -f "$LAKE_TOML" ]] && echo yes || echo no)"
DBDEPS="$(grep -nE '^[[:space:]]*(rusqlite|pkdump-db|libsqlite3-sys|sqlx|diesel)[[:space:]]*[.=]' \
	"$LAKE_TOML")"
none "it depends on no SQLite crate and not pkdump-db" "$DBDEPS"

# And nothing in its source names one either — a path dependency added under a
# different key, or a re-export reached through another crate, would not show
# up above.
LAKE_RS_HITS=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	hits="$(rust_code "$f" | grep -inE "${IDENT_RX}|rusqlite|pkdump_db" |
		sed "s|^|${f#"$REPO_DIR"/}:|")"
	[[ -n "$hits" ]] && LAKE_RS_HITS+="${hits}"$'\n'
done < <(rust_sources "$LAKE_CRATE")
none "no pkdump-lake source names a tenant or a SQLite handle" "${LAKE_RS_HITS%$'\n'}"

log "2. the offline derive calls no tenant-resolving API (pd-1uem's crate)"
# pkdump-lakehouse cannot get the structural guard: it derives shared.sqlite, so
# it links pkdump-db and rusqlite legitimately. What it must never touch is the
# tenant-resolving half of that API — pkdump_db::tenants::*, ::registry::*,
# tenant_db_file/tenant_db_path*, tenants_dir. It opens exactly one thing,
# `pkdump_db::open_shared(shared_db_path())`, plus the two user-named paths the
# `diff` subcommand compares.
LAKEHOUSE_HITS=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	hits="$(rust_code "$f" |
		grep -inE "${IDENT_RX}|tenant_db_file|tenant_db_path|tenants_dir" |
		sed "s|^|${f#"$REPO_DIR"/}:|")"
	[[ -n "$hits" ]] && LAKEHOUSE_HITS+="${hits}"$'\n'
done < <(rust_sources "$LAKEHOUSE_CRATE")
none "no pkdump-lakehouse source names a tenant or the registry" "${LAKEHOUSE_HITS%$'\n'}"

log "3. the Python write path cannot open a database at all (structural)"
# Same argument as §1, one tier up: a module that never imports sqlite3 cannot
# open a tenant database however it assembles the path. This is what makes the
# string-stripping in §4's grep safe.
PY_SQLITE=""
PY_TENANT=""
for m in "${WRITE_PATH_PY[@]}"; do
	f="${PYLAKE}/${m}"
	[[ -f "$f" ]] || continue
	hits="$(grep -nE '^[[:space:]]*(import[[:space:]]+sqlite3|from[[:space:]]+sqlite3)' "$f" |
		sed "s|^|lake/src/pkdump_lake/${m}:|")"
	[[ -n "$hits" ]] && PY_SQLITE+="${hits}"$'\n'
	hits="$(py_code "$f" | grep -inE "${IDENT_RX}" |
		sed "s|^|lake/src/pkdump_lake/${m}:|")"
	[[ -n "$hits" ]] && PY_TENANT+="${hits}"$'\n'
done
none "no write-path module imports sqlite3" "${PY_SQLITE%$'\n'}"
none "no write-path module names a tenant in code" "${PY_TENANT%$'\n'}"

log "4. the modules that may open a database are exactly the allowlist"
# A new sqlite3 importer under lake/ is a decision about tenant isolation, and
# it should have to be argued for here rather than merged as an import line.
ACTUAL_SQLITE="$(grep -lE '^[[:space:]]*(import[[:space:]]+sqlite3|from[[:space:]]+sqlite3)' \
	"${PYLAKE}"/*.py 2>/dev/null | xargs -r -n1 basename | sort | tr '\n' ' ')"
EXPECTED_SQLITE="$(printf '%s\n' "${SQLITE_ALLOWED[@]}" | sort | tr '\n' ' ')"
check "sqlite3 importers under lake/src/pkdump_lake" \
	"$EXPECTED_SQLITE" "$ACTUAL_SQLITE"

# verify.py is on that list for ONE database — the shared catalog, opened
# query_only on a verification path. It has no business reaching the registry or
# the tenants directory, and that is what separates it from the transform tier.
VERIFY_HITS="$(py_code "${PYLAKE}/verify.py" | grep -inE "${IDENT_RX}" |
	sed 's|^|lake/src/pkdump_lake/verify.py:|')"
none "verify.py reaches no registry and no tenants dir" "$VERIFY_HITS"

log "5. no Iceberg table carries a tenant-identifying column"
# The literal first half of assertion 3. catalog.prices is five fields —
# tcgplayer_product_id, sub_type_name, price_type, price, observed_date — and
# none of them says which collection the row belongs to, because none of them
# can: the lake holds catalog data only.
FIELDS="$(iceberg_fields)"
BAD_FIELDS="$(grep -iE ":(${FIELD_RX})$|:[a-z0-9_]*(${FIELD_RX})[a-z0-9_]*$" <<<"$FIELDS")"
none "every Iceberg schema field is catalog-scoped" "$BAD_FIELDS"

log "6. the transform tier reads the lake and never writes to it"
# value_snapshots.py is the one job allowed to open tenant databases, so the
# rule for it runs the other way: prices come OUT of Iceberg, and nothing about
# a tenant goes back IN. It uses load_table + scan; a write would be
# create_table, append/overwrite on a table, or add_files.
#
# `.append(` alone would match `outcomes.append(...)` — a Python list — so the
# pattern is the table-receiver and pyarrow spellings, not the bare method.
VS="${PYLAKE}/value_snapshots.py"
check "the transform tier is present to check" "yes" \
	"$([[ -f "$VS" ]] && echo yes || echo no)"
WRITES="$(grep -nE 'create_table|create_namespace|add_files|\.overwrite\(|table\.append\(|\.append\(pa\.' \
	"$VS" | sed 's|^|lake/src/pkdump_lake/value_snapshots.py:|')"
none "it makes no Iceberg write call" "$WRITES"

log "7. the guard read something (fail closed)"
# Every section above passes vacuously if its corpus is empty — a renamed crate
# directory or a moved python package turns this whole file green while
# asserting nothing. Assert the inputs were real.
check "pkdump-lake has Rust sources" "yes" \
	"$([[ -n "$(rust_sources "$LAKE_CRATE")" ]] && echo yes || echo no)"
check "pkdump-lakehouse has Rust sources" "yes" \
	"$([[ -n "$(rust_sources "$LAKEHOUSE_CRATE")" ]] && echo yes || echo no)"
FOUND_PY=0
for m in "${WRITE_PATH_PY[@]}"; do [[ -f "${PYLAKE}/${m}" ]] && FOUND_PY=$((FOUND_PY + 1)); done
check "every write-path python module was found" "${#WRITE_PATH_PY[@]}" "$FOUND_PY"
check "Iceberg schema fields were found to check" "yes" \
	"$([[ -n "$FIELDS" ]] && echo yes || echo no)"
# The parsers themselves: if python3 stopped emitting anything, §1-§4 go green
# by producing no lines to grep.
check "the Rust comment stripper produced output" "yes" \
	"$([[ -n "$(rust_code "${LAKE_CRATE}/src/keys.rs")" ]] && echo yes || echo no)"
check "the Python tokenizer produced output" "yes" \
	"$([[ -n "$(py_code "${PYLAKE}/prices.py")" ]] && echo yes || echo no)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — no Iceberg field is tenant-identifying, the lake write path"
echo "         links no SQLite at all, and the transform tier only reads."
