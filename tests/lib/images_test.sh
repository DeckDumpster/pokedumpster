#!/usr/bin/env bash
# Ratchet: a container gate removes the image tag it named into existence
# (pd-5aba).
#
# Every gate under tests/ builds — or, under PKDUMP_PREBUILT_IMAGE, re-tags —
# its image at a tag carrying a sha1 of its own checkout path, because
# concurrent polecats run whole suites from their own worktrees. That suffix is
# what keeps two runs apart, and it is also what makes the leak unbounded: the
# tag is unique per (gate, checkout), the worktree is deleted when the polecat
# is done, and NOTHING on the box ever collects the tag it left behind.
#
# Six gates already removed theirs and three did not. The three were
# tests/tenants/handles.sh, tests/tenants/upgrade.sh and tests/keys/run.sh, and
# together they were the bulk of the leaked images found on the prod disk with
# it at 80% — fourteen `pkdump:{handles,upgrade}-*` tags from worktrees that no
# longer existed. Reclaiming them by hand is operational and one-shot; this is
# the part that stops it recurring.
#
# The rule is stated over the TREE rather than over the three files that were
# wrong, because the failure mode is a gate nobody has written yet: a new
# harness copies a neighbour that leaks, and nothing says so until a disk fills
# up months later. It fails here in under a second instead.
#
# tests/ only. A deployment KEEPS its image — that is what deploy/setup.sh and
# deploy/deploy.sh exist to leave behind — so deploy/ is deliberately not
# subject to this.
#
# Hermetic — no podman, no network — so deploy/ci.sh runs it in the lint tier
# beside tests/lib/ports_test.sh.
#
#   bash tests/lib/images_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

pass=0
fail=0
log() { printf '\n=== %s ===\n' "$*"; }
# An assertion that prints the offending files, because "some gate leaks an
# image" is useless without saying which one does.
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
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}

# Every gate under tests/, minus this file — its patterns are written to look
# like the thing they detect.
harnesses() {
	find "${REPO_DIR}/tests" -name '*.sh' -type f \
		! -path "${REPO_DIR}/tests/lib/images_test.sh" | sort
}

# The shell variables a file names an image tag into existence THROUGH:
# `pkdump_image_ensure "$IMAGE" …` (build or re-tag) and `podman build -t
# "$JOB_IMAGE" …`. Only variables — a literal tag in a gate would be the
# cross-checkout collision tests/lib/ports_test.sh's own ratchet is about, and
# there are none.
tags_created_in() { # tags_created_in <file>
	grep -hoE '(pkdump_image_ensure|podman build -t) +"\$\{?[A-Za-z_][A-Za-z0-9_]*' "$1" |
		grep -oE '[A-Za-z_][A-Za-z0-9_]*$' | sort -u
}

# …and the ones it removes. `podman rmi` takes several tags on one line
# (tests/lake/phase3.sh removes its job and app images together), so every
# variable on the line counts.
tags_removed_in() { # tags_removed_in <file>
	grep -hE 'podman +rmi' "$1" |
		grep -oE '\$\{?[A-Za-z_][A-Za-z0-9_]*' |
		grep -oE '[A-Za-z_][A-Za-z0-9_]*$' | sort -u
}

# ---------------------------------------------------------------------------
log "1. every image a gate creates, the same gate removes"

created_total=0
leaks=""
while IFS= read -r f; do
	rel="${f#"${REPO_DIR}"/}"
	created="$(tags_created_in "$f")"
	[[ -z "$created" ]] && continue
	removed="$(tags_removed_in "$f")"
	while IFS= read -r var; do
		[[ -z "$var" ]] && continue
		created_total=$((created_total + 1))
		if ! grep -qx "$var" <<<"$removed"; then
			leaks+="${rel}: creates \$${var} and never \`podman rmi\`s it"$'\n'
		fi
	done <<<"$created"
done < <(harnesses)

none "no gate leaves its image tag behind" "${leaks%$'\n'}"

# The detector has to have found something, or the assertion above is vacuous —
# a typo in either pattern would report a clean tree over zero files.
if ((created_total >= 9)); then
	echo "  PASS  the scan found image-creating gates to check ($created_total)"
	pass=$((pass + 1))
else
	echo "  FAIL  the scan found only $created_total image-creating tags — expected >= 9;"
	echo "        tags_created_in has almost certainly stopped matching."
	fail=$((fail + 1))
fi

# ---------------------------------------------------------------------------
log "2. the ratchet is seen red"

# The claim of §1 is worth exactly as much as its ability to fail. A copy of a
# real gate with its `podman rmi` line deleted must be caught — otherwise a
# pattern that matches nothing passes §1 forever.
RED="$(mktemp -d /tmp/pd-images-red.XXXXXX)"
trap 'rm -rf "$RED"' EXIT
grep -v 'podman rmi' "${REPO_DIR}/tests/tenants/handles.sh" >"${RED}/leaky.sh"
check "a gate with its rmi line removed creates a tag" "IMAGE" "$(tags_created_in "${RED}/leaky.sh")"
check "…and removes none" "" "$(tags_removed_in "${RED}/leaky.sh")"

# ---------------------------------------------------------------------------
printf '\n=== %s ===\n' "RESULT"
echo "  $pass passed, $fail failed"
((fail == 0)) || exit 1
