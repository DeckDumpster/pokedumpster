#!/usr/bin/env bash
# What a build orphans, collected — measured against real podman (pd-h3wy).
#
# A build leaves two kinds of unreachable image behind: the STAGE images of a
# multi-stage build, which are never tagged, and the image a re-pointed tag used
# to name. Nothing on the box collected either, so they accumulated at roughly
# 2 GB per build — 5.1 GB found in prod's default store, 3.6 GB in the non-prod
# one — on the filesystem prod runs from. At 91% full deploy/ci.sh stopped at its
# own disk floor and the whole container tier became unrunnable.
#
# deploy/image-lib.sh closes it with one rule: A BUILD COLLECTS WHAT THE BUILD
# BEFORE IT ORPHANED. tests/deploy/run.sh §11b asserts everything about that
# which is shell — that both Containerfiles carry the label, that every builder
# in deploy/ goes through the wrapper. What it CANNOT assert is the part that
# actually matters, because all of it is behaviour of podman rather than of this
# repo:
#
#   §1 the store stops growing — flat across four consecutive builds
#   §2 the layer cache SURVIVES the collection
#   §3 a neighbour's dangling image on a shared store is never touched
#   §4 seen red: the same four builds without the rule grow the store
#   §5 seen red: collecting your OWN generation instead destroys the cache —
#      which is why the rule is the previous one, and the easiest thing about
#      this design for a later change to "simplify" away
#
# NOT hermetic — it runs real podman. It needs no registry and no network: the
# fixture is `FROM scratch` plus COPY, so nothing is pulled. It is safe on the
# box that runs prod BY CONSTRUCTION — every image it makes is in a throwaway
# store of its own, and it removes nothing outside it.
#
#   bash tests/store/orphans.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

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

command -v podman >/dev/null || {
	echo "ERROR: podman is not on PATH — this gate needs it." >&2
	exit 1
}

# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"

# Start from the default store like tests/store/netns_split.sh does: this gate is
# about a store it creates itself, and shimming a second store on top of the
# box's would leave `podman` exec'ing through two sets of --root flags.
pkdump_store_deactivate

WORK="$(mktemp -d)"
STORE_ROOT="${WORK}/pkdump-orphans-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)"
CTX="${WORK}/ctx"
mkdir -p "$CTX"

cleanup() {
	export PKDUMP_STORE_ROOT="$STORE_ROOT"
	pkdump_store_teardown >/dev/null 2>&1
	rm -rf "$WORK"
}
trap cleanup EXIT

# The fixture. `FROM scratch` so the gate pulls nothing, and shaped like the real
# Containerfile in the two ways that matter: it has a stage whose image is never
# tagged, and a tagged result that moves every build.
#
#   /cached   identical every build   -> the layer the cache must keep
#   /payload  8 MB of fresh random    -> a genuinely new layer per build, so
#                                        "the store grew" is a real measurement
#                                        and not a rounding artefact
cat > "${CTX}/Buildfile" <<'EOF'
FROM scratch AS builder
COPY cached /cached
COPY payload /payload
LABEL pkdump.build="1"

FROM scratch
COPY --from=builder /payload /payload
LABEL pkdump.build="1"
EOF
dd if=/dev/urandom of="${CTX}/cached" bs=1M count=8 2>/dev/null

export PKDUMP_STORE_ROOT="$STORE_ROOT"
pkdump_store_activate >/dev/null 2>&1 || {
	echo "ERROR: could not activate a store at ${STORE_ROOT}" >&2
	exit 1
}

store_mb() { du -sm "${STORE_ROOT}/storage" 2>/dev/null | cut -f1; }
fresh_payload() { dd if=/dev/urandom of="${CTX}/payload" bs=1M count=8 2>/dev/null; }
cache_hits() { grep -c 'Using cache' <<<"$1"; }
labelled_dangling() { podman images -f dangling=true -f "label=${PKDUMP_IMAGE_LABEL}" -q | wc -l; }

# ---------------------------------------------------------------------------
log "1. Four builds through the real helper, and the store stops growing"
# ---------------------------------------------------------------------------
#
# The point is not "small" — it is FLAT. Without the rule each build adds its
# whole payload and nothing ever gives it back, which is §4.

fresh_payload
pkdump_image_build_collecting -t orph:x -f "${CTX}/Buildfile" "$CTX" >/dev/null 2>&1
AFTER_FIRST="$(store_mb)"

LAST_BUILD_OUT=""
for _ in 1 2 3; do
	fresh_payload
	LAST_BUILD_OUT="$(pkdump_image_build_collecting -t orph:x -f "${CTX}/Buildfile" "$CTX" 2>&1)"
done
AFTER_FOURTH="$(store_mb)"

# One payload of slack: the steady state is the current generation's litter plus
# the one being collected, and du rounds. Four builds of an 8 MB payload each
# would be +24 MB without the rule.
check "the store is flat across four builds" "yes" \
	"$([ "$AFTER_FOURTH" -le $((AFTER_FIRST + 10)) ] && echo yes || echo no)"
[ "$AFTER_FOURTH" -le $((AFTER_FIRST + 10)) ] ||
	echo "        after build 1: ${AFTER_FIRST} MB, after build 4: ${AFTER_FOURTH} MB"

# Bounded, and bounded at ONE generation — this is what says the collection ran
# at all rather than the payload having been deduplicated.
check "one generation of litter is left, not four" "2" "$(labelled_dangling)"

# ---------------------------------------------------------------------------
log "2. ...and the layer cache survives every collection"
# ---------------------------------------------------------------------------
#
# The regression this rule is arranged around. deploy/image-lib.sh exists because
# five gates sharing one build turn a 5m23s compile into 4 seconds, and a
# collection that took the cache with it would hand every one of them the compile
# back — while looking like a disk fix.

check "the last build still hit the cache" "yes" \
	"$([ "$(cache_hits "$LAST_BUILD_OUT")" -ge 1 ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
log "3. A neighbour's dangling image on the same store is never touched"
# ---------------------------------------------------------------------------
#
# What makes this safe to run in prod's store at all. That store is shared —
# another project's images and another project's litter are in it — so `podman
# image prune` is not ours to run there. The label is the confinement, and this
# is the assertion that it is real rather than intended.

printf 'FROM scratch\nCOPY cached /neighbour\n' > "${CTX}/Neighbour"
podman build -t neighbour:x -f "${CTX}/Neighbour" "$CTX" >/dev/null 2>&1
NB="$(podman image inspect -f '{{.Id}}' neighbour:x 2>/dev/null)"
podman image untag neighbour:x >/dev/null 2>&1
check "the neighbour's image is dangling to start with" "1" \
	"$(podman images -f dangling=true -q --no-trunc | grep -c "$NB")"

fresh_payload
pkdump_image_build_collecting -t orph:x -f "${CTX}/Buildfile" "$CTX" >/dev/null 2>&1
fresh_payload
pkdump_image_build_collecting -t orph:x -f "${CTX}/Buildfile" "$CTX" >/dev/null 2>&1
check "...and two of our builds later it is still there" "1" \
	"$(podman images -a -q --no-trunc | grep -c "$NB")"

# ---------------------------------------------------------------------------
log "4. SEEN RED: without the rule, the same four builds grow the store"
# ---------------------------------------------------------------------------
#
# A flat store in §1 proves nothing unless the shape of the run that is NOT flat
# is the same run with the collection taken out.

podman rmi -f "$NB" >/dev/null 2>&1
podman rmi -f "$(podman images -f dangling=true -q)" >/dev/null 2>&1
fresh_payload
podman build -t red:x -f "${CTX}/Buildfile" "$CTX" >/dev/null 2>&1
RED_FIRST="$(store_mb)"
for _ in 1 2 3; do
	fresh_payload
	podman build -t red:x -f "${CTX}/Buildfile" "$CTX" >/dev/null 2>&1
done
RED_FOURTH="$(store_mb)"
check "a bare podman build grows the store instead" "yes" \
	"$([ "$RED_FOURTH" -gt $((RED_FIRST + 10)) ] && echo yes || echo no)"
[ "$RED_FOURTH" -gt $((RED_FIRST + 10)) ] ||
	echo "        after build 1: ${RED_FIRST} MB, after build 4: ${RED_FOURTH} MB"

# ---------------------------------------------------------------------------
log "5. SEEN RED: collecting your OWN generation loses the cache"
# ---------------------------------------------------------------------------
#
# Why the rule is the PREVIOUS build's orphans and not this one's. Collecting
# what you just made drops the last reference to the intermediates and podman
# cascades them away, so the next build recompiles — the exact regression §2
# guards, arrived at by an edit that reads like a simplification.

podman rmi -f "$(podman images -f dangling=true -q)" >/dev/null 2>&1
fresh_payload
podman build -t own:x -f "${CTX}/Buildfile" "$CTX" >/dev/null 2>&1
# The wrong rule, spelled out: collect after the build rather than before it.
podman rmi $(podman images -f dangling=true -f "label=${PKDUMP_IMAGE_LABEL}" -q) >/dev/null 2>&1
fresh_payload
OWN_OUT="$(podman build -t own:x -f "${CTX}/Buildfile" "$CTX" 2>&1)"
check "the cached layer has to be rebuilt" "0" "$(cache_hits "$OWN_OUT")"

# ---------------------------------------------------------------------------
printf '\n=== %s: %d passed, %d failed ===\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
