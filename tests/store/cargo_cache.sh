#!/usr/bin/env bash
# The image ships the binaries the image's OWN sources build (pd-wjmd).
#
# The Containerfile hands cargo a `--mount=type=cache` target directory that
# outlives the build, scoped by `id=` on the builder's Debian release and the
# CHECKOUT PATH. A CI runner is ONE checkout path holding EVERY tree over time —
# a different PR merge ref on every run — so that id is constant while the
# sources under it are not, and cargo's freshness check is an MTIME COMPARISON.
# Hand it sources older than the rlibs a different tree left in the cache and it
# compiles nothing; `cp` then copies the other tree's binaries into the image.
#
# It is not hypothetical. On CI run 33683221574 the builder reported
#
#     Finished `release` profile [optimized] target(s) in 1.92s
#
# over a crates/ with no `sets.ptcgio_covered` in it, and shipped a
# pkdump-lake-derive that writes that column. tests/lake/derive.sh caught it as
# a catalog disagreeing with the one the host built — which is a gate finding it
# by accident, three tiers away from the cause.
#
# The Containerfile answers it with a STAMP: the cache records the sources it
# was compiled from, and a build handed a cache written from anything else
# touches the sources before compiling, which is the one thing that makes the
# mtime comparison tell the truth again.
#
# WHAT THIS GATE ADDS, AND WHY IT IS NOT HERMETIC. That podman's COPY preserves
# the build context's mtimes, and that cargo then calls an old source fresh
# against a newer artifact, are facts about podman and cargo rather than about
# this repo — a shell test can only assert that the Containerfile asks for the
# right thing, which is exactly what tests/deploy/run.sh §11c does. Here the two
# builds really happen, and §3 requires the hazard to be REAL: with the restamp
# taken out, the same two builds ship the first tree's binary. A gate whose red
# arm passes is a gate measuring nothing.
#
# The fixture is a hello-world crate rather than this workspace, so each build is
# seconds instead of minutes; it uses the builder image named by the REAL
# Containerfile, and no registry, so it pulls nothing the container tier does not
# already need. Its cache id is unique to this run and removed on the way out, so
# it can neither read nor poison any build cache on the box.
#
#   bash tests/store/cargo_cache.sh
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

# The builder base is read out of the real Containerfile rather than repeated
# here: pd-pejn is the bead about a base image moving underneath this repo, and
# a fixture pinned to its own release would be the next copy to go stale.
BUILDER_FROM="$(sed -n 's/^FROM \(.*\) AS builder$/\1/p' "${REPO_DIR}/Containerfile" | head -1)"
[[ -n "$BUILDER_FROM" ]] || {
	echo "ERROR: no 'FROM ... AS builder' line in ${REPO_DIR}/Containerfile" >&2
	exit 1
}
echo "  builder base (read from the real Containerfile): ${BUILDER_FROM}"

WORK="$(mktemp -d)"
SUFFIX="$(printf '%s-%s' "$REPO_DIR" "$$" | sha1sum | cut -c1-12)"
CACHE_ID="pkdump-cargocache-test-${SUFFIX}"
IMAGE="localhost/pkdump-cargocache-test:${SUFFIX}"
CTX="${WORK}/ctx"
mkdir -p "${CTX}/src"

cleanup() {
	podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
	# The cache mount is NOT inside the container store — buildah keeps it at
	# /var/tmp/buildah-cache-<uid>/<id>. Nothing else collects it, and this gate
	# creates one per run, so removing it here is the whole of its cleanup.
	rm -rf "/var/tmp/buildah-cache-$(id -u)/${CACHE_ID}"
	rm -rf "$WORK"
}
trap cleanup EXIT

# The fixture crate: no dependencies, so the build needs no registry and no
# network, and one `println!` is the whole observable difference between the two
# trees. That is deliberately the WEAKEST possible signal — a stale rlib that
# still exports everything referenced links cleanly, which is what makes this
# failure mode ship old behaviour instead of failing to build.
cat >"${CTX}/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.0.0"
edition = "2021"

[workspace]
EOF

# <ctx>/Buildfile, shaped like the real builder stage in the three ways that
# matter: the same cache mount, the same stamp-and-restamp preamble, and the
# same `cp` of a binary OUT of a cache directory that outlives the build.
#
# `$1` = "restamp" (the real shape) or "naive" (the shape this bead found).
#
# Named `Buildfile`, not `Containerfile`, for the reason tests/store/orphans.sh's
# fixture is: tests/deploy/run.sh §11 asserts that nothing under tests/ runs a
# builder over the SHIPPED Containerfile, and a fixture that answered to that
# name would have to be carved out of the assertion instead of being outside it.
write_containerfile() { # write_containerfile <restamp|naive>
	local guard
	if [[ "$1" == "restamp" ]]; then
		guard=' && if [ "$(cat target/.pkdump-source-stamp 2>/dev/null)" != "$stamp" ]; then \
        echo "==> the cargo target cache was written from other sources — restamping mtimes"; \
        find ${PKDUMP_SOURCES} -type f -exec touch {} +; \
    fi \'
	else
		guard=' \'
	fi
	cat >"${CTX}/Buildfile" <<EOF
FROM ${BUILDER_FROM} AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src/ src/
ENV PKDUMP_SOURCES="Cargo.toml src"
RUN --mount=type=cache,target=/app/target,sharing=locked,id=${CACHE_ID} \\
    stamp="\$(find \${PKDUMP_SOURCES} -type f -exec sha256sum {} + \\
             | LC_ALL=C sort | sha256sum | cut -d' ' -f1)"${guard}
 && rm -f target/.pkdump-source-stamp \\
 && cargo build --release --offline \\
 && printf '%s\\n' "\$stamp" > target/.pkdump-source-stamp \\
 && mkdir -p /out && cp target/release/fixture /out/fixture
EOF
}

# Every source file older than anything the cache can hold. This is what a COPY
# that hits podman's layer cache does for free — it restores the layer's own
# mtimes rather than today's — and stating it outright makes the gate about the
# mtime comparison instead of about whether a layer happened to be cached.
write_tree() { # write_tree <word>
	printf 'fn main() { println!("%s"); }\n' "$1" >"${CTX}/src/main.rs"
	find "$CTX" -type f -exec touch -d '2020-01-01T00:00:00Z' {} +
}

build() { # build [extra podman build args...] -> stdout: the builder's output
	podman build "$@" -t "$IMAGE" -f "${CTX}/Buildfile" "$CTX" 2>&1
}
says() { # says -> stdout: what the shipped binary prints
	podman run --rm --entrypoint /out/fixture "$IMAGE" 2>/dev/null
}
restamped() { # restamped <build output> -> yes|no
	grep -qE '^[[:space:]]*==> the cargo target cache' <<<"$1" && echo yes || echo no
}

# ---------------------------------------------------------------------------
log "1. A first build fills the cache, and ships its own sources"
# ---------------------------------------------------------------------------
write_containerfile restamp
write_tree ALPHA
FIRST="$(build)"
check "the first build succeeded" "yes" \
	"$([[ -n "$(says)" ]] && echo yes || echo no)"
check "it ships the tree it was given" "ALPHA" "$(says)"

# ---------------------------------------------------------------------------
log "2. A DIFFERENT tree, older than the cache, still ships ITSELF"
# ---------------------------------------------------------------------------
#
# The whole bead in three lines. Nothing about the second tree is newer than the
# first build's artifacts, so cargo has no honest reason to recompile — and the
# stamp is the reason it is given one.
write_tree BETA
SECOND="$(build)"
# Anchored at the start of a line: podman ECHOES the RUN it is about to execute,
# so the phrase appears in every build's log whether or not the branch ran. An
# unanchored grep passes §3 and §2 alike and measures nothing.
check "the second build noticed the cache was another tree's" "yes" \
	"$(restamped "$SECOND")"
check "and it ships ITS OWN sources, not the cache's" "BETA" "$(says)"
[[ "$(says)" == "BETA" ]] || {
	echo "        an image built from BETA shipped '$(says)' — the cache leaked"
	sed 's/^/        | /' <<<"$SECOND" | tail -20
}

# ---------------------------------------------------------------------------
log "3. …and the cache is still worth having: the SAME tree recompiles nothing"
# ---------------------------------------------------------------------------
#
# The regression a fix for §2 arrives disguised as. Touching unconditionally
# would pass §2 and hand every build the compile back — deploy/image-lib.sh
# exists because that difference is 4 seconds against 5m23s.
#
# `--no-cache` so the RUN really executes rather than being answered by the
# layer cache: the claim under test is what the stamp comparison DOES with a
# cache that matches, and a step podman skipped would assert nothing about it.
# It also re-COPYs, so the sources arrive with the context's 2020 mtimes — the
# same shape as §2, differing only in that the cache is this tree's.
THIRD="$(build --no-cache)"
check "an unchanged tree is not restamped" "no" "$(restamped "$THIRD")"
check "and cargo compiled nothing" "yes" \
	"$(grep -qE 'Compiling fixture' <<<"$THIRD" && echo no || echo yes)"
check "it still ships the right binary" "BETA" "$(says)"

# ---------------------------------------------------------------------------
log "4. SEEN RED: without the restamp, the second tree ships the FIRST one's"
# ---------------------------------------------------------------------------
#
# Without this section §2 proves nothing: a fixture in which the hazard does not
# reproduce would pass it with the guard doing no work at all.
rm -rf "/var/tmp/buildah-cache-$(id -u)/${CACHE_ID}"
podman rmi -f "$IMAGE" >/dev/null 2>&1
write_containerfile naive
write_tree ALPHA
build >/dev/null
NAIVE_FIRST="$(says)"
write_tree BETA
build >/dev/null
check "the naive build's first tree shipped" "ALPHA" "$NAIVE_FIRST"
check "and its SECOND tree shipped the FIRST one's binary" "ALPHA" "$(says)"
[[ "$(says)" == "ALPHA" ]] || echo "        the hazard did not reproduce — §2 proves nothing as written"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
if [[ "$fail" -ne 0 ]]; then
	echo "  FAIL — the image can ship binaries its own sources did not build."
	exit 1
fi
echo "  PASS — the shipped binaries are the ones the image's sources build,"
echo "         and an unchanged tree still costs nothing."
