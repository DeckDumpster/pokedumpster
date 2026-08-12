#!/usr/bin/env bash
# CI tier selection gate (pd-s2mj, item 3 of pd-6onp).
#
# Selection makes CI cheaper by not running tiers a change cannot affect, so
# its failure mode is a gate that quietly stops running. That is the same shape
# as every bug this repo's harness tier exists to catch, and it is why the rule
# gets a test rather than a review.
#
# Four properties are worth more than the rest:
#
#   IT FAILS CLOSED — §1. An unrecognised path, a path that merely LOOKS like a
#   recognised one, an empty list, no list at all: every one of them runs the
#   full suite. This is asserted by enumerating answers, not by reading the
#   default arm and believing it.
#
#   THE BROWSER TIER STILL RUNS FOR SHARED CSS AND COMPONENTS — §3. pd-tf4h:
#   visual baselines went un-re-recorded because a change did not look like it
#   touched the UI. A selector that skipped screenshots for anything under
#   frontend/ that is not a route file would reintroduce that exactly.
#
#   MASTER AND EVERY OTHER CALLER GET EVERYTHING — §4/§5. deploy/ci.sh runs the
#   full suite unless handed a list, and .github/workflows/ci.yml only ever
#   hands it one for a pull_request. Both halves are asserted: the script's own
#   behaviour, and the workflow's gating condition.
#
#   THE NAMES HAVE NOT DRIFTED — §6. Every tier deploy/ci.sh guards on exists in
#   the canonical list, and every tier in the canonical list is guarded on
#   somewhere in ci.sh. A renamed tier that reached one file and not the other
#   would silently never run.
#
# Hermetic: no podman, no network, no compilation. Sub-second, so it runs in
# the lint tier beside the other harness self-tests.
#
#   bash tests/ci/select_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SELECT="${REPO_DIR}/deploy/ci-select.sh"
CI_SH="${REPO_DIR}/deploy/ci.sh"
WORKFLOW="${REPO_DIR}/.github/workflows/ci.yml"

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
log() { printf '\n=== %s ===\n' "$*"; }

ALL="$(bash "$SELECT" --all-tiers | tr '\n' ' ' | sed 's/ $//')"

# select <path>... -> the selected tiers, space-separated, in canonical order.
select_for() {
	printf '%s\n' "$@" | bash "$SELECT" | tr '\n' ' ' | sed 's/ $//'
}

# ---------------------------------------------------------------------------
log "1. It fails closed"

# The headline requirement: a path nothing recognises runs EVERYTHING.
check "an unrecognised new top-level path -> everything" \
	"$ALL" "$(select_for "quantum/widget.rs")"
check "a bare new top-level file -> everything" \
	"$ALL" "$(select_for "NOTICE")"
check "the workflow that runs CI is not special-cased -> everything" \
	"$ALL" "$(select_for ".github/workflows/ci.yml")"
check "the selector itself changing -> everything" \
	"$ALL" "$(select_for "deploy/ci-select.sh")"

# Near-misses. These are the ones a prefix match written in a hurry gets wrong,
# and each of them must land in the default arm rather than a bucket.
check "docs-something/ is not docs/ -> everything" \
	"$ALL" "$(select_for "docs-legacy/notes.txt")"
check "frontend-extra/ is not frontend/ -> everything" \
	"$ALL" "$(select_for "frontend-extra/app.js")"
check "a file named frontend is not the directory -> everything" \
	"$ALL" "$(select_for "frontend")"
check "notes.markdown is not a .md file -> everything" \
	"$ALL" "$(select_for "notes.markdown")"
check "a file named docs is not the directory -> everything" \
	"$ALL" "$(select_for "docs")"
check "a path with a space in it, unrecognised -> everything" \
	"$ALL" "$(select_for "some dir/thing.rs")"

# Degenerate input is not "nothing to test", it is "we do not know what
# changed" — which is the loudest reason of all to run everything.
check "an empty list -> everything" "$ALL" \
	"$(printf '' | bash "$SELECT" | tr '\n' ' ' | sed 's/ $//')"
check "a list of blank lines -> everything" "$ALL" \
	"$(printf '\n\n\n' | bash "$SELECT" | tr '\n' ' ' | sed 's/ $//')"

# ---------------------------------------------------------------------------
log "2. Docs-only runs the lint tier and nothing else"

check "README.md alone -> lint only" "lint" "$(select_for "README.md")"
check "a nested .md -> lint only" "lint" "$(select_for "architecture/CARD_DATA_ACCESS.md")"
check "docs/ -> lint only" "lint" "$(select_for "docs/guide.md")"
check "wiki/ -> lint only" "lint" "$(select_for "wiki/projects/x.md")"
check "a non-md file under docs/ is still docs -> lint only" \
	"lint" "$(select_for "docs/diagram.svg")"
check "several docs at once -> lint only" \
	"lint" "$(select_for "README.md" "CLAUDE.md" "deep-dives/x.md")"
# A .md under frontend/ is read by people, not by vite.
check "a .md inside frontend/ -> lint only" "lint" "$(select_for "frontend/README.md")"

# The lint tier is what keeps a docs-only run capable of going red, so it is
# never absent from any answer.
check "lint is in every selection (docs)" "lint" "$(select_for "README.md" | cut -d' ' -f1)"
check "lint is in every selection (unrecognised)" "lint" "$(select_for "x/y" | cut -d' ' -f1)"

# ---------------------------------------------------------------------------
log "3. Frontend-only runs frontend + container + browser (pd-tf4h)"

FE="lint frontend container browser"
check "a route file" "$FE" "$(select_for "frontend/src/routes/browse/+page.svelte")"

# THE pd-tf4h REGRESSION CHECK. Baselines were once not re-recorded because the
# change did not look like a UI change. Each of these repaints every route at
# once, and each must still take the browser tier.
check "a design token (tokens.css)" "$FE" \
	"$(select_for "frontend/src/lib/styles/tokens.css")"
check "the global stylesheet (app.css)" "$FE" \
	"$(select_for "frontend/src/app.css")"
check "a shared UI primitive" "$FE" \
	"$(select_for "frontend/src/lib/components/ui/Panel.svelte")"
check "the root layout" "$FE" "$(select_for "frontend/src/routes/+layout.svelte")"
check "a lib module with no visual name at all" "$FE" \
	"$(select_for "frontend/src/lib/api.ts")"
check "a build config" "$FE" "$(select_for "frontend/vite.config.ts")"
check "a lockfile" "$FE" "$(select_for "frontend/package-lock.json")"

# And what frontend-only must NOT run.
for skipped in rust deploy litestream tenants schema lake refresh; do
	check "frontend-only skips '${skipped}'" "0" \
		"$(select_for "frontend/src/app.css" | tr ' ' '\n' | grep -cx "$skipped")"
done

# Unions, not overrides. A path can only ever add tiers.
check "docs + frontend -> the frontend set" "$FE" \
	"$(select_for "README.md" "frontend/src/app.css")"
check "frontend + a crate -> everything" "$ALL" \
	"$(select_for "frontend/src/app.css" "crates/pkdump-db/src/lib.rs")"
check "docs + a crate -> everything" "$ALL" \
	"$(select_for "README.md" "crates/pkdump-db/src/lib.rs")"
check "order does not matter" "$ALL" \
	"$(select_for "crates/pkdump-db/src/lib.rs" "README.md")"

# The visual baselines live OUTSIDE frontend/, so touching them is not a
# frontend-only change — and must not be treated as one.
check "a committed baseline -> everything" "$ALL" \
	"$(select_for "tests/visual/baselines/browse-1440.png")"

# The browser tier drives the instance the container tier starts, so a
# selection can never hold one without the other.
for p in "frontend/src/app.css" "crates/x.rs" "README.md"; do
	got="$(select_for "$p")"
	has_browser="$(printf '%s\n' $got | grep -cx browser)"
	has_container="$(printf '%s\n' $got | grep -cx container)"
	check "browser implies container (${p})" "0" \
		"$((has_browser > has_container ? 1 : 0))"
done

# ---------------------------------------------------------------------------
log "4. deploy/ci.sh runs everything unless told otherwise"

# The real script, not a copy of its rules. PKDUMP_CI_SELECT_ONLY exits before
# it touches podman or the disk, so this stays hermetic.
#
# `env -u` matters: this file runs from INSIDE ci.sh's lint tier, so a run that
# was itself given a changed-path list exports one into every child. Without the
# unset, "no list -> everything" quietly became "whatever the outer run
# selected" — it passed standalone and failed the moment CI ran it, which is how
# it was found.
plan() { # plan [changed-files path] -> the tiers ci.sh says it will RUN
	local list="${1:-}"
	if [ -n "$list" ]; then
		env -u PKDUMP_CI_SELECTED PKDUMP_CI_SELECT_ONLY=1 \
			PKDUMP_CI_CHANGED_FILES="$list" bash "$CI_SH" 2>&1
	else
		env -u PKDUMP_CI_SELECTED -u PKDUMP_CI_CHANGED_FILES \
			PKDUMP_CI_SELECT_ONLY=1 bash "$CI_SH" 2>&1
	fi | sed -n 's/^    RUN   //p' | tr '\n' ' ' | sed 's/ $//'
}

check "no changed-path list -> ci.sh runs every tier" "$ALL" "$(plan)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

printf 'README.md\n' > "${WORK}/docs.txt"
check "ci.sh with a docs-only list -> lint only" "lint" "$(plan "${WORK}/docs.txt")"

printf 'frontend/src/lib/styles/tokens.css\n' > "${WORK}/fe.txt"
check "ci.sh with a shared-CSS list -> frontend set" "$FE" "$(plan "${WORK}/fe.txt")"

printf 'quantum/widget.rs\n' > "${WORK}/unknown.txt"
check "ci.sh with an unrecognised list -> everything" "$ALL" "$(plan "${WORK}/unknown.txt")"

: > "${WORK}/empty.txt"
check "ci.sh with an EMPTY list -> everything" "$ALL" "$(plan "${WORK}/empty.txt")"

# A list named but missing is a broken caller, and a broken caller that got a
# silent full run would stay broken forever. Refuse instead.
env -u PKDUMP_CI_SELECTED PKDUMP_CI_SELECT_ONLY=1 \
	PKDUMP_CI_CHANGED_FILES="${WORK}/nope.txt" bash "$CI_SH" > /dev/null 2>&1
check "ci.sh refuses a changed-path list that does not exist" "1" "$?"

# ---------------------------------------------------------------------------
log "5. Selection is reachable ONLY from a pull request"

# The guarantee that a push to master cannot be made selective: ci.sh only ever
# selects when PKDUMP_CI_CHANGED_FILES is set, and the one place that sets it
# is gated on the event being a pull_request.
check "ci.yml sets the changed-path list exactly once" "1" \
	"$(grep -c 'PKDUMP_CI_CHANGED_FILES=' "$WORKFLOW")"
# ...and it is gated inside the SAME step. A matching `if:` anywhere else in the
# file would satisfy a bare grep while gating nothing, so the step is cut out
# and both lines looked for inside it.
STEP="$(awk '
	/^      - / { if (found) exit; block = "" }   # a new step: keep the one we wanted
	            { block = block $0 "\n" }
	/PKDUMP_CI_CHANGED_FILES=/ { found = 1 }
	END         { printf "%s", block }
' "$WORKFLOW")"
check "the step that sets it is gated on pull_request, in that same step" "1" \
	"$(printf '%s' "$STEP" | grep -c "if: github.event_name == 'pull_request'")"
# Nothing else in the tree may hand ci.sh a selection.
check "nothing under deploy/ sets a changed-path list" "0" \
	"$(grep -rl 'PKDUMP_CI_CHANGED_FILES=' "${REPO_DIR}/deploy" 2>/dev/null | grep -cv 'ci.sh$')"

# Both sides of a rename, or the bucket is computed from half the change: with
# rename detection on, moving frontend/Thing.svelte to docs/thing.md reports the
# destination alone and reads as a docs-only PR while the frontend lost a file.
check "the diff is taken with --no-renames" "1" \
	"$(printf '%s' "$STEP" | grep -c 'git diff --no-renames --name-only')"
# The selector's own half of that: with both sides listed, the answer is right.
check "a rename out of frontend/ still runs the frontend set" "$FE" \
	"$(select_for "frontend/src/lib/components/ui/Thing.svelte" "docs/thing.md")"

# ---------------------------------------------------------------------------
log "6. The tier names in ci.sh and ci-select.sh have not drifted"

# Every tier ci.sh guards on...
GUARDED="$(grep -oE '^ *if tier [a-z]+;' "$CI_SH" | awk '{print $3}' | tr -d ';' | sort -u)"
CANON="$(bash "$SELECT" --all-tiers | sort)"

check "every guarded tier is a real tier" "" \
	"$(comm -23 <(printf '%s\n' "$GUARDED") <(printf '%s\n' "$CANON") | tr '\n' ' ' | sed 's/ *$//')"
check "every real tier is guarded somewhere in ci.sh" "" \
	"$(comm -13 <(printf '%s\n' "$GUARDED") <(printf '%s\n' "$CANON") | tr '\n' ' ' | sed 's/ *$//')"

# A typo that read as "not selected" would skip a gate forever, so the library
# answers a third way for a name that is not a tier at all — and ci.sh treats
# that answer as fatal. Both halves asserted: the status, and the handling.
# shellcheck source=deploy/ci-select.sh
. "$SELECT"
PKDUMP_CI_SELECTED="$(pkdump_ci_all_tiers | tr '\n' ' ')"

pkdump_ci_tier_selected lint
check "a selected tier answers 0" "0" "$?"
PKDUMP_CI_SELECTED="lint" pkdump_ci_tier_selected rust
check "a real but unselected tier answers 1" "1" "$?"
pkdump_ci_tier_selected nosuchtier
check "a name that is not a tier answers 2, not 1" "2" "$?"

# And ci.sh acts on it. `tier` is defined inside that script, so this asserts on
# the shape of the guard it installs rather than re-deriving one here.
check "ci.sh exits on the not-a-tier answer" "1" \
	"$(grep -c 'which is not one of' "$CI_SH")"

# ---------------------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
