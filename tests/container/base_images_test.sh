#!/usr/bin/env bash
# Unit test for the Containerfile's base-image pins (pd-pejn).
#
# The bug: stage 1 was `FROM docker.io/library/rust:1.94-slim`, which does not
# name a Debian release. Upstream retagged it from bookworm to trixie, so the
# builder began linking against glibc 2.39 while the runtime stage stayed on
# bookworm's 2.36, and every image built after that point shipped a binary that
# cannot exec:
#
#   pkdump: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found
#
# Nothing in the tree held the invariant `builder glibc <= runtime glibc` — it
# was being held by upstream's tagging habits. This file holds it instead.
#
#   §1 every base image names a Debian release; none is a moving tag.
#   §2 builder and runtime name the SAME release. This is the invariant.
#   §3 THE SECOND HALF OF THE BUG. Re-pinning the FROM line alone fixed
#      nothing: the BuildKit cache at /app/target still held the objects
#      compiled on trixie, cargo fingerprints do not record which base image
#      produced them (same crate versions, same rustc, same triple), so it
#      relinked nothing and `cp target/release/pkdump /out/pkdump` copied the
#      poisoned artifact through. The build said `Finished release profile in
#      0.80s` and looked like a successful fix. So the cache is scoped by `id=`
#      to the builder's release, and that id is asserted to AGREE with the FROM
#      line — a base change that forgets the id fails here rather than shipping
#      an image that cannot start.
#
# Deliberately hermetic — no podman, no network, no build — so deploy/ci.sh can
# run it in the sub-second tier beside tests/lib/ports_test.sh, long before any
# container gate spends ten minutes building the image this would have broken.
#
#   bash tests/container/base_images_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Debian's release names. A base tag that carries none of these is not pinned
# to a release, whatever else it says.
CODENAMES='bullseye|bookworm|trixie|forky|sid'

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
# "Some base image is unpinned" is useless without saying which line.
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

# Every image build in the tree, not just the one that broke. A second build
# file added later gets the same rules without anyone remembering this test.
buildfiles() {
	find "${REPO_DIR}" \
		\( -name node_modules -o -name .git -o -name target -o -name build \) -prune -o \
		\( -iname 'Containerfile' -o -iname 'Containerfile.*' \
		-o -iname 'Dockerfile' -o -iname 'Dockerfile.*' \) -type f -print | sort
}

# The release named by a stage's tag, or empty if it names none.
release_of() { # release_of <from-line>
	grep -oE "$CODENAMES" <<<"$1" | head -1
}

CONTAINERFILE="${REPO_DIR}/Containerfile"

log "0. the tree has build files to check"
FILES="$(buildfiles)"
check "at least one Containerfile/Dockerfile found" "yes" \
	"$([[ -n "$FILES" ]] && echo yes || echo no)"
check "the shipped Containerfile is one of them" "yes" \
	"$(grep -qxF "$CONTAINERFILE" <<<"$FILES" && echo yes || echo no)"

log "1. every base image names a Debian release (pd-pejn)"
# `rust:1.94-slim` is the exact shape that drifted. Scratch and a stage
# referring to an earlier stage by name are not base images from a registry.
UNPINNED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	while IFS= read -r line; do
		[[ -z "$line" ]] && continue
		# `<file>:<lineno>:FROM [--flag …] <ref> [AS <stage>]` — the ref is the
		# first token after FROM that is not a flag.
		ref="$(awk '{for (i = 2; i <= NF; i++) if ($i !~ /^--/) { print $i; exit }}' \
			<<<"${line#*:}")"
		[[ "$ref" == scratch ]] && continue
		[[ "$ref" == *"/"* ]] || continue # bare word = an earlier stage
		[[ -n "$(release_of "$ref")" ]] || UNPINNED+="${line}"$'\n'
	done < <(grep -nE '^[[:space:]]*FROM[[:space:]]' "$f" /dev/null)
done <<<"$FILES"
none "no FROM rides a moving tag" "${UNPINNED%$'\n'}"

log "2. builder and runtime are the same release (the glibc invariant)"
BUILDER_FROM="$(grep -E '^FROM .*AS builder' "$CONTAINERFILE")"
# The runtime stage is the last FROM: the one whose glibc has to be new enough
# for everything copied into it.
RUNTIME_FROM="$(grep -E '^FROM ' "$CONTAINERFILE" | tail -1)"
BUILDER_RELEASE="$(release_of "$BUILDER_FROM")"
RUNTIME_RELEASE="$(release_of "$RUNTIME_FROM")"
check "a builder stage exists" "yes" "$([[ -n "$BUILDER_FROM" ]] && echo yes || echo no)"
check "the runtime stage is not the builder" "yes" \
	"$([[ "$RUNTIME_FROM" != "$BUILDER_FROM" ]] && echo yes || echo no)"
check "builder release == runtime release" \
	"${RUNTIME_RELEASE:-<none>}" "${BUILDER_RELEASE:-<none>}"

log "3. the target cache is scoped to that release (pd-pejn, second half)"
# A cache mount over the compiler's output directory MUST carry an id, or it is
# shared with whatever the last base image left in it.
TARGET_MOUNT="$(grep -nE -- '--mount=type=cache,[^[:space:]]*target=/app/target' "$CONTAINERFILE")"
check "the target cache mount is there to scope" "yes" \
	"$([[ -n "$TARGET_MOUNT" ]] && echo yes || echo no)"
UNSCOPED="$(grep -nE -- '--mount=type=cache,[^[:space:]]*target=/app/target' "$CONTAINERFILE" |
	grep -vE 'id=')"
none "it carries an id=" "$UNSCOPED"
CACHE_ID="$(grep -oE -- 'id=[^,[:space:]\\]+' <<<"$TARGET_MOUNT" | head -1)"
CACHE_ID="${CACHE_ID#id=}"
# The whole point: the id has to MOVE when the base image moves. Agreement with
# the builder's release is what makes forgetting it a test failure instead of
# an image that cannot start.
check "the id names the builder's release" "${BUILDER_RELEASE:-<none>}" \
	"$(release_of "$CACHE_ID")"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — builder and runtime are pinned to ${RUNTIME_RELEASE}, and the"
echo "         target cache is scoped to it."
