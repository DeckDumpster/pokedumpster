#!/usr/bin/env bash
# Container-tier gate (pd-hkbc): `pkdump data refresh` writes the SHARED
# catalog and NOT ONE BYTE of any tenant database.
#
# §9 (pd-nons) is the second thing it gates, and it is here rather than in its
# own script because it needs exactly this scaffolding — the shipped image, a
# fixture upstream, and a data directory with real tenants in it — and because
# its own claim ends with "and it still wrote no tenant database".
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
# ── WHY THE UPSTREAM IS A FIXTURE ───────────────────────────────────────────
# The assertion only means something if the refresh RUNS TO THE END — a refresh
# that dies at the first fetch would pass this gate with step 7 still in it. So
# it has to complete, and reaching the real tcgcsv.com to get there would walk
# ~180 English and ~450 Japanese groups at two requests each and make a CI gate
# depend on somebody else's uptime.
#
# tests/refresh/upstream.py publishes NOTHING instead: no sets, no groups. Every
# local phase still runs — variants, sub-type map, bundles, search metadata, set
# discovery, promo synthesis, variant expansion, symbols, latest_prices — and
# the run reaches the end, which is the only part this gate is about. §5 asserts
# it really got there rather than exiting early.
#
# `PKDUMP_TCGCSV_BASE_URL` / `PKDUMP_POKEMONTCG_BASE_URL` are how it is pointed
# there; see crates/pkdump-ingest/src/upstream.rs for why that seam exists and
# why an override announces itself.
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
	podman run --rm -v "${DATA}:/data:Z" \
		-e PKDUMP_TCGCSV_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/tcgplayer" \
		-e PKDUMP_POKEMONTCG_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/v2" \
		--entrypoint pkdump "$IMAGE" data refresh
}

# Every byte under tenants/, keyed by path. WAL and shared-memory sidecars are
# deliberately included: a refresh that merely OPENS a tenant database in WAL
# mode creates a `-wal` beside it, and that is a tenant byte too.
tenant_fingerprint() {
	(cd "$DATA" && find tenants -type f -printf '%p\n' | sort |
		xargs -r -d '\n' sha256sum) 2>/dev/null
}

# Everything else in the data directory, so a stray write somewhere new — a
# registry row, a fresh tenants/ entry, a snapshot file — cannot hide.
data_inventory() { (cd "$DATA" && find . -type f -printf '%P\n' | sort); }

log "1. the shipped image"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"

log "2. a data directory with real tenant databases in it"
mkdir -p "$DATA"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
# The symbol glyphs are pre-normalized so the symbols phase has nothing to
# fetch. It reaches images.pokemontcg.io, which is deliberately outside the
# landing zone and outside this fixture; the http-prefix gate in
# normalize_all_symbols skips a row that already points at /sym/<set>.png.
# Nothing about the tenant claim depends on card art.
sqlite3 "${DATA}/shared.sqlite" \
	"UPDATE sets SET symbol_url = '/sym/' || set_code || '.png' WHERE symbol_url LIKE 'http%';"

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
# wrote nothing" pass for the wrong reason: there would be nothing to value.
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
	echo "  ABORT: the fixture collection has ${SEED_ROWS:-0} rows — with nothing to"
	echo "         value, step 7 would write nothing and this gate would pass on"
	echo "         a binary that still has it."
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
	"$(grep -c '^Refresh complete: /data/shared.sqlite$' "${WORK}/refresh.log" || true)"

log "5. and it really did the work, rather than skipping to the end"
# A refresh that no-ops everything would also leave the tenants alone. These
# are the local derivation phases, each of which must have run for §6 to mean
# "the refresh does its job without touching a tenant" rather than "the refresh
# does nothing".
# Matched as FIXED strings, not patterns: one of these lines is literally
# "N (group, sub_type) → variant rows", and as an ERE its parentheses are
# grouping — the check would look for text that never appears and fail for a
# reason that has nothing to do with the refresh.
for phase in \
	"variant rows reconciled" \
	"(group, sub_type) → variant rows" \
	"bundles registered" \
	"cards synthesized" \
	"printings" \
	"latest-price rows materialized"; do
	check "phase ran: ${phase}" "yes" \
		"$(grep -qF "$phase" "${WORK}/refresh.log" && echo yes || echo no)"
done
check "the shared catalog was written" "changed" \
	"$([[ "$(sha256sum "${DATA}/shared.sqlite" | cut -d' ' -f1)" == "$BEFORE_SHARED" ]] &&
		echo unchanged || echo changed)"

log "6. NOT ONE BYTE of any tenant database moved"
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
# registry row written by a job that has no business provisioning one.
if diff <(echo "$BEFORE_INVENTORY") <(data_inventory) >"${WORK}/inventory.diff" 2>&1; then
	check "and the data directory gained no new files" "same" "same"
else
	check "and the data directory gained no new files" "same" "DIFFERENT"
	sed 's/^/  /' "${WORK}/inventory.diff"
fi

log "7. a second refresh is the same story"
# Once could be an accident of ordering — the first run finding today's rows
# already present, say. The tenant bytes have to survive a refresh over a
# catalog the previous refresh already touched.
SECOND_RC=0
refresh >"${WORK}/refresh2.log" 2>&1 || SECOND_RC=$?
check "the second refresh succeeded" "0" "$SECOND_RC"
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
DOWN_RC=0
podman run --rm -v "${DATA}:/data:Z" \
	-e PKDUMP_TCGCSV_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/tcgplayer" \
	-e PKDUMP_POKEMONTCG_BASE_URL="http://host.containers.internal:${UPSTREAM_PORT}/v2-down" \
	-e PKDUMP_HTTP_RETRY_ATTEMPTS=3 \
	-e PKDUMP_HTTP_RETRY_BASE_MS=25 \
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

# And the run carried on past it: the local derivation phases after the
# acquisition all ran. This is the difference between a lost night and a
# partial one.
check "the derivation continued past the failed tail" "yes" \
	"$(grep -qF "latest-price rows materialized" "${WORK}/refresh-down.log" && echo yes || echo no)"
# Order, not merely presence: TCGCSV is fetched AFTER the tail has given up.
# That is the difference between a partial night and a lost one, and it is the
# assertion that would have failed on the old binary — which returned from the
# tail's error without issuing a single TCGCSV request.
TAIL_LINE=$(grep -nF "Filling newest sets from pokemontcg.io" \
	"${WORK}/refresh-down.log" | head -1 | cut -d: -f1)
TCGCSV_LINE=$(grep -nF "Importing TCGCSV groups, products, prices" \
	"${WORK}/refresh-down.log" | head -1 | cut -d: -f1)
check "TCGCSV was acquired after the tail gave up" "yes" \
	"$([[ -n "${TCGCSV_LINE:-}" && -n "${TAIL_LINE:-}" && "$TAIL_LINE" -lt "$TCGCSV_LINE" ]] &&
		echo yes || echo no)"

# A partial run is still a run that writes no tenant database.
if diff <(echo "$BEFORE_TENANTS") <(tenant_fingerprint) >"${WORK}/tenants3.diff" 2>&1; then
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
