#!/usr/bin/env bash
# The real-scale acceptance harness for the offline derive (pd-aer9, pd-lunn):
# a partition landed by the REAL `pkdump data refresh` from the REAL upstreams
# derives into a real catalog, answering every URL out of raw/.
#
#   PKDUMP_REAL_DERIVE=1 bash tests/lake/real_upstream_derive.sh
#
# NOT a CI gate, deliberately. It fetches ~1,400 responses from tcgcsv.com and
# api.pokemontcg.io and takes the better part of an hour; `deploy/ci.sh` must
# stay hermetic. It is the thing a person runs once per epic milestone, and the
# `PKDUMP_REAL_DERIVE=1` guard is what keeps it from being run by accident.
#
# ── WHY THIS EXISTS BESIDE THE OTHER TWO GATES ─────────────────────────────
# crates/pkdump-lakehouse/tests/row_identical.rs proves row-identity against a
# FIXTURE upstream — three sets, five products, hand-written JSON.
# tests/lake/derive.sh proves the shipped image needs no network. Neither has
# ever seen a real payload. What is unproven by both is everything that only
# exists at scale and only in real bytes:
#
#   * ~700 TCGCSV groups, both categories, including the 450-group Japanese
#     catalog whose sets and cards are synthesized from TCGCSV alone;
#   * set discovery, which invents sets from unbridged numbered groups;
#   * ~25k cards through variant expansion, and the ~300k price rows a real
#     night files;
#   * the real upstreams' real pagination, real nulls, real duplicates.
#
# A replay that got any of that subtly wrong would pass both existing gates.
#
# ── THE ROW-IDENTITY CLAIM THIS USED TO MAKE, AND WHERE IT WENT ────────────
# Until pd-lunn this harness ran BOTH halves — `pkdump data refresh` fetched and
# derived a catalog, the offline job replayed the same bytes into another, and
# §4 diffed them. That comparison retired with its second half: item 6 deleted
# the inline derive, so there is exactly one thing in the workspace that builds
# `shared.sqlite` and nothing left to diff it against.
#
# It is not being quietly dropped. It was RUN, on prod's own nightly partition
# (ingest_date=2026-08-25) against the catalog prod's own refresh had built:
# row-identical across twenty tables, 12,598,388 price rows included. That run
# is what unblocked item 6, and deleting the second builder is what it bought.
#
# What is left here is what one builder can still be held to at real scale, and
# §4 is the part that is genuinely two-sided: derive the same partition twice,
# independently, and require the two catalogs to be row-identical.
#
# ── WHAT IT DOES NOT PROVE, AND WHY ────────────────────────────────────────
# It does not derive the REAL partitions prod's lake holds. Landing a fresh one
# would mean WRITING to the real lake bucket, which this harness is not allowed
# to do, so §2-§5 work over a partition landed HERE, by the real writer, from
# the real upstreams.
#
# §1 still reads the real bucket, read-only, and asserts the two refusals that
# have named dates behind them: the hand-landed 2026-08-11 partition that
# predates `Manifest.started_at` (no clock, so no derive), and a date the
# nightly never landed (no run, and it never reaches for the newest one). Both
# dates are overridable — prod has landed nightly since 2026-08-24, so a date
# picked at random is now a partition that derives fine, which is not what §1 is
# asking about.
#
# Prod-safe: its own PKDUMP_HOME under $WORK, its own landing zone on local
# disk (PKDUMP_LAKE_DIR), no container, no systemd unit, no tenant database,
# and not one byte written to any S3 bucket. The catalog the landing run reads
# is opened READ-ONLY by the command itself, and §4a checks the bytes.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

[[ -n "${PKDUMP_REAL_DERIVE:-}" ]] || {
	echo "refusing to run: this harness hits the real upstreams for ~40 minutes." >&2
	echo "  PKDUMP_REAL_DERIVE=1 bash tests/lake/real_upstream_derive.sh" >&2
	exit 2
}

# The big disk, not the one prod runs from. Overridable, but never defaulted
# into $HOME: a full catalog plus a landing zone is a couple of gigabytes.
WORK="${WORK:-$(mktemp -d "${PKDUMP_REAL_DERIVE_ROOT:-/workspaces}/pdreal.XXXXXX")}"
READER="$WORK/reader"
OFFLINE="$WORK/offline"
OFFLINE2="$WORK/offline2"
RAW="$WORK/raw-zone"
LOGS="$WORK/logs"
mkdir -p "$READER" "$OFFLINE" "$OFFLINE2" "$RAW" "$LOGS"

PKDUMP="${PKDUMP_BIN:-$REPO_DIR/target/release/pkdump}"
DERIVE="${PKDUMP_DERIVE_BIN:-$REPO_DIR/target/release/pkdump-lake-derive}"

# The partition the landing half lands and the offline half rebuilds. The clock
# picks it (`DeriveClock::now`), so it is today in UTC — the same value
# `pkdump data refresh` will choose a moment from now.
DATE="${PKDUMP_REAL_DERIVE_DATE:-$(date -u +%F)}"

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
die() {
	echo "!! $*" >&2
	exit 1
}
log() { printf '\n=== %s ===\n' "$*"; }

for bin in "$PKDUMP" "$DERIVE"; do
	[[ -x "$bin" ]] || die "$bin is not built — cargo build --release -p pkdump-cli -p pkdump-lakehouse"
done

log "0. what this run is"
echo "  work dir      ${WORK}"
echo "  ingest_date   ${DATE}"
echo "  reader  →     ${READER}/shared.sqlite   (READ by the landing run; must not change)"
echo "  offline →     ${OFFLINE}/shared.sqlite   (replays ${RAW})"
echo "  offline2 →    ${OFFLINE2}/shared.sqlite  (replays it again, independently)"

log "1. the REAL lake bucket, read-only: what it holds and what it refuses"
# The operator's real lake.env, sourced for the S3 settings and the AWS profile
# it names, with PKDUMP_LAKE_DIR out of the way so this resolves the BUCKET.
# Read-only by construction: `open_reader` hands back an `ObjectSource`, which
# has no `put` on it at all — this cannot write to the lake even if it tried.
LAKE_ENV_FILE="${PKDUMP_REAL_LAKE_ENV:-$HOME/.config/pkdump/lake.env}"
real_derive() { # real_derive <ingest-date> <logfile>
	(
		set -a
		# shellcheck disable=SC1090
		. "$LAKE_ENV_FILE"
		set +a
		unset PKDUMP_LAKE_DIR
		"$DERIVE" shared --ingest-date "$1" \
			--db "$WORK/must-not-exist.sqlite" --no-upstream-fallback
	) >"$2" 2>&1
}

if [[ ! -f "$LAKE_ENV_FILE" ]]; then
	echo "  SKIP — no ${LAKE_ENV_FILE}; §1 needs the real bucket's host config"
else
	# A partition the lake holds that CANNOT be derived: landed by hand on
	# 2026-08-11 (pd-fet2), before `Manifest.started_at` existed. Nightly
	# landing started on 2026-08-24 (pd-kncd), so every date from then on is
	# derivable and would prove the opposite of what this section asks.
	REAL_DATE="${PKDUMP_REAL_PARTITION_DATE:-2026-08-11}"
	RC=0
	real_derive "$REAL_DATE" "$LOGS/real-bucket.log" || RC=$?
	sed 's/^/    /' "$LOGS/real-bucket.log"
	check "the real partition is refused, not derived" "yes" \
		"$([[ "$RC" -ne 0 ]] && echo yes || echo no)"
	check "…because its manifests carry no clock" "yes" \
		"$(grep -q 'records no started_at' "$LOGS/real-bucket.log" && echo yes || echo no)"
	check "the refusal left no catalog behind" "yes" \
		"$([[ ! -f "$WORK/must-not-exist.sqlite" ]] && echo yes || echo no)"

	# A night the nightly refresh ran and landed nothing — the shape pd-kncd
	# describes. It must refuse rather than reach for the date beside it.
	RC=0
	real_derive "${PKDUMP_REAL_UNLANDED_DATE:-2026-08-12}" "$LOGS/real-bucket-missing.log" || RC=$?
	sed 's/^/    /' "$LOGS/real-bucket-missing.log"
	check "a date the nightly never landed is refused too" "yes" \
		"$([[ "$RC" -ne 0 ]] && echo yes || echo no)"
	check "…and it never falls back to the newest one" "yes" \
		"$(grep -q 'never falls back' "$LOGS/real-bucket-missing.log" && echo yes || echo no)"
fi

log "2. a baseline catalog, and the same bytes everywhere it is used"
# Every side starts from ONE catalog, copied. That is prod's shape — a nightly
# run happens against a catalog that already holds every set — and it is what
# keeps the comparison about the derivation rather than about acquisition: the
# interesting rows are the ones the compared pass writes on top.
#
# `--skip-prices` because the TCGCSV pass is exactly what the compared refresh
# does next; the bulk corpus (cards and sets, from the pokemon-tcg-data
# tarball) is all the baseline needs. Not landed: this pass is scaffolding, and
# the partition under comparison must hold one run's bytes and no other's.
if [[ -f "${PKDUMP_REAL_DERIVE_BASELINE:-}" ]]; then
	echo "  reusing the baseline at ${PKDUMP_REAL_DERIVE_BASELINE}"
	cp "$PKDUMP_REAL_DERIVE_BASELINE" "$WORK/baseline.sqlite"
else
	START=$SECONDS
	# `--skip-tail` as well, so the baseline needs api.pokemontcg.io not at all.
	# It answers 500 to something like 40% of requests (pd-nons), and a baseline
	# that cannot be built is a harness that cannot run; the ONE pokemontcg.io
	# request the compared refresh makes is retried in §2b instead.
	PKDUMP_HOME="$WORK/baseline-home" "$PKDUMP" setup \
		--db "$WORK/baseline.sqlite" --skip-prices --skip-tail >"$LOGS/baseline.log" 2>&1 || {
		tail -40 "$LOGS/baseline.log"
		die "the baseline setup failed"
	}
	echo "  built in $((SECONDS - START))s — keep it with PKDUMP_REAL_DERIVE_BASELINE=$WORK/baseline.sqlite"
fi
cp "$WORK/baseline.sqlite" "$READER/shared.sqlite"
cp "$WORK/baseline.sqlite" "$OFFLINE/shared.sqlite"
cp "$WORK/baseline.sqlite" "$OFFLINE2/shared.sqlite"
# The symbol cache travels with it. Set symbols are IMAGES and images are
# deliberately never landed (pd-5w4n), so this is the one phase a replay cannot
# answer from raw/. It is a no-op on both sides here — the baseline already
# rewrote every `sets.symbol_url` to `/sym/<set>.png`, and the http-prefix gate
# in `normalize_all_symbols` skips a row that no longer names an upstream — but
# the cache is copied anyway so a set that arrives between the two passes finds
# its PNG on disk rather than making the phase a live-network coin flip.
# `$WORK/symbols` because a run's data dir is its DATABASE's directory, not
# $PKDUMP_HOME.
for side in "$READER" "$OFFLINE" "$OFFLINE2"; do
	cp -r "$WORK/symbols" "$side/symbols" 2>/dev/null || true
done
check "every side starts from the same bytes" "yes" \
	"$(cmp -s "$OFFLINE/shared.sqlite" "$OFFLINE2/shared.sqlite" && echo yes || echo no)"
echo "        baseline: $(sqlite3 "$WORK/baseline.sqlite" 'SELECT COUNT(*) FROM cards') cards, $(sqlite3 "$WORK/baseline.sqlite" 'SELECT COUNT(*) FROM sets') sets"

log "2b. LAND: a real \`pkdump data refresh\` against the real upstreams"
# The whole of the online half since pd-lunn: fetch every upstream, write every
# response into raw/, build nothing.
#
# Retried, because api.pokemontcg.io answers 500 to a large fraction of requests
# and the tail is reached FIRST. A tail that gives up is not a failure here —
# the run exits 2 and TCGCSV is landed anyway (pd-nons) — but it lands a short
# partition, and a short partition is not what §3 is meant to derive. So an
# attempt is all-or-nothing: exit 0 or start over with an empty landing zone. A
# half-run's parts carried into the next attempt would make the derive read two
# runs' bytes and refuse for disagreeing about the clock.
START=$SECONDS
ATTEMPTS="${PKDUMP_REAL_DERIVE_ATTEMPTS:-6}"
for attempt in $(seq 1 "$ATTEMPTS"); do
	rm -rf "$RAW"
	mkdir -p "$RAW"
	cp "$WORK/baseline.sqlite" "$READER/shared.sqlite"
	if PKDUMP_LAKE_DIR="$RAW" PKDUMP_LAKE_ENV=/nonexistent/lake.env \
		"$PKDUMP" data refresh --db "$READER/shared.sqlite" \
		>"$LOGS/land.log" 2>&1; then
		echo "  attempt ${attempt}: clean"
		break
	fi
	echo "  attempt ${attempt}: FAILED — $(grep -m1 '^Error:' "$LOGS/land.log" || echo 'see the log')"
	cp "$LOGS/land.log" "$LOGS/land-failed-${attempt}.log"
	[[ "$attempt" -lt "$ATTEMPTS" ]] || {
		tail -40 "$LOGS/land.log"
		die "the landing run failed ${ATTEMPTS} times — the upstreams are having a day"
	}
done
echo "  $((SECONDS - START))s, $(find "$RAW" -name 'part-*.zst' | wc -l) part(s), $(du -sh "$RAW" | cut -f1) landed"
grep -E '^  (raw:|landed|[0-9]+ group)' "$LOGS/land.log" | sed 's/^/  /' || true
check "every landed manifest is complete" "0" \
	"$(grep -c 'INCOMPLETE' "$LOGS/land.log" || true)"
check "the partition it landed is ${DATE}" "yes" \
	"$([[ -d "$RAW/raw/source=tcgcsv/dataset=prices/ingest_date=${DATE}" ]] && echo yes || echo no)"
# The acceptance criterion for pd-lunn, at real scale: the refresh READS the
# catalog and writes no table of it. Byte-identical, because "no rows I thought
# to count" is a weaker claim than the one the read-only connection makes.
check "the landing run left the catalog it read untouched" "yes" \
	"$(cmp -s "$WORK/baseline.sqlite" "$READER/shared.sqlite" && echo yes || echo no)"
check "…and derived nothing into it" "yes" \
	"$([[ "$(sqlite3 "$READER/shared.sqlite" 'SELECT COUNT(*) FROM raw_derivation')" == 0 ]] && echo yes || echo no)"

log "3. OFFLINE: rebuild the same date from raw/ alone"
START=$SECONDS
PKDUMP_LAKE_DIR="$RAW" PKDUMP_LAKE_ENV=/nonexistent/lake.env \
	"$DERIVE" shared --ingest-date "$DATE" \
	--db "$OFFLINE/shared.sqlite" --data-dir "$OFFLINE" \
	--no-upstream-fallback >"$LOGS/offline.log" 2>&1 || {
	tail -40 "$LOGS/offline.log"
	die "the offline derive failed"
}
echo "  $((SECONDS - START))s"
grep -E 'clock|URL\(s\) replayable|raw coverage|Derive complete' "$LOGS/offline.log" | sed 's/^/  /'
check "every request was answered from raw/" "1" \
	"$(grep -c 'raw coverage: complete' "$LOGS/offline.log" || true)"
check "the clock came from the manifests" "1" \
	"$(grep -c "recovered from the run's manifests" "$LOGS/offline.log" || true)"

log "4. the SAME partition, derived a second time, independently"
# The two-sided claim that survives having one builder. Not "derive twice into
# the same file", which the hermetic tier already covers — a second catalog,
# from the same baseline, from the same bytes, by a second process. A
# derivation that read anything but that partition (a clock, a directory
# listing, whatever raw/ happened to hold newest) differs here.
START=$SECONDS
PKDUMP_LAKE_DIR="$RAW" PKDUMP_LAKE_ENV=/nonexistent/lake.env \
	"$DERIVE" shared --ingest-date "$DATE" \
	--db "$OFFLINE2/shared.sqlite" --data-dir "$OFFLINE2" \
	--no-upstream-fallback >"$LOGS/offline2.log" 2>&1 || {
	tail -40 "$LOGS/offline2.log"
	die "the second offline derive failed"
}
echo "  $((SECONDS - START))s"

# `raw_derivation` records WHEN the derive ran, so two runs legitimately differ
# in it. Named on the command line rather than skipped quietly.
"$DERIVE" diff --left "$OFFLINE/shared.sqlite" --right "$OFFLINE2/shared.sqlite" \
	--exclude raw_derivation >"$LOGS/diff.log" 2>&1 || true
sed 's/^/  /' "$LOGS/diff.log"
TABLES=$(grep -c '^  ok ' "$LOGS/diff.log" || true)
check "the comparison covered the real catalog" "yes" \
	"$([[ "$TABLES" -ge 10 ]] && echo yes || echo "no (${TABLES})")"
check "the two catalogs are row-identical" "1" \
	"$(grep -c '^ROW-IDENTICAL' "$LOGS/diff.log" || true)"

log "5. and it is not an empty comparison"
rows() { sqlite3 "$1" "SELECT COUNT(*) FROM $2"; }
for table in cards sets printings prices latest_prices tcgcsv_products sealed_prices; do
	L=$(rows "$OFFLINE/shared.sqlite" "$table")
	B=$(rows "$WORK/baseline.sqlite" "$table")
	check "${table}: non-trivial, and the derive added to it" "yes" \
		"$([[ "$L" -gt 100 && "$L" -ge "$B" ]] && echo "yes" || echo "no (${L}, baseline ${B})")"
	echo "        ${table}: ${L} rows (baseline ${B})"
done

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
echo "  logs and both catalogs kept at ${WORK}"
if [[ "$fail" -ne 0 ]]; then
	echo "  FAIL — the offline derive does not hold up on real data."
	exit 1
fi
echo "  PASS — on ${DATE}, at real catalog scale, from the real upstreams:"
echo "         a landing-only refresh left its catalog byte-identical, the"
echo "         partition it landed answered every URL the derive asked for, and"
echo "         two independent derives of it are row-identical."
