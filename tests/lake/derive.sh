#!/usr/bin/env bash
# Container-tier gate (pd-1uem): shared.sqlite is derived from raw/ ALONE, by
# the SHIPPED image, with the network provably gone.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/derive.sh          # ~2min after the image is warm
#   KEEP=1 bash tests/lake/derive.sh   # leave WORK and the network up
#
# ── WHAT THIS EXISTS TO CATCH, THAT THE RUST TIER CANNOT ────────────────────
# crates/pkdump-lakehouse/tests/row_identical.rs already proves row-identity,
# idempotence, the refusals and the loud fallback — nine tests, each observed
# red as well as green. What it cannot prove is the thing the whole epic is
# for: that the derive needs NO NETWORK. Its fixture upstream is a socket on
# loopback and has to be, or the fallback could never be exercised at all. So a
# derive that quietly reached out for one missing piece would pass every one of
# those tests and leave `raw/` decorative — and we would find out on the day an
# upstream is down, which is the day the lake was bought for.
#
# Here the network is not a convention, it is removed:
#
#   * the podman network is `--internal`. §2 proves it by trying to reach the
#     internet FROM THE SHIPPED IMAGE and requiring the attempt to fail. The
#     same socket-to-1.1.1.1 assertion tests/lake/prices.sh makes.
#   * nothing else is on that network. No upstream stand-in is ever started —
#     the raw bytes were landed on the host, in §0, before any of this existed.
#   * the fixture's upstream origin is still handed to the derive, deliberately
#     (raw/ is keyed by URL, so a replay must build the URLs the fetch built).
#     It points at a host port that is unreachable from an --internal network,
#     which is exactly the property being asserted: a single request would fail
#     the run rather than silently succeed. A URL the partition does not hold is
#     a refusal in the job itself (item 4 removed the fallback), so "it needed
#     the network" cannot be a slow success either way.
#
# And the comparison is against a catalog built by the OTHER path from the SAME
# bytes: §0 lands raw and derives online in one pass over one set of responses.
# Fetching twice would make "the two catalogs disagree" and "the upstream
# answered differently" indistinguishable.
#
# Prod-safe: its own podman network, image tag, temp dir and fixture. It touches
# no pkdump-* unit, no pkdump-*-data volume, no bucket, and no tenant database
# — the derive writes the shared catalog and nothing else.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout: several polecats run deploy/ci.sh at once from their own
# worktrees, and a fixed name means one run's teardown kills another's.
SUFFIX="${PDDERIVE_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
NET="pdderive-net-${SUFFIX}"
IMAGE="localhost/pkdump:derive-${SUFFIX}"

WORK=${WORK:-$(mktemp -d /tmp/pdderive.XXXXXX)}
FIXTURE="$WORK/fixture"

# The two dates the fixture lands. Kept in step with
# crates/pkdump-lakehouse/tests/row_identical.rs.
DAY1=2026-08-10
DAY2=2026-08-11

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
	diag "!! $*"
	exit 1
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving everything up:"
		echo "  network ${NET} (internal: reach it with podman run --network ${NET})"
		echo "  work    ${WORK}"
		echo "  tear down: podman network rm ${NET}; rm -rf ${WORK}"
		return
	fi
	podman network rm "$NET" >/dev/null 2>&1 || true
	podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
	rm -rf "$WORK"
}
trap cleanup EXIT

# The offline derive, in the shipped image, on a network with no way out.
#
# The data directory is mounted read-WRITE (the catalog is the output) and the
# landing zone read-only — `raw/` is immutable, and the job that derives from
# the evidence must not be able to rewrite it. That is enforced in the code by
# `ObjectSource` having no `put`; the mount says the same thing again.
derive() { # derive <ingest-date> [extra args...]
	local date="$1"
	shift
	podman run --rm --network "$NET" \
		-v "${FIXTURE}:/fixture:Z" \
		-v "${FIXTURE}/raw-zone:/raw:ro,Z" \
		-e PKDUMP_LAKE_DIR=/raw \
		-e PKDUMP_LAKE_ENV=/nonexistent/lake.env \
		-e PKDUMP_TCGCSV_BASE_URL="$ORIGIN" \
		-e PKDUMP_POKEMONTCG_BASE_URL="$ORIGIN" \
		--entrypoint pkdump-lake-derive "$IMAGE" \
		shared --ingest-date "$date" \
		--db /fixture/derived.sqlite --data-dir /fixture "$@"
}

# The shipped comparator, on the same image.
diff_catalogs() { # diff_catalogs <left> <right>
	podman run --rm --network "$NET" -v "${FIXTURE}:/fixture:Z" \
		--entrypoint pkdump-lake-derive "$IMAGE" \
		diff --left "$1" --right "$2" --exclude raw_derivation
}

log "0. land raw and derive ONLINE from the same upstream bytes"
# Produced on the HOST by the real clients through the real RawLanding, before
# anything below exists. Nothing on the gate's network ever serves a response.
mkdir -p "$FIXTURE"
PKDUMP_DERIVE_FIXTURE_OUT="$FIXTURE" \
	cargo test --manifest-path "${REPO_DIR}/Cargo.toml" \
	-p pkdump-lakehouse --test row_identical the_fixture >"${WORK}/fixture.log" 2>&1 ||
	{
		sed 's/^/  /' "${WORK}/fixture.log"
		die "the fixture generator failed — see crates/pkdump-lakehouse/tests/row_identical.rs"
	}
[[ -f "${FIXTURE}/online.sqlite" ]] || die "the fixture produced no online.sqlite"
ORIGIN="$(cat "${FIXTURE}/upstream-origin.txt")"
echo "  $(find "$FIXTURE/raw-zone" -name 'part-*.zst' | wc -l) part(s), $(find "$FIXTURE/raw-zone" -name '_manifest.json' | wc -l) manifest(s)"
echo "  the fixture upstream was ${ORIGIN} — and is gone now"

log "1. build the shipped image"
# Through the helper, never a bare `podman build`: deploy/ci.sh builds the shipped
# image ONCE per run and exports PKDUMP_PREBUILT_IMAGE, and tests/deploy/run.sh
# asserts that image-lib.sh is the only thing that builds it (pd-5l2e).
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >"${WORK}/build.log" 2>&1 ||
	{
		tail -40 "${WORK}/build.log"
		die "the image build failed"
	}
check "the image carries the offline derive" "ok" \
	"$(podman run --rm --entrypoint pkdump-lake-derive "$IMAGE" --help >/dev/null 2>&1 && echo ok || echo missing)"

log "2. an INTERNAL network: there is nowhere to fetch from"
podman network create --internal "$NET" >/dev/null
# The assertion the whole gate turns on, made mechanically rather than by
# convention. If this ever starts succeeding, the network stopped being
# internal and every "derived from raw alone" claim below is unproven.
#
# bash's /dev/tcp rather than a python one-liner: the runtime image is
# debian-slim with two binaries in it and no interpreter.
EGRESS=$(podman run --rm --network "$NET" --entrypoint bash "$IMAGE" -c \
	'timeout 5 bash -c "exec 3<>/dev/tcp/1.1.1.1/443" 2>/dev/null && echo REACHABLE || echo blocked')
check "egress from the shipped image is blocked" "blocked" "$EGRESS"
[[ "$EGRESS" == "blocked" ]] ||
	die "the derive container can reach the internet — 'built from raw alone' would prove nothing"

log "3. derive ${DAY1} from raw/, with nowhere to fetch from"
derive "$DAY1" >"${WORK}/derive1.log" 2>&1 || {
	sed 's/^/  /' "${WORK}/derive1.log"
	die "the derive failed for ${DAY1}"
}
check "every request was answered from raw/" "1" \
	"$(grep -c 'raw coverage: complete' "${WORK}/derive1.log" || true)"
check "the clock came from the manifests" "1" \
	"$(grep -c "recovered from the run's manifests" "${WORK}/derive1.log" || true)"

log "4. it is ROW-IDENTICAL to the catalog the online refresh built"
# Not an empty comparison: two empty catalogs are also row-identical.
ROWS=$(podman run --rm -v "${FIXTURE}:/fixture:Z" --entrypoint pkdump-lake-derive "$IMAGE" \
	diff --left /fixture/day1.sqlite --right /fixture/derived.sqlite --exclude raw_derivation |
	tee "${WORK}/diff1.log" | grep -c '^  ok ' || true)
check "the comparison covered real tables" "yes" "$([[ "$ROWS" -ge 10 ]] && echo yes || echo "no (${ROWS})")"
check "${DAY1} derived from raw == ${DAY1} derived online" "1" \
	"$(grep -c 'ROW-IDENTICAL' "${WORK}/diff1.log" || true)"
grep -q 'ROW-IDENTICAL' "${WORK}/diff1.log" || {
	sed 's/^/  /' "${WORK}/diff1.log"
	die "the two catalogs are not row-identical"
}

log "5. and so is the SECOND day, deprecations included"
# Day two renumbers a TCGCSV product, so a printing is soft-deprecated and the
# run's clock is stamped into `printings.deprecated_at` — the one value an
# offline rebuild could not reproduce while variant expansion read its own.
derive "$DAY2" >"${WORK}/derive2.log" 2>&1 || {
	sed 's/^/  /' "${WORK}/derive2.log"
	die "the derive failed for ${DAY2}"
}
diff_catalogs /fixture/online.sqlite /fixture/derived.sqlite >"${WORK}/diff2.log" 2>&1 || {
	sed 's/^/  /' "${WORK}/diff2.log"
	die "day two is not row-identical"
}
check "${DAY2} derived from raw == ${DAY2} derived online" "1" \
	"$(grep -c 'ROW-IDENTICAL' "${WORK}/diff2.log" || true)"

log "6. re-deriving the same date changes nothing"
cp "${FIXTURE}/derived.sqlite" "${FIXTURE}/before-rerun.sqlite"
derive "$DAY2" >"${WORK}/derive2b.log" 2>&1 ||
	die "the second derive of ${DAY2} failed"
diff_catalogs /fixture/before-rerun.sqlite /fixture/derived.sqlite >"${WORK}/diff3.log" 2>&1 || {
	sed 's/^/  /' "${WORK}/diff3.log"
	die "re-deriving ${DAY2} changed the catalog"
}
check "twice equals once" "1" "$(grep -c 'ROW-IDENTICAL' "${WORK}/diff3.log" || true)"

log "7. …and the rerun is IDENTIFIABLE, not merely tolerated"
PROV=$(podman run --rm -v "${FIXTURE}:/fixture:Z" --entrypoint bash "$IMAGE" -c \
	'exec pkdump-lake-derive diff --left /fixture/before-rerun.sqlite \
	   --right /fixture/derived.sqlite' 2>&1 || true)
check "raw_derivation is where the rerun shows" "1" \
	"$(printf '%s' "$PROV" | grep -c 'DIFF raw_derivation' || true)"

log "8. a date nobody landed is REFUSED, not derived from the newest one"
RC=0
derive 2001-01-01 >"${WORK}/refuse.log" 2>&1 || RC=$?
check "the derive refused" "yes" "$([[ "$RC" -ne 0 ]] && echo yes || echo no)"
check "it named the date it was asked for" "yes" \
	"$(grep -q '2001-01-01' "${WORK}/refuse.log" && echo yes || echo no)"
check "it said it never falls back" "yes" \
	"$(grep -q 'never falls back' "${WORK}/refuse.log" && echo yes || echo no)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
if [[ "$fail" -ne 0 ]]; then
	echo "  FAIL — the offline derive is not reproducing the online catalog from raw/ alone."
	exit 1
fi
echo "  PASS — shared.sqlite derives from raw/ with egress provably blocked,"
echo "         row-identical to the online refresh over the same bytes."
