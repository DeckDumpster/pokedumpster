#!/usr/bin/env bash
# Container-tier gate (pd-hkbc, pd-lunn): `pkdump data refresh` LANDS, and
# writes NOTHING — not one byte of any tenant database, and since pd-lunn not
# one byte of the shared catalog either.
#
# §9 (pd-nons, pd-llbq) is the second thing it gates, and it is here rather than
# in its own script because it needs exactly this scaffolding — the shipped
# image, a fixture upstream, and a data directory with real tenants in it — and
# because its own claim ends with "and it still wrote nothing".
#
# §9 lands into a landing zone OF ITS OWN, separate from the one the ordinary
# refreshes above write into. Not fastidiousness: its assertions are about the
# partition ONE night left behind — one run, one manifest per dataset — and a
# zone the healthy runs had already landed into would answer every one of them
# about the wrong run. What it is looking at is the shape a dead tail leaves:
# `finalize` computes `complete` PER DATASET and `acquire` deliberately does not
# hand it the tail's error, so the night is supposed to come out honest —
# `pokemontcgio/sets` short, `tcgcsv/*` whole. That asymmetry is the input
# `pkdump-lake-derive` reads to decide whether a date is a partial night
# (exit 2) or an underivable one (exit 1), and it went unexercised for long
# enough that the two units ended up answering the same night in opposite ways
# (pd-llbq). The hermetic half of that claim is
# crates/pkdump-derive/tests/tail_failure.rs; this is the shipped binary making
# the same partition.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/refresh/tenant_bytes.sh          # ~1min after the image is warm
#   KEEP=1 bash tests/refresh/tenant_bytes.sh   # leave WORK in place
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# The refresh used to end with a step 7 that snapshotted today's collection
# value into the ONE database `$PKDUMP_USER` resolves to. No loop. Every other
# registered tenant got no value history, ever, and the run reported success
# (pd-s5yn) — latent only because prod happens to have one tenant. The fix was
# not to add a loop: `lake/src/pkdump_lake/value_snapshots.py` owns that job now
# and walks the registry (pd-ruwh), and the refresh gave up tenant writes
# entirely.
#
# "Gave up tenant writes entirely" is a claim about a whole binary, and the only
# honest way to check it is to run the real refresh over a data directory that
# HAS tenant databases and see whether they moved. pd-s0i4 wanted this assertion
# and could not have it: with step 7 in place it fails, which is the point.
#
# ── AND WHAT pd-lunn ADDED TO IT ────────────────────────────────────────────
# Item 6 of the lake epic deleted the inline derivation too, so the refresh now
# writes no CATALOG table either. That turns §5's assertion inside out — the
# shared catalog used to have to CHANGE for this gate to mean anything, and now
# it has to be byte-identical — and it is the acceptance criterion for pd-lunn
# checked against the shipped binary rather than against a function.
#
# The claim is enforced one level in, at the connection: the command opens the
# catalog with `open_shared_readonly`, so a write is an error rather than a
# review note. This gate is what says the shipped binary really does that.
#
# ── WHY THE UPSTREAM IS A FIXTURE ───────────────────────────────────────────
# The assertion only means something if the refresh RUNS TO THE END — a refresh
# that dies at the first fetch would pass this gate with step 7 still in it. So
# it has to complete, and reaching the real tcgcsv.com to get there would walk
# ~180 English and ~450 Japanese groups at two requests each and make a CI gate
# depend on somebody else's uptime.
#
# tests/refresh/upstream.py publishes NOTHING instead: no sets, no groups. Every
# phase still runs and the run reaches the end, which is the only part this gate
# is about. §5 asserts it really got there rather than exiting early.
#
# `PKDUMP_TCGCSV_BASE_URL` / `PKDUMP_POKEMONTCG_BASE_URL` are how it is pointed
# there; see crates/pkdump-ingest/src/upstream.rs for why that seam exists and
# why an override announces itself.
#
# ── AND WHY THERE IS A LANDING ZONE ─────────────────────────────────────────
# Landing is not opt-in any more (pd-lunn): a refresh that lands nothing does
# nothing, so the command requires one. It is a DIRECTORY on the host, mounted
# at /lake — deliberately not inside the data directory, because §6 asserts that
# directory gains no new files and a landing zone under it would be a stream of
# them.
#
# Prod-safe: its own image tag, its own temp directory, its own host port from
# the kernel. It touches no pkdump-* unit, no pkdump-*-data volume, no bucket,
# and no real collection — the data directory is built from the committed
# fixtures on every run.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES="${REPO_DIR}/tests/ui/fixtures"

# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

# The shipped image, built here or — when deploy/ci.sh already built it once for
# every gate in the run — tagged from that one. See deploy/image-lib.sh.
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"

# PER-CHECKOUT: several polecats run deploy/ci.sh concurrently from their own
# worktrees, and a fixed image tag means one run's teardown pulls the image out
# from under another.
SUFFIX="${PDREF_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
IMAGE="localhost/pkdump:refresh-${SUFFIX}"

# Taken from the kernel, never picked — tests/lib/ports.sh explains what a
# picked one costs, and tests/lib/ports_test.sh fails the build over a relapse.
UPSTREAM_PORT=${UPSTREAM_PORT:-$(free_port)}

WORK=${WORK:-$(mktemp -d /tmp/pd-refresh.XXXXXX)}
DATA="$WORK/data"
# The landing zone every ordinary refresh writes into, OUTSIDE the data
# directory — see the header.
LAKE="$WORK/lake"
# §9's own, for the same reason and one more: its claims are about the partition
# a single night left behind, so it may not share a zone with the runs above.
RAW="$WORK/raw-zone"
UPSTREAM_LOG="$WORK/upstream.jsonl"
UPSTREAM_PID=""

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	[[ -n "$UPSTREAM_PID" ]] && kill "$UPSTREAM_PID" 2>/dev/null
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving WORK=$WORK in place."
		return
	fi
	podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
	rm -rf "$WORK"
}
trap cleanup EXIT

# A one-off `pkdump` against the data directory, through the SHIPPED image —
# the same way deploy/TENANTS.md and pkdump-refresh@.service invoke it. Running
# a `cargo run` binary instead would not be the thing that ships.
pkdump() { podman run --rm -v "${DATA}:/data:Z" --entrypoint pkdump "$IMAGE" "$@"; }

# The refresh, pointed at the fixture upstream. `host.containers.internal` is
# how a rootless container reaches a listener on the host (the pattern
# tests/alarming/run.sh uses for its sink).
refresh() {
	podman run --rm -v "${DATA}:/data:Z" -v "${LAKE}:/lake:Z" \
		-e PKDUMP_TCGCSV_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/tcgplayer" \
		-e PKDUMP_POKEMONTCG_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/v2" \
		-e PKDUMP_LAKE_DIR=/lake \
		--entrypoint pkdump "$IMAGE" data refresh
}

# Every landed part in a zone, so "it fetched and threw the bytes away" is not a
# way to pass this gate. §9 lands into its own, so the zone is an argument.
# `|| true` on the find, not just 2>/dev/null: this runs under `set -o
# pipefail`, so a zone that does not exist yet would make the whole pipeline
# exit 1 and abort the gate rather than report "nothing landed there".
landed_parts() { { find "${1:-$LAKE}" -name 'part-*' 2>/dev/null || true; } | wc -l; }

# Every byte under tenants/, keyed by path. WAL and shared-memory sidecars are
# deliberately included: a refresh that merely OPENS a tenant database in WAL
# mode creates a `-wal` beside it, and that is a tenant byte too.
tenant_fingerprint() {
	(cd "$DATA" && find tenants -type f -printf '%p\n' | sort |
		xargs -r -d '\n' sha256sum) 2>/dev/null
}

# Everything else in the data directory, so a stray write somewhere new — a
# registry row, a fresh tenants/ entry, a snapshot file — cannot hide.
#
# The catalog's OWN `-wal` / `-shm` sidecars are excluded, and that is a fact
# about SQLite rather than a hole: opening a WAL database read-only still
# creates the wal-index beside it, and a read-only connection cannot clean it up
# on close. Measured, not assumed:
#
#   $ sqlite3 "file:t.sqlite?mode=ro" 'SELECT 1'   # -> t.sqlite-shm, t.sqlite-wal
#
# What matters is that the catalog's CONTENT did not move, and §5b hashes
# shared.sqlite itself for that. Nothing else is excluded — a tenant sidecar is
# still a tenant byte, and `tenant_fingerprint` above counts it as one.
data_inventory() {
	# `|| true` on the filter: grep exits 1 when it selects nothing, and this
	# runs under `set -e` inside a command substitution, where that would abort
	# the gate rather than report an empty directory.
	(cd "$DATA" && find . -type f -printf '%P\n' |
		{ grep -v '^shared\.sqlite-\(wal\|shm\)$' || true; } | sort)
}

log "1. the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"

log "2. a data directory with real tenant databases in it"
mkdir -p "$DATA" "$LAKE"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
# The symbol glyphs are pre-normalized so the symbols phase has nothing to
# fetch. It reaches images.pokemontcg.io, which is deliberately outside the
# landing zone and outside this fixture; the http-prefix gate in
# normalize_all_symbols skips a row that already points at /sym/<set>.png.
# Nothing about the tenant claim depends on card art.
sqlite3 "${DATA}/shared.sqlite" \
	"UPDATE sets SET symbol_url = '/sym/' || set_code || '.png' WHERE symbol_url LIKE 'http%';"

# Converge the fixture's schema, the way `pkdump setup` or the nightly derive
# would. `open_shared` re-applies schema_shared.sql on every read-WRITE open, so
# any of those commands heals a catalog that predates a table; the committed
# fixture predates `catalog_price_overrides` and `raw_derivation`.
#
# It is here, explicitly, because the refresh used to do it as a side effect and
# no longer can (pd-lunn): it opens the catalog READ-ONLY, which is the whole
# point. A gate whose scaffolding depended on the thing under test healing it
# would be measuring the wrong binary — and §8's backfill, which ATTACHes the
# catalog read-only, fails outright on the un-converged fixture.
#
# `apply-corrections --dry-run` is the cheapest command that opens the catalog
# read-write: it applies the schema and the seeds, reports, and writes no row.
pkdump data apply-corrections --db /data/shared.sqlite --dry-run >/dev/null
check "the fixture catalog carries the current schema" "yes" \
	"$(sqlite3 "file:${DATA}/shared.sqlite?mode=ro" \
		"SELECT COUNT(*) FROM sqlite_master WHERE name IN ('raw_derivation','catalog_price_overrides')" |
		grep -qx 2 && echo yes || echo no)"

# TWO tenants, provisioned by the REAL `pkdump tenant create` so the registry
# and the opaque-id layout are prod's, not a hand-built approximation. One of
# them is `collection` — the handle $PKDUMP_USER defaults to, and therefore the
# exact database step 7 used to write. The other is the tenant step 7 never
# knew existed, which is the bug (pd-s5yn) in one line.
pkdump tenant create collection >/dev/null
pkdump tenant create alice >/dev/null
TENANT_FILES=$(find "${DATA}/tenants" -name '*.sqlite' | wc -l)
check "two tenants provisioned" "2" "$TENANT_FILES"

# Give each one a real collection. An empty database would let "the refresh
# wrote nothing" pass for the wrong reason: there would be nothing to write.
for db in "${DATA}"/tenants/*.sqlite; do
	cp "${FIXTURES}/collection.sqlite" "$db"
done
# Any WAL or shared-memory sidecar `tenant create` left behind belongs to the
# database that was just overwritten, not to the fixture now sitting there.
# SQLite would try to replay it. It also has to go before the fingerprint, or
# the first open of the run would remove it and read as a tenant write.
rm -f "${DATA}"/tenants/*.sqlite-wal "${DATA}"/tenants/*.sqlite-shm
SEED_ROWS=$(sqlite3 "file:$(find "${DATA}/tenants" -name '*.sqlite' | head -1)?mode=ro" \
	'SELECT count(*) FROM collection;')
if [[ "${SEED_ROWS:-0}" -lt 2 ]]; then
	echo "  ABORT: the fixture collection has ${SEED_ROWS:-0} rows — with nothing in"
	echo "         it, the deleted step 7 would have written nothing either and this"
	echo "         gate would pass on a binary that still has it."
	exit 1
fi
echo "  ${SEED_ROWS} collection rows in each tenant"

log "3. the fixture upstream"
: >"$UPSTREAM_LOG"
python3 "${SCRIPT_DIR}/upstream.py" "$UPSTREAM_PORT" "$UPSTREAM_LOG" >"${WORK}/upstream.err" 2>&1 &
UPSTREAM_PID=$!
for _ in $(seq 40); do
	curl -fsS -m 2 "http://127.0.0.1:${UPSTREAM_PORT}/ready" >/dev/null 2>&1 && break
	sleep 0.25
done
check "the fixture upstream is answering" "yes" \
	"$([[ "$(grep -c '"path": "/ready"' "$UPSTREAM_LOG" || true)" -gt 0 ]] && echo yes || echo no)"

log "4. fingerprint every tenant byte, then refresh"
BEFORE_TENANTS=$(tenant_fingerprint)
BEFORE_INVENTORY=$(data_inventory)
BEFORE_SHARED=$(sha256sum "${DATA}/shared.sqlite" | cut -d' ' -f1)

REFRESH_RC=0
refresh >"${WORK}/refresh.log" 2>&1 || REFRESH_RC=$?
if [[ "$REFRESH_RC" -ne 0 ]]; then
	echo "  ABORT: the refresh exited ${REFRESH_RC}. Every assertion below is about"
	echo "         what a COMPLETED refresh wrote; a run that died early would"
	echo "         report clean tenant bytes and prove nothing."
	echo "  --- refresh log ---"
	sed 's/^/  /' "${WORK}/refresh.log"
	echo "  --- what the fixture upstream was asked for ---"
	sed 's/^/  /' "$UPSTREAM_LOG"
	exit 1
fi

# It reached the end rather than exiting quietly partway. The last line the
# refresh prints is its own completion.
check "the refresh ran to completion" "1" \
	"$(grep -c '^Refresh complete: landed, not derived' "${WORK}/refresh.log" || true)"

log "5. and it really did the work, rather than skipping to the end"
# A refresh that no-ops everything would also leave the tenants alone. These
# are the phases the command has left, each of which must have run for §6 to
# mean "the refresh does its job without touching anything" rather than "the
# refresh does nothing".
# Matched as FIXED strings, not patterns.
for phase in \
	"Landing raw upstream responses in " \
	"Landing the newest sets from pokemontcg.io" \
	"Landing TCGCSV groups, products, prices" \
	"Landing the Pokémon Japan catalog"; do
	check "phase ran: ${phase}" "yes" \
		"$(grep -qF "$phase" "${WORK}/refresh.log" && echo yes || echo no)"
done
# It opened the catalog read-only and said so, which is the mechanism the whole
# claim below rests on.
check "the catalog was opened READ-ONLY" "1" \
	"$(grep -c '^Opening shared catalog READ-ONLY at /data/shared.sqlite$' "${WORK}/refresh.log" || true)"
# And the bytes really left the process. A run that fetched and dropped
# everything on the floor would satisfy every other assertion here.
check "responses were landed" "yes" \
	"$([[ "$(landed_parts)" -gt 0 ]] && echo yes || echo "no ($(landed_parts))")"
check "…under a manifest that says the run completed" "yes" \
	"$(grep -rl '"complete": true' "$LAKE" >/dev/null 2>&1 && echo yes || echo no)"

log "5b. and the SHARED CATALOG did not move either (pd-lunn)"
# The acceptance criterion for item 6, against the shipped binary. This
# assertion is the inverse of the one that stood here until pd-lunn — the
# refresh used to have to WRITE the catalog for this gate to mean anything, and
# it must now leave it alone. A binary that still derived inline fails here.
check "shared.sqlite is byte-identical" "unchanged" \
	"$([[ "$(sha256sum "${DATA}/shared.sqlite" | cut -d' ' -f1)" == "$BEFORE_SHARED" ]] &&
		echo unchanged || echo CHANGED)"
check "…and nothing was derived into it" "0" \
	"$(sqlite3 "file:${DATA}/shared.sqlite?mode=ro" 'SELECT COUNT(*) FROM raw_derivation' 2>/dev/null || echo ERR)"

log "6. NOT ONE BYTE of any tenant database moved either"
# The assertion the bead exists for. With step 7 in place this diff shows every
# tenant `collection` resolves to, changed by the snapshot rows it wrote.
if diff <(echo "$BEFORE_TENANTS") <(tenant_fingerprint) >"${WORK}/tenants.diff" 2>&1; then
	check "every tenant database is byte-identical" "identical" "identical"
else
	check "every tenant database is byte-identical" "identical" "CHANGED"
	echo "  --- what moved ---"
	sed 's/^/  /' "${WORK}/tenants.diff"
	echo "  This is pd-s5yn's step 7: the catalog refresh wrote a collection."
fi

# Nothing appeared anywhere else either — no new tenant file, no sidecar, no
# registry row written by a job that has no business provisioning one. The
# landing zone is mounted OUTSIDE this directory precisely so that landing —
# which is now the job's whole output — does not have to be excluded from here.
if diff <(echo "$BEFORE_INVENTORY") <(data_inventory) >"${WORK}/inventory.diff" 2>&1; then
	check "and the data directory gained no new files" "same" "same"
else
	check "and the data directory gained no new files" "same" "DIFFERENT"
	sed 's/^/  /' "${WORK}/inventory.diff"
fi

log "7. a second refresh is the same story"
# Once could be an accident of ordering. A second run lands into the SAME
# ingest_date under a new run ULID — which is what a retry after a partial
# night does in production — and must still leave every byte on disk alone.
SECOND_RC=0
refresh >"${WORK}/refresh2.log" 2>&1 || SECOND_RC=$?
check "the second refresh succeeded" "0" "$SECOND_RC"
check "shared.sqlite is still byte-identical" "unchanged" \
	"$([[ "$(sha256sum "${DATA}/shared.sqlite" | cut -d' ' -f1)" == "$BEFORE_SHARED" ]] &&
		echo unchanged || echo CHANGED)"
if diff <(echo "$BEFORE_TENANTS") <(tenant_fingerprint) >"${WORK}/tenants2.diff" 2>&1; then
	check "tenants still byte-identical after two refreshes" "identical" "identical"
else
	check "tenants still byte-identical after two refreshes" "identical" "CHANGED"
	sed 's/^/  /' "${WORK}/tenants2.diff"
fi

# The capability itself did not go away with the call site — the transform tier
# still needs it, and tests/lake/value_snapshots.sh diffs against it.
log "8. the snapshot capability still exists, just not inside the refresh"
BACKFILL_RC=0
pkdump data backfill-value-history >"${WORK}/backfill.log" 2>&1 || BACKFILL_RC=$?
check "pkdump data backfill-value-history still runs" "0" "$BACKFILL_RC"
if [[ "$BACKFILL_RC" -ne 0 ]]; then
	sed 's/^/  /' "${WORK}/backfill.log"
fi

# Step 8 legitimately writes tenant value-history rows — that is its entire
# job. §9 below asks a narrower question ("did the PARTIAL REFRESH itself
# touch a tenant"), so its baseline has to be taken here, after backfill, not
# reused from BEFORE_TENANTS at step 4 — reusing that stale snapshot would
# blame the refresh for bytes backfill already legitimately changed.
AFTER_BACKFILL_TENANTS=$(tenant_fingerprint)

log "9. a dead pokemontcg.io no longer ends the run (pd-nons)"
# On 2026-08-11 api.pokemontcg.io answered 5xx to ~45% of requests. The tail's
# error propagated, so one bad response ended the refresh in its first second —
# TCGCSV was never reached and no prices were imported at all. A day's prices
# cannot be re-fetched later; a day's set list can.
#
# The Rust tier proves the deferral and the retry
# (crates/pkdump-derive/tests/tail_failure.rs,
# crates/pkdump-ingest/tests/retry.rs). What only the container tier can prove
# is the part systemd actually sees: the SHIPPED binary retries on the budget
# its unit hands it, keeps going, and exits 2 rather than 0 or 1.
DOWN_BEFORE=$(grep -c '"path": "/v2-down/sets' "$UPSTREAM_LOG" || true)
mkdir -p "$RAW"
PARTS_BEFORE_DOWN=$(landed_parts "$RAW")
DOWN_RC=0
podman run --rm -v "${DATA}:/data:Z" -v "${RAW}:/raw:Z" \
	-e PKDUMP_TCGCSV_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/tcgplayer" \
	-e PKDUMP_POKEMONTCG_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/v2-down" \
	-e PKDUMP_HTTP_RETRY_ATTEMPTS=3 \
	-e PKDUMP_HTTP_RETRY_BASE_MS=25 \
	-e PKDUMP_LAKE_DIR=/raw \
	-e PKDUMP_LAKE_ENV=/raw/no-such-lake.env \
	--entrypoint pkdump "$IMAGE" data refresh \
	>"${WORK}/refresh-down.log" 2>&1 || DOWN_RC=$?

# 2, not 1 and not 0. 1 is "the run failed"; 0 would say a stale set list is
# nothing to mention. See crates/pkdump-cli/src/data.rs::refresh.
check "a failed tail exits 2 (partial), not 1" "2" "$DOWN_RC"
check "and says so on stderr" "1" \
	"$(grep -c 'Refresh PARTIAL' "${WORK}/refresh-down.log" || true)"

# It really did retry, in the shipped image, on the budget the environment
# named — 3 attempts at one URL, not 1.
DOWN_HITS=$(($(grep -c '"path": "/v2-down/sets' "$UPSTREAM_LOG" || true) - DOWN_BEFORE))
check "the tail spent its whole retry budget" "3" "$DOWN_HITS"

# And the run carried on past it: the perishable half was still landed. This is
# the difference between a lost night and a partial one — and since pd-lunn the
# night's prices are BYTES IN THE BUCKET rather than rows in a catalog, so a
# tail that took the run with it would leave the derive nothing to build from
# and lose the day exactly as before.
check "the landing continued past the failed tail" "yes" \
	"$(grep -qF "Landing TCGCSV groups, products, prices" "${WORK}/refresh-down.log" && echo yes || echo no)"
# Not merely a log line: the partial run put MORE bytes in the zone than were
# there before it. (The fixture publishes no groups, so what lands is the group
# lists themselves — this gate is about the run reaching TCGCSV at all, and
# tests/lake/real_upstream_derive.sh is where a real price partition is landed.)
check "…and it really left bytes behind" "yes" \
	"$([[ "$(landed_parts "$RAW")" -gt "$PARTS_BEFORE_DOWN" ]] && echo yes ||
		echo "no ($(landed_parts "$RAW") vs ${PARTS_BEFORE_DOWN})")"
# Order, not merely presence: TCGCSV is fetched AFTER the tail has given up.
# That is the difference between a partial night and a lost one, and it is the
# assertion that would have failed on the old binary — which returned from the
# tail's error without issuing a single TCGCSV request.
TAIL_LINE=$(grep -nF "Landing the newest sets from pokemontcg.io" \
	"${WORK}/refresh-down.log" | head -1 | cut -d: -f1)
TCGCSV_LINE=$(grep -nF "Landing TCGCSV groups, products, prices" \
	"${WORK}/refresh-down.log" | head -1 | cut -d: -f1)
check "TCGCSV was landed after the tail gave up" "yes" \
	"$([[ -n "${TCGCSV_LINE:-}" && -n "${TAIL_LINE:-}" && "$TAIL_LINE" -lt "$TCGCSV_LINE" ]] &&
		echo yes || echo no)"

# --- and the partition it left behind (pd-llbq) -----------------------------
# The half of pd-nons's honesty that only the landing path can show: `finalize`
# computes `complete` per DATASET, and `acquire` does not hand it the tail's
# error, so a night only the tail lost comes out short in the tail and whole
# everywhere else. The offline derive reads exactly this to tell a partial
# night (exit 2) from an underivable one (exit 1).
manifest_of() { # manifest_of <dataset>
	find "$RAW" -path "*dataset=$1/*" -name '_manifest.json' 2>/dev/null | head -1
}
SETS_MANIFEST=$(manifest_of sets)
GROUPS_MANIFEST=$(manifest_of groups)
check "the landing zone is outside the data directory" "0" \
	"$(find "$DATA" -name '_manifest.json' 2>/dev/null | wc -l)"
check "the dead tail still landed a manifest" "yes" \
	"$([[ -n "$SETS_MANIFEST" ]] && echo yes || echo no)"
if [[ -n "$SETS_MANIFEST" ]]; then
	# INCOMPLETE, and it says what it ended on. A prefix that read as whole
	# here is what would let a smaller catalog derive from a lost night.
	check "…and it says INCOMPLETE" "1" \
		"$(grep -c '"complete": false' "$SETS_MANIFEST" || true)"
	check "…naming the 502 its retries ended on" "1" \
		"$(grep -c '"status": 502' "$SETS_MANIFEST" || true)"
	# ONE failure, not three. A manifest failure means "this URL was not
	# fetched"; recording the attempts a retry recovers from would mark a
	# whole night incomplete for a hiccup it survived.
	check "…once, not once per attempt" "1" \
		"$(grep -c '"url":' "$SETS_MANIFEST" || true)"
fi
check "the TCGCSV half of the SAME run landed" "yes" \
	"$([[ -n "$GROUPS_MANIFEST" ]] && echo yes || echo no)"
if [[ -n "$GROUPS_MANIFEST" ]]; then
	# The asymmetry itself. Same run, same `finalize` call, opposite answers —
	# which is only possible because completeness is computed per dataset.
	check "…and it is COMPLETE despite the dead tail" "1" \
		"$(grep -c '"complete": true' "$GROUPS_MANIFEST" || true)"
fi
check "one run landed this night, not one per failure" "1" \
	"$(find "$RAW" -name 'run=*' -type d 2>/dev/null | sed 's#.*/##' | sort -u | wc -l)"

# A partial run is still a run that writes no tenant database. Compared
# against AFTER_BACKFILL_TENANTS (step 8's own write already accounted for),
# not the original step-4 baseline.
if diff <(echo "$AFTER_BACKFILL_TENANTS") <(tenant_fingerprint) >"${WORK}/tenants3.diff" 2>&1; then
	check "tenants byte-identical after a partial refresh" "identical" "identical"
else
	check "tenants byte-identical after a partial refresh" "identical" "CHANGED"
	sed 's/^/  /' "${WORK}/tenants3.diff"
fi

if [[ "$DOWN_RC" -ne 2 ]]; then
	echo "  --- the partial refresh's log ---"
	sed 's/^/  /' "${WORK}/refresh-down.log"
fi

printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]] || exit 1
echo "==> tests/refresh/tenant_bytes.sh PASSED"
