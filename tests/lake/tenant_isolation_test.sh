#!/usr/bin/env bash
# Mechanical guard: tenant identity stays out of the CATALOG zone (pd-cgi9,
# re-cut by pd-7x83).
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
# ── WHY THE AXIS MOVED (pd-7x83) ────────────────────────────────────────────
# The rule as first written was "the LAKE holds no tenant data", and its
# corpora were directory globs over crates/pkdump-lake, crates/pkdump-lakehouse
# and lake/src/pkdump_lake. The inbound-leg epic (pd-8lw7) makes that premise
# false in a way that is DESIGNED rather than accidental: the same bucket now
# holds a **tenant zone** under `tenant/` — holdings and valuations, always
# tenant-keyed, retained 90 days, reached by credentials that reach nothing
# else — and its layout module lives in crates/pkdump-lake beside the catalog's.
#
# So the axis is no longer the lake and not-the-lake. It is the CATALOG ZONE
# (`raw/`, `lake/` — cross-tenant, shared, indefinitely retained) against the
# TENANT ZONE (`tenant/` — tenant-keyed by construction, governed separately).
# The catalog zone keeps every assertion it had. The tenant zone is a named
# carve-out with its OWN rules, and they run the other way:
#
#     catalog zone   must never be keyed by a tenant
#     tenant zone    must always be keyed by a tenant, must resolve none,
#                    and must never reach the catalog
#
# The carve-out is by ZONE, and it is TOTAL rather than a list of exceptions:
# §12 asserts every Rust file in crates/pkdump-lake and every Python module
# under lake/src is classified into exactly one zone, so a new file is a zone
# decision that fails this gate until someone makes it. A per-file exemption
# list is what erodes; a classification that must cover the directory cannot be
# added to silently.
#
# Where the original sections went: old §1 split into §1 (the structural half,
# which now covers BOTH zones and is stronger for it) and §2 (the catalog-zone
# grep); §2→§3, §3→§4, §4→§5, §5→§6, §6→§7; §7 (fail closed) is now §12,
# because it has to run after the sections it is checking. §8-§11 are new.
#
# ── THE SHAPE OF THE RULE ────────────────────────────────────────────────────
# "Nothing under lake/ may mention a tenant" is the WRONG rule, and asserting it
# would have to be deleted the first time it ran. Tiers, each with different
# permissions:
#
#   * the CATALOG WRITE PATH (the catalog-zone modules of crates/pkdump-lake,
#     crates/pkdump-lakehouse, and lake/src/pkdump_lake/{catalog,prices,raw,
#     roundtrip}.py) may not go near a tenant at all;
#   * VERIFY (verify.py) opens one database — the shared catalog — read-only,
#     on a verification path, and must not reach the registry or tenants/;
#   * the TRANSFORM TIER (value_snapshots.py) opens EVERY tenant's database on
#     purpose: it is the job that walks the registry and writes each tenant's
#     value snapshot back to that tenant's own SQLite. Its rule is the other
#     direction — it reads the lake and never writes to it;
#   * the TENANT ZONE (crates/pkdump-lake/src/tenant.rs) is where tenant-keyed
#     objects are SUPPOSED to live. It may be keyed by a `database_id` it is
#     handed; it may not look one up, and it may not reach the catalog;
#   * the SHIPPER (crates/pkdump-ship) is the only thing in the workspace that
#     writes under `tenant/`. It opens a tenant database on purpose — that is
#     its job — and its rule is the containment one: nothing it writes may land
#     outside the tenant zone;
#   * the ONLINE PATH (crates/pkdump-db, crates/pkdump-server, crates/
#     pkdump-keys) may not reach EITHER zone. The outbox is how holdings leave
#     a collection, and it leaves through a table in that collection's own
#     database.
#
# So the guard is per-tier, and prefers STRUCTURAL assertions to greps wherever
# the structure will carry them:
#
#   §1  crates/pkdump-lake depends on no SQLite crate at all. It cannot open a
#       tenant database because it cannot open a database — and that now covers
#       the tenant zone's own module too, which is why the tenant zone's layout
#       can live in this crate while its writer cannot. Strongest form here,
#       and the direct analogue of the closed Source enum.
#   §2  the catalog-zone modules of that crate name no tenant. A grep, over
#       comment-stripped source.
#   §3  crates/pkdump-lakehouse DOES link pkdump-db — it writes the shared
#       catalog — so the structural guard is unavailable and this is a grep for
#       the tenant-resolving half of that API.
#   §4  the Python write path imports no sqlite3. Structural again: a module
#       that cannot open a database cannot open a tenant's, whatever path
#       string it builds.
#   §5  the sqlite3 importers are EXACTLY the allowlist, and the catalog zone's
#       READERS resolve no tenant either. A new sqlite3 importer is a decision,
#       not an accident.
#   §6  no Iceberg schema field name is tenant-identifying — the literal first
#       half of the assertion.
#   §7  the transform tier never writes to Iceberg.
#   §8  the tenant zone is keyed by tenant and resolves none. The inverted
#       rule: every key it builds takes a database_id, and it reaches no
#       registry, no tenants directory and no PKDUMP_USER.
#   §9  the shipper reaches no catalog prefix, no catalog entry point and no
#       catalog credential — and does reach the tenant zone, so the section is
#       not passing because the shipper does nothing.
#   §10 the online path links neither zone. Structural: pkdump-db (the outbox),
#       pkdump-server and pkdump-keys depend on no lake crate and no S3 SDK.
#   §11 the two zones do not reach each other, in either direction.
#   §12 fail closed: every corpus above was non-empty, and every file — Rust in
#       the lake crate, Python under lake/src — is classified into exactly one
#       zone. A guard whose file glob silently matches nothing passes forever,
#       and a module in no list is scanned by no section: freshness.py was in
#       exactly that state until this section grew its Python half.
#
# Comments are stripped before grepping, because every one of these files
# DISCUSSES tenants at length and a naive grep would match the prose explaining
# the rule. Strings are stripped with them on the identifier checks; §1/§4 are
# what stop a tenant path from being reached through a string literal. §9 is
# the exception that needs them KEPT — an object key IS a string, so "does the
# shipper name a catalog prefix" is a question only about literals.
#
# Deliberately hermetic — no podman, no network, no MinIO, no Nessie, no build —
# so deploy/ci.sh runs it in the lint tier beside
# tests/container/base_images_test.sh (~2s), and NOT in the two-minute lake tier
# it is named after. A docs-only PR runs it too. python3 is used as a parser (Rust
# comment stripping, Python tokenization); it is already a hard requirement of
# lake/ itself.
#
# The credential half of the boundary — that the catalog role cannot READ
# `tenant/` and the tenant role cannot read `raw/` — is not here and cannot be:
# it is a property of two IAM documents against a real bucket, and
# tests/lake/tenant_zone.sh §4-§6 asserts it in both directions, seen red. This
# file is the source-level half: what the code may name, link and build.
#
# Seen red by tests/lake/tenant_isolation_selftest.sh, which injects one
# violation at a time into a copy of the tree and requires this file to fail on
# each — including the one that must NOT fire, a legitimately tenant-keyed
# addition to the tenant zone.
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
# The inverted rules need the other shape: this must be there.
some() { # some <label> <lines>
	if [[ -n "$2" ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1"
		echo "          found nothing, so the rule above is asserting nothing"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# ── what "tenant-identifying" means ─────────────────────────────────────────
# Three different questions, so three different patterns.
#
# IDENT_RX — source identifiers that reach a tenant. `registry` and `tenants`
# are the layout (`pkdump_db::paths`), `database_id` is the opaque id a handle
# resolves to, `PKDUMP_USER` selects one.
IDENT_RX='tenant|database_id|user_db|registry|collection\.sqlite|PKDUMP_USER'
# RESOLVE_RX — IDENT_RX minus the two things the tenant zone is ALLOWED to say.
# `tenant` is its own name and `database_id` is its first partition component,
# so neither can be a violation there; what would be is looking a tenant UP.
# Being handed an id is the whole contract — the registry is the authority on
# which ids exist (`pkdump_db::registry`), and a zone that consulted it would
# be resolving identity rather than partitioning by one.
RESOLVE_RX='user_db|registry|collection\.sqlite|PKDUMP_USER|tenants_dir|tenant_db_file|tenant_db_path|rusqlite|pkdump_db'
# FIELD_RX — column names that would key a row by tenant. Wider and blunter,
# because an Iceberg schema is small, reviewed rarely, and forever: a column is
# the thing that actually makes a table tenant-scoped.
FIELD_RX='tenant|handle|database_id|user|owner|account|profile|collection|registry'

# ── parsers ─────────────────────────────────────────────────────────────────
# Rust with comments removed. A state machine rather than `sed 's|//.*||'`,
# which cannot tell a comment from `"https://…"` and would strip the rest of
# any line holding a URL — hiding a violation that follows it on that line.
# Line numbers are preserved so a failure names a real line.
#
# `rust_code <file> keep-strings` leaves string literals in place, for the one
# question that is about them (§9: an object key is a string).
#
# Memoised on disk rather than in a variable: every caller is inside a `$( )`,
# so a shell-level cache would be written in a subshell and thrown away. Most
# files are scanned by several sections, and a python interpreter per scan per
# file is what put this at two seconds — enough to matter in the lint tier, and
# enough to matter thirty times over in tenant_isolation_selftest.sh.
STRIP_CACHE="$(mktemp -d "${TMPDIR:-/tmp}/pkdump-tenant-guard.XXXXXX")"
trap 'rm -rf "$STRIP_CACHE"' EXIT

cache_path() { printf '%s/%s__%s' "$STRIP_CACHE" "${2:-code}" "${1//\//_}"; }

rust_code() { # rust_code <file> [keep-strings]
	local cached
	cached="$(cache_path "$1" "${2:-code}")"
	[[ -f "$cached" ]] || strip_rust "${2:-code}" "$1" "$cached"
	cat "$cached"
}

# Strip every file in ONE interpreter. Started per file, python is the single
# biggest cost in this gate, and the sections deliberately overlap — a file
# scanned by §2 is scanned again by §11, and the shipper's sources are scanned
# five times by §9 alone.
prime_strip_cache() { # prime_strip_cache <mode> <file>...
	local mode="$1" f args=()
	shift
	for f in "$@"; do
		[[ -f "$f" ]] || continue
		args+=("$f" "$(cache_path "$f" "$mode")")
	done
	[[ ${#args[@]} -gt 0 ]] && strip_rust "$mode" "${args[@]}"
	return 0
}

strip_rust() { # strip_rust <mode> <in> <out> [<in> <out>...]
	python3 - "$@" <<-'PY'
		import sys

		keep = sys.argv[1] == "keep-strings"
		pairs = list(zip(sys.argv[2::2], sys.argv[3::2]))


		def strip(src):
		    out, i, n = [], 0, len(src)
		    state = "code"  # code | line | block | string | raw
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
		                    out.append('r"' if keep else " ")
		                    continue
		            if c == '"':
		                state, i = "string", i + 1
		                out.append('"' if keep else " ")
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
		                if keep:
		                    out.append(src[i : i + 2])
		                i += 2
		                continue
		            if c == '"':
		                state = "code"
		                if keep:
		                    out.append(c)
		            elif c == "\n":
		                out.append("\n")
		            elif keep:
		                out.append(c)
		            i += 1
		            continue
		        if state == "raw":
		            if c == '"' and src[i + 1 : i + 1 + hashes] == "#" * hashes:
		                state, i = "code", i + 1 + hashes
		                if keep:
		                    out.append('"')
		                continue
		            if c == "\n":
		                out.append("\n")
		            elif keep:
		                out.append(c)
		            i += 1
		            continue
		    return "".join(out)


		for path, dest in pairs:
		    text = strip(open(path, encoding="utf-8").read())
		    with open(dest, "w", encoding="utf-8") as fh:
		        for lineno, line in enumerate(text.split("\n"), 1):
		            fh.write(f"{lineno}:{line}\n")
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

# Every Iceberg field name declared in the CATALOG zone's Python, as
# `<file>:<line>:<name>`. Two spellings, because both are used: pyiceberg's
# NestedField(id, "name", …) and pyarrow's pa.field("name", …).
#
# The corpus is named rather than `lake/src` wholesale, and that is the one
# place the re-cut had to touch this section: `pa.field` is a pyarrow spelling,
# and the tenant zone is pyarrow Parquet. A tenant-zone module under lake/ that
# declared `pa.field("database_id")` would be doing its job, and a guard that
# reddened on it would be a guard people start adding exceptions to.
iceberg_fields() { # iceberg_fields <file>...
	grep -HnoE 'NestedField\([0-9]+,[[:space:]]*"[^"]+"|pa\.field\([[:space:]]*"[^"]+"' \
		"$@" |
		sed -E 's/.*"([^"]+)"$/\0/' |
		sed -E 's/(NestedField\([0-9]+,[[:space:]]*|pa\.field\([[:space:]]*)"([^"]+)"/\2/'
}

# Every `pub fn` in a file whose return type is a key — `Result<String>` — as
# `<name>:<takes a database_id?>`. The tenant zone's inverted rule is about
# these and only these: a key builder that needs no tenant id builds a key
# that is not inside anybody's deletion prefix.
key_builders() { # key_builders <file>
	python3 - "$1" <<-'PY'
		import re, sys
		src = open(sys.argv[1], encoding="utf-8").read()
		# Signature lines only; a doc comment showing a `pub fn` is not one.
		src = "\n".join(
		    "" if re.match(r"\s*//", line) else line for line in src.split("\n")
		)
		for m in re.finditer(r"\bpub fn\s+(\w+)\s*\(", src):
		    body = src.find("{", m.end())
		    sig = src[m.start() : body if body != -1 else len(src)]
		    if "-> Result<String>" not in sig:
		        continue
		    print(f"{m.group(1)}:{'yes' if 'database_id' in sig else 'NO'}")
	PY
}

# The zone roots as the code declares them — TENANT_ROOT and every element of
# CATALOG_ROOTS — checked for containment. Prints `ok`, or the offending pair.
zone_roots() { # zone_roots <tenant.rs>
	python3 - "$1" <<-'PY'
		import re, sys
		src = open(sys.argv[1], encoding="utf-8").read()
		src = "\n".join("" if re.match(r"\s*//", l) else l for l in src.split("\n"))
		roots = []
		m = re.search(r'TENANT_ROOT:\s*&str\s*=\s*"([^"]*)"', src)
		if m:
		    roots.append(("TENANT_ROOT", m.group(1)))
		m = re.search(r"CATALOG_ROOTS:\s*&\[&str\]\s*=\s*&\[(.*?)\]", src, re.S)
		if m:
		    roots += [("CATALOG_ROOTS", v) for v in re.findall(r'"([^"]*)"', m.group(1))]
		if len(roots) < 2:
		    print(f"only found {roots}; the roots are not declared where this can read them")
		    raise SystemExit
		bad = [
		    f"{an}={a!r} contains {bn}={b!r}"
		    for an, a in roots
		    for bn, b in roots
		    if (an, a) != (bn, b) and b.startswith(a)
		]
		print("\n".join(bad) if bad else "ok")
	PY
}

# ── the corpora ─────────────────────────────────────────────────────────────
LAKE_CRATE="${REPO_DIR}/crates/pkdump-lake"
LAKEHOUSE_CRATE="${REPO_DIR}/crates/pkdump-lakehouse"
SHIP_CRATE="${REPO_DIR}/crates/pkdump-ship"
PYLAKE="${REPO_DIR}/lake/src/pkdump_lake"

# ── the zone split inside crates/pkdump-lake ────────────────────────────────
# The crate holds the layout of BOTH zones, so the file list is where the
# boundary is drawn. §12 asserts these three lists together cover src/*.rs
# exactly — a file in none of them fails the gate, which is what makes adding
# one a zone decision rather than an omission.
#
# CATALOG: `raw/` and the warehouse beside it. Cross-tenant, shared, forever.
CATALOG_ZONE_RS=(config.rs error.rs keys.rs manifest.rs reader.rs sink.rs store.rs)
# TENANT: `tenant/`. Tenant-keyed by construction, 90 days, its own credential.
TENANT_ZONE_RS=(tenant.rs)
# The crate root routes to both and resolves neither. It is its own class
# because it is the one file allowed to name the tenant zone's API — and still
# not allowed to look a tenant up.
ZONE_ROOT_RS=(lib.rs)

# ── the same split, on the Python side ──────────────────────────────────────
# Classified for the same reason and checked for totality the same way (§12).
# The hole this closes is not hypothetical: freshness.py landed after the
# original guard, and no section scanned it — it was in no list, so it could
# have named a tenant and nothing would have fired.
#
# The catalog write path. verify.py and value_snapshots.py are deliberately
# absent — they have their own sections.
WRITE_PATH_PY=(catalog.py prices.py raw.py roundtrip.py __init__.py)
# Catalog-zone modules that READ. Same rule as the write path — a reader that
# resolved a tenant would be reading one zone with the other's business.
CATALOG_READ_PY=(verify.py freshness.py)
# The one job that opens tenant databases on purpose (§7 is its rule).
TRANSFORM_PY=(value_snapshots.py)
# Tenant-zone Python: none today, and that is the honest entry. The tenant zone
# is written by crates/pkdump-ship and is plain Parquet, not Iceberg. A module
# added here is what would make §6's corpus need a carve-out, and §12 is what
# stops one arriving unclassified.
TENANT_ZONE_PY=()
# The only modules allowed to import sqlite3, and why.
SQLITE_ALLOWED=(value_snapshots.py verify.py)

# The crates that serve requests and record holdings changes. None of them may
# link a zone. pkdump-cli is deliberately NOT here: it is the one binary that
# is both `pkdump serve` and the offline landing commands, so it links
# pkdump-lake by construction. That boundary is a DEPLOYMENT one — the app
# container is given no lake credential — and it is asserted where it lives,
# in deploy/refresh.sh and tests/deploy/run.sh.
ONLINE_CRATES=(pkdump-db pkdump-server pkdump-keys)

rust_sources() { # rust_sources <crate-dir>
	find "$1/src" -name '*.rs' -type f 2>/dev/null | sort
}

# Grep a set of comment-stripped Rust sources, naming file and line.
scan() { # scan <regex> <file>...  [reads $1 as ERE]
	local rx="$1" f hits out=""
	shift
	for f in "$@"; do
		[[ -f "$f" ]] || continue
		rust_code "$f" >/dev/null # prime, if this file was not primed above
		hits="$(grep -inE "$rx" "$(cache_path "$f" code)" | sed "s|^|${f#"$REPO_DIR"/}:|")"
		[[ -n "$hits" ]] && out+="${hits}"$'\n'
	done
	printf '%s' "${out%$'\n'}"
}

# The same, with string literals left in — for the questions that are about
# what a key says rather than what a symbol is called.
scan_strings() { # scan_strings <regex> <file>...
	local rx="$1" f hits out=""
	shift
	for f in "$@"; do
		[[ -f "$f" ]] || continue
		rust_code "$f" keep-strings >/dev/null
		hits="$(grep -inE "$rx" "$(cache_path "$f" keep-strings)" | sed "s|^|${f#"$REPO_DIR"/}:|")"
		[[ -n "$hits" ]] && out+="${hits}"$'\n'
	done
	printf '%s' "${out%$'\n'}"
}

zone_files() { # zone_files <name-of-array>
	local -n names="$1"
	local n
	for n in "${names[@]}"; do printf '%s\n' "${LAKE_CRATE}/src/${n}"; done
}

mapfile -t CATALOG_RS_FILES < <(zone_files CATALOG_ZONE_RS)
mapfile -t TENANT_RS_FILES < <(zone_files TENANT_ZONE_RS)
mapfile -t ROOT_RS_FILES < <(zone_files ZONE_ROOT_RS)
mapfile -t LAKEHOUSE_RS_FILES < <(rust_sources "$LAKEHOUSE_CRATE")
mapfile -t SHIP_RS_FILES < <(rust_sources "$SHIP_CRATE")

# One interpreter for each mode, before any section runs. Correctness does not
# depend on this — rust_code strips on demand for anything not primed — only
# the runtime does.
prime_strip_cache code \
	"${CATALOG_RS_FILES[@]}" "${TENANT_RS_FILES[@]}" "${ROOT_RS_FILES[@]}" \
	"${LAKEHOUSE_RS_FILES[@]}" "${SHIP_RS_FILES[@]}"
prime_strip_cache keep-strings "${TENANT_RS_FILES[@]}" "${SHIP_RS_FILES[@]}"

# ════════════════════════════════════════════════════════════════════════════
#   PART A — THE CATALOG ZONE.  `raw/`, `lake/`: no tenant, anywhere, ever.
# ════════════════════════════════════════════════════════════════════════════

log "1. the lake crate cannot open a database at all (structural)"
# pkdump-lake writes raw/ and says where tenant/ is. It links no SQLite: not
# rusqlite, not pkdump-db, not libsqlite3-sys. That is why "no lake write path
# opens a tenant database" is true of it by construction rather than by
# discipline — and adding one of these to its Cargo.toml is the code change
# that has to show up in review, exactly like adding a variant to Source.
#
# The re-cut made this stronger rather than weaker. The tenant zone's LAYOUT
# lives in this crate; its WRITER is crates/pkdump-ship, a separate crate, and
# the reason is exactly this line: filling a tenant part means reading a
# tenant's SQLite, and a crate that links no SQLite cannot be asked to.
LAKE_TOML="${LAKE_CRATE}/Cargo.toml"
check "crates/pkdump-lake/Cargo.toml exists" "yes" \
	"$([[ -f "$LAKE_TOML" ]] && echo yes || echo no)"
DBDEPS="$(grep -nE '^[[:space:]]*(rusqlite|pkdump-db|libsqlite3-sys|sqlx|diesel)[[:space:]]*[.=]' \
	"$LAKE_TOML")"
none "it depends on no SQLite crate and not pkdump-db" "$DBDEPS"

log "2. no catalog-zone module names a tenant"
# And nothing in the catalog zone's source names one either — a path dependency
# added under a different key, or a re-export reached through another crate,
# would not show up above. This is the original §1 grep, over the catalog zone's
# files rather than the whole crate.
none "no catalog-zone source names a tenant or a SQLite handle" \
	"$(scan "${IDENT_RX}|rusqlite|pkdump_db" "${CATALOG_RS_FILES[@]}")"

log "3. the offline derive calls no tenant-resolving API (pd-1uem's crate)"
# pkdump-lakehouse cannot get the structural guard: it derives shared.sqlite, so
# it links pkdump-db and rusqlite legitimately. What it must never touch is the
# tenant-resolving half of that API — pkdump_db::tenants::*, ::registry::*,
# tenant_db_file/tenant_db_path*, tenants_dir. It opens exactly one thing,
# `pkdump_db::open_shared(shared_db_path())`, plus the two user-named paths the
# `diff` subcommand compares.
none "no pkdump-lakehouse source names a tenant or the registry" \
	"$(scan "${IDENT_RX}|tenant_db_file|tenant_db_path|tenants_dir" "${LAKEHOUSE_RS_FILES[@]}")"

log "4. the Python write path cannot open a database at all (structural)"
# Same argument as §1, one tier up: a module that never imports sqlite3 cannot
# open a tenant database however it assembles the path. This is what makes the
# string-stripping in §5's grep safe.
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

log "5. the modules that may open a database are exactly the allowlist"
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
PY_READ=""
for m in "${CATALOG_READ_PY[@]}"; do
	f="${PYLAKE}/${m}"
	[[ -f "$f" ]] || continue
	hits="$(py_code "$f" | grep -inE "${IDENT_RX}" |
		sed "s|^|lake/src/pkdump_lake/${m}:|")"
	[[ -n "$hits" ]] && PY_READ+="${hits}"$'\n'
done
none "no catalog-zone reader reaches the registry or the tenants dir" "${PY_READ%$'\n'}"

log "6. no Iceberg table carries a tenant-identifying column"
# The literal first half of assertion 3. catalog.prices is five fields —
# tcgplayer_product_id, sub_type_name, price_type, price, observed_date — and
# catalog.sealed_prices (pd-bbv7) is the same four without the sub-type. None
# of them says which collection the row belongs to, because none of them can:
# the catalog zone holds catalog data only, and a SEALED price is a catalog
# fact about a product exactly as a card price is — what a tenant OWNS lives in
# the tenant zone and is valued against these.
#
# Unchanged by the re-cut, and it must stay that way: the tenant zone is plain
# partitioned Parquet, NOT Iceberg (crates/pkdump-lake/src/tenant.rs), so every
# Iceberg schema in the tree is a catalog schema and this corpus needs no
# carve-out. §11 is what notices if that ever stops being true.
CATALOG_PY_FILES=()
for m in "${WRITE_PATH_PY[@]}" "${CATALOG_READ_PY[@]}" "${TRANSFORM_PY[@]}"; do
	[[ -f "${PYLAKE}/${m}" ]] && CATALOG_PY_FILES+=("${PYLAKE}/${m}")
done
FIELDS="$(iceberg_fields "${CATALOG_PY_FILES[@]}")"
BAD_FIELDS="$(grep -iE ":(${FIELD_RX})$|:[a-z0-9_]*(${FIELD_RX})[a-z0-9_]*$" <<<"$FIELDS")"
none "every Iceberg schema field is catalog-scoped" "$BAD_FIELDS"

log "7. the transform tier reads the lake and never writes to it"
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

# ════════════════════════════════════════════════════════════════════════════
#   PART B — THE TENANT ZONE.  `tenant/`: tenant-keyed on purpose, contained.
# ════════════════════════════════════════════════════════════════════════════

log "8. the tenant zone is keyed by tenant, and resolves none (pd-uz8q)"
# The carve-out, and the reason it is a carve-out rather than an exemption:
# the tenant zone has rules of its own and they are the INVERSE of the catalog's.
#
# It must be tenant-keyed. `database_id` is the FIRST partition component so
# that one prefix covers a tenant's holdings and their valuations together —
# that prefix is the unit a deletion drops, so a key builder that does not take
# a database_id builds an object no deletion would find.
BUILDERS="$(key_builders "${LAKE_CRATE}/src/tenant.rs")"
some "the tenant zone declares key builders at all" "$BUILDERS"
none "every tenant-zone key builder takes a database_id" \
	"$(grep -v ':yes$' <<<"$BUILDERS" | sed 's|^|crates/pkdump-lake/src/tenant.rs: |')"

# What it may NOT do is look a tenant up. It is handed an id by a caller that
# got it from the registry; consulting the registry itself, or a tenants
# directory, or PKDUMP_USER, would make the zone an authority on identity
# instead of a partitioning of it — and it links no SQLite (§1) precisely so
# that this is a grep about intent rather than a hole.
none "the tenant zone resolves no tenant identity" \
	"$(scan "${RESOLVE_RX}" "${TENANT_RS_FILES[@]}")"

# The crate root routes to both zones — `pub mod tenant`, `open_tenant_zone` —
# and that is all it may do with the tenant's name. Same rule, same pattern.
none "the crate root routes to the tenant zone without resolving one" \
	"$(scan "${RESOLVE_RX}" "${ROOT_RS_FILES[@]}")"

# The two zones' roots have to be disjoint, or every containment assertion in
# this file is about a boundary that does not exist. Containment, not equality:
# a `tenant/` moved under `lake/` would be inside the catalog's own prefix,
# governed by the catalog's indefinite-retention lifecycle rule, and reachable
# by any credential granted the catalog — while every line of code and every
# policy document still said the two zones were separate.
ROOT_OVERLAP="$(zone_roots "${LAKE_CRATE}/src/tenant.rs")"
check "no zone root contains another" "ok" "$ROOT_OVERLAP"

log "9. the shipper writes the tenant zone and reaches no catalog prefix (pd-dxn3)"
# crates/pkdump-ship is the only thing in the workspace that writes under
# `tenant/`. It opens a tenant database on purpose — that is the outbox it
# reads — so §1's structural argument is unavailable and deliberately so. Its
# rule is containment: everything it writes lands in the tenant zone.
#
# Strings are KEPT here. An object key is a string literal, so "does the
# shipper name a catalog prefix" is a question the identifier scan cannot ask.
none "no catalog-zone prefix appears in the shipper" \
	"$(scan_strings '"(raw|lake)/' "${SHIP_RS_FILES[@]}")"
# Nor may it reach the catalog through the API. pkdump_lake::open/open_reader
# hand out the catalog's landing zone under the catalog's ambient credentials;
# the tenant zone has its own entry points because it has its own identity.
none "the shipper calls no catalog-zone entry point" \
	"$(scan 'pkdump_lake::(open|open_reader|keys|sink|manifest|reader)\b|RawLanding|RawZone' \
		"${SHIP_RS_FILES[@]}")"
# And it must not name the catalog's credential. One profile for both zones is
# not a narrow policy, it is no boundary at all (crates/pkdump-lake/src/
# tenant.rs), and the shipper is the process holding a tenant key.
none "the shipper names no catalog credential" \
	"$(scan_strings 'AWS_PROFILE|KEY_CATALOG_PROFILE' "${SHIP_RS_FILES[@]}")"
# The other direction, so none of the three above is passing because the
# shipper does nothing: it does reach the tenant zone, through the entry points
# that carry the tenant credential, and it builds its keys from the layout
# module rather than by hand.
some "the shipper opens the tenant zone through its own entry point" \
	"$(scan 'open_tenant_zone' "${SHIP_RS_FILES[@]}")"
some "and builds its object keys from the tenant-zone layout module" \
	"$(scan 'pkdump_lake::(range_part_key|part_key|tenant_prefix|partition_prefix)' \
		"${SHIP_RS_FILES[@]}")"

log "10. the online path cannot reach either zone (structural)"
# Item 1's outbox (pd-5m54) is a table in the tenant's OWN database, written by
# triggers inside the mutation's transaction. That is the whole inbound leg's
# first premise: holdings leave a collection through the outbox, and the
# offline side is fed from there — never by the request-serving process
# writing to a bucket. So the crates that serve requests and record events link
# no lake crate and no S3 SDK, which makes a dual write impossible rather than
# merely absent, and keeps the standing rule mechanical: nothing that serves a
# request may hold a lake credential.
ONLINE_DEPS=""
ONLINE_FOUND=0
for c in "${ONLINE_CRATES[@]}"; do
	toml="${REPO_DIR}/crates/${c}/Cargo.toml"
	[[ -f "$toml" ]] || continue
	ONLINE_FOUND=$((ONLINE_FOUND + 1))
	hits="$(grep -nE '^[[:space:]]*(pkdump-lake|pkdump-lakehouse|pkdump-ship|aws-sdk-s3|aws-config)[[:space:]]*[.=]' \
		"$toml" | sed "s|^|crates/${c}/Cargo.toml:|")"
	[[ -n "$hits" ]] && ONLINE_DEPS+="${hits}"$'\n'
done
check "every online crate was found to check" "${#ONLINE_CRATES[@]}" "$ONLINE_FOUND"
none "no online crate depends on a lake crate or the S3 SDK" "${ONLINE_DEPS%$'\n'}"

log "11. the two zones do not reach each other"
# The boundary, in both directions, at the source level. §6 holds while the
# tenant zone is plain Parquet; the moment catalog-zone code could build a
# tenant key, "every Iceberg field is catalog-scoped" would stop being a
# statement about where tenant data can be.
none "no catalog-zone module reaches the tenant zone" \
	"$(scan 'tenant::|TENANT_ROOT|TenantZone|open_tenant_zone|TenantDataset' \
		"${CATALOG_RS_FILES[@]}")"
# And the mirror: the tenant zone names the catalog's roots — it has to, they
# are the boundary it is defined against — but it must not USE the catalog's
# key builders or landing sink. Naming a boundary is not crossing it.
none "the tenant zone builds no catalog key" \
	"$(scan 'crate::(keys|sink|manifest|reader)\b|RawLanding|Source::' \
		"${TENANT_RS_FILES[@]}")"
# The offline derive writes the catalog and only the catalog. §3 already
# refuses `tenant` there as an identifier; this says the same thing about the
# zone's API, so a rename that got past IDENT_RX would still be caught.
none "the offline derive reaches no tenant-zone entry point" \
	"$(scan 'open_tenant_zone|TENANT_ROOT|TenantZone|range_part_key' "${LAKEHOUSE_RS_FILES[@]}")"

# ════════════════════════════════════════════════════════════════════════════
#   PART C — the guard read something.
# ════════════════════════════════════════════════════════════════════════════

log "12. the guard read something (fail closed)"
# Every section above passes vacuously if its corpus is empty — a renamed crate
# directory or a moved python package turns this whole file green while
# asserting nothing. Assert the inputs were real.
#
# This section is why the zone split is three lists that must COVER the
# directory rather than one list of exceptions: a file added to
# crates/pkdump-lake/src and classified nowhere is checked by nothing, and the
# only symptom would be a section quietly scanning one file fewer.
check "pkdump-lake has Rust sources" "yes" \
	"$([[ -n "$(rust_sources "$LAKE_CRATE")" ]] && echo yes || echo no)"
check "pkdump-lakehouse has Rust sources" "yes" \
	"$([[ ${#LAKEHOUSE_RS_FILES[@]} -gt 0 ]] && echo yes || echo no)"
check "pkdump-ship has Rust sources" "yes" \
	"$([[ ${#SHIP_RS_FILES[@]} -gt 0 ]] && echo yes || echo no)"

# The classification is total: every .rs in the lake crate's src is in exactly
# one zone list, and every file in a zone list exists.
ACTUAL_RS="$(find "${LAKE_CRATE}/src" -maxdepth 1 -name '*.rs' -type f 2>/dev/null |
	xargs -r -n1 basename | sort | tr '\n' ' ')"
CLASSIFIED_RS="$(printf '%s\n' "${CATALOG_ZONE_RS[@]}" "${TENANT_ZONE_RS[@]}" "${ZONE_ROOT_RS[@]}" |
	sort | tr '\n' ' ')"
check "every lake-crate source is classified into exactly one zone" \
	"$ACTUAL_RS" "$CLASSIFIED_RS"
# A subdirectory would escape the -maxdepth above and be classified by nobody.
none "the lake crate's sources are all at the top of src/" \
	"$(find "${LAKE_CRATE}/src" -mindepth 2 -name '*.rs' -type f 2>/dev/null |
		sed "s|^${REPO_DIR}/||")"

FOUND_PY=0
for m in "${WRITE_PATH_PY[@]}"; do [[ -f "${PYLAKE}/${m}" ]] && FOUND_PY=$((FOUND_PY + 1)); done
check "every write-path python module was found" "${#WRITE_PATH_PY[@]}" "$FOUND_PY"

# The Python side of the same totality argument, and it closes a hole rather
# than guarding against a future one: freshness.py landed after this gate did,
# in none of the lists, scanned by no section. A module nobody classified is a
# module nobody checks, and the only symptom is a corpus one file short.
ACTUAL_PY="$(find "$PYLAKE" -maxdepth 1 -name '*.py' -type f 2>/dev/null |
	xargs -r -n1 basename | sort | tr '\n' ' ')"
CLASSIFIED_PY="$(printf '%s\n' "${WRITE_PATH_PY[@]}" "${CATALOG_READ_PY[@]}" \
	"${TRANSFORM_PY[@]}" ${TENANT_ZONE_PY+"${TENANT_ZONE_PY[@]}"} |
	sort | tr '\n' ' ')"
check "every lake python module is classified into exactly one zone" \
	"$ACTUAL_PY" "$CLASSIFIED_PY"
check "Iceberg schema fields were found to check" "yes" \
	"$([[ -n "$FIELDS" ]] && echo yes || echo no)"
# The parsers themselves: if python3 stopped emitting anything, §1-§5 go green
# by producing no lines to grep.
check "the Rust comment stripper produced output" "yes" \
	"$([[ -n "$(rust_code "${LAKE_CRATE}/src/keys.rs")" ]] && echo yes || echo no)"
KEPT="$(rust_code "${LAKE_CRATE}/src/tenant.rs" keep-strings)"
check "the Rust stripper keeps strings when asked" "yes" \
	"$([ "$(grep -cE '"tenant/"' <<<"$KEPT" || true)" != 0 ] && echo yes || echo no)"
check "the zone roots were found to compare" "yes" \
	"$([[ "$ROOT_OVERLAP" == ok || "$ROOT_OVERLAP" == *contains* ]] && echo yes || echo no)"
check "the Python tokenizer produced output" "yes" \
	"$([[ -n "$(py_code "${PYLAKE}/prices.py")" ]] && echo yes || echo no)"
check "the key-builder parser produced output" "yes" \
	"$([[ -n "$BUILDERS" ]] && echo yes || echo no)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — the catalog zone is keyed by no tenant, the tenant zone is keyed"
echo "         by nothing else, neither reaches the other, and the online path"
echo "         reaches neither."
