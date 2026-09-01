#!/usr/bin/env bash
# Ratchet: a baseline approval covers every viewport (pd-tf4h, pd-4tce).
#
# §1-§2 drive the guard itself over the argument vectors a human actually
# types, in both directions — the refusals AND, at least as importantly, the
# approvals that must still go through. A guard whose first contact with real
# work is a false positive is a guard that gets worked around, and the way it
# would be worked around here is by invoking `npx playwright test` directly.
#
# §3-§5 state the rule over the TREE, because the failure mode is not the
# entry point that exists today: it is the second one somebody adds. The
# bypass this replaces was not a mistake in a script — it was a RECIPE IN THE
# README, recommending `--update-snapshots --project=desktop-1440` as the way
# to approve a subset. Anything that can execute Playwright must arrive
# through the guard, and nothing anywhere may spell the pair.
#
# Hermetic — no container, no network, no Playwright — so deploy/ci.sh runs it
# in the lint tier beside tests/lib/images_test.sh.
#
#   bash tests/visual/approval_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/visual/approval-guard.sh
. "$SCRIPT_DIR/approval-guard.sh"

pass=0
fail=0
log() { printf '\n=== %s ===\n' "$*"; }
ok() {
	echo "  PASS  $1"
	pass=$((pass + 1))
}
bad() {
	echo "  FAIL  $1"
	[ $# -gt 1 ] && printf '          %s\n' "$2"
	fail=$((fail + 1))
}
none() { # none <label> <lines>
	if [ -z "$2" ]; then ok "$1"; else bad "$1" "$2"; fi
}
refuses() { # refuses <label> <args...>
	local label="$1"
	shift
	local out
	out=$(pkdump_visual_approval_guard "$@" 2>&1)
	if [ $? -eq 0 ]; then
		bad "$label — guard allowed it"
	elif ! printf '%s' "$out" | grep -q 'pd-4tce'; then
		bad "$label — refused without naming why" "$out"
	else
		ok "$label"
	fi
}
allows() { # allows <label> <args...>
	local label="$1"
	shift
	local out
	if out=$(pkdump_visual_approval_guard "$@" 2>&1); then
		ok "$label"
	else
		bad "$label — guard refused a legitimate run" "$out"
	fi
}

log "1. A viewport-restricted approval is refused, however it is spelled"
refuses "--update-snapshots --project=desktop-1440" \
	--update-snapshots --project=desktop-1440
refuses "--project first, then the update flag" \
	--project=mobile-768 --update-snapshots
refuses "the space-separated --project form" \
	--update-snapshots --project mobile-768
refuses "the -u short form" -u --project=desktop-1440
refuses "--update-snapshots=all" --update-snapshots=all --project=desktop-1440
refuses "--update-snapshots=changed" --update-snapshots=changed --project=mobile-768
# The recipe tests/visual/README.md used to carry, verbatim. This is the one
# case in §1 that is a regression test rather than a variation on it.
refuses "the README's retired subset recipe" \
	--update-snapshots --project=desktop-1440 -g collection

log "2. Everything else still runs"
# Genuinely no arguments — how deploy/ci.sh's browser step calls playwright.sh.
allows "a plain check with no arguments at all"
allows "a plain check" ""
allows "a check narrowed to one viewport" --project=desktop-1440
allows "a check narrowed to one route" -g collection
allows "an approval covering every viewport" --update-snapshots
allows "an approval narrowed to a ROUTE keeps the viewports in lockstep" \
	--update-snapshots -g collection
allows "an approval with a reporter flag" --update-snapshots --reporter=list
# Playwright's own "write nothing" mode. Refusing it would be the guard
# inventing a rule nobody asked for.
allows "--update-snapshots=none is not an approval" \
	--update-snapshots=none --project=desktop-1440
# `--project` with no value is malformed and Playwright says so better.
allows "a bare trailing --project is left to Playwright" --update-snapshots --project

log "3. Every APPROVAL in the tree arrives through the guard"
# Running `npx playwright test` directly is a documented convenience and stays
# one — a check bypasses nothing, because a check writes no baseline. What may
# not exist anywhere is a scripted path that WRITES baselines without passing
# through the guard first. tests/visual/playwright.sh is the sanctioned one; it
# is exempt because it is the thing that calls the guard.
invocations=$(grep -rn --include='*.sh' --include='*.json' --include='*.yml' --include='*.md' \
	-e 'playwright test' -e 'npx playwright' \
	"$REPO_DIR/tests" "$REPO_DIR/deploy" "$REPO_DIR/.github" 2>/dev/null |
	grep -v '/node_modules/' |
	grep -v "^${REPO_DIR}/tests/visual/playwright.sh:" |
	grep -v "^${REPO_DIR}/tests/visual/approval_test.sh:" |
	grep -v 'playwright install' |
	grep -e '--update-snapshots' -e ' -u ' || true)
none "no approval reaches Playwright except through playwright.sh" "$invocations"

# And the entry point that used to: `npm run approve` was `playwright test
# --update-snapshots`, undocumented, reachable from the same directory, and
# past the guard by construction.
approve=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['scripts'].get('approve',''))" \
	"$SCRIPT_DIR/package.json")
case "$approve" in
	*playwright.sh*) ok "npm run approve goes through playwright.sh" ;;
	*) bad "npm run approve goes through playwright.sh" "approve = $approve" ;;
esac

guard_call=$(grep -c 'pkdump_visual_approval_guard "\$@"' "$SCRIPT_DIR/playwright.sh")
if [ "$guard_call" = "1" ]; then
	ok "tests/visual/playwright.sh calls the guard with its own arguments"
else
	bad "tests/visual/playwright.sh calls the guard" "found $guard_call call(s)"
fi

# run.sh asks the same guard before it builds a container, three minutes
# upstream of playwright.sh. Not a second rule — the same function, called at
# the earliest point on that path, because a refusal an operator waits for an
# image build to receive is one they learn to route around.
guard_line=$(grep -n 'pkdump_visual_approval_guard' "$SCRIPT_DIR/run.sh" | tail -1 | cut -d: -f1)
build_line=$(grep -n 'deploy/setup.sh\|teardown.sh' "$SCRIPT_DIR/run.sh" | head -1 | cut -d: -f1)
if [ -n "$guard_line" ] && [ -n "$build_line" ] && [ "$guard_line" -lt "$build_line" ]; then
	ok "run.sh asks the guard before it stands anything up (line $guard_line < $build_line)"
else
	bad "run.sh asks the guard before it stands anything up" "guard=$guard_line build=$build_line"
fi

# ...and calls it BEFORE it spends anything. A guard that fires after `npm ci`
# and a Chromium download still refuses correctly, but it teaches the operator
# that the refusal is expensive, which is how a refusal gets routed around.
guard_line=$(grep -n 'pkdump_visual_approval_guard' "$SCRIPT_DIR/playwright.sh" | tail -1 | cut -d: -f1)
spend_line=$(grep -n 'npm ci\|npx playwright install\|npx playwright test' "$SCRIPT_DIR/playwright.sh" | head -1 | cut -d: -f1)
if [ -n "$guard_line" ] && [ -n "$spend_line" ] && [ "$guard_line" -lt "$spend_line" ]; then
	ok "the guard runs before the install (line $guard_line < $spend_line)"
else
	bad "the guard runs before the install" "guard=$guard_line spend=$spend_line"
fi

log "4. Nothing in the tree spells an approval beside a viewport filter"
# Including the documentation. The bypass this gate replaces WAS a doc recipe,
# so a gate that read only the scripts would have found nothing wrong.
#
# The unit is the LINE, which is the copy-pasteable form: prose is free to
# discuss `--project` and free to discuss approving, and only a line carrying
# both is a command somebody can run. That is deliberately not an exemption
# list — it is the shape of the thing being forbidden.
# CLAUDE.md is in scope for the same reason README.md is: it is read as
# instructions, and a recipe there reaches every agent that opens the repo.
pair=$(grep -rn --include='*.sh' --include='*.md' --include='*.json' --include='*.yml' \
	-e '--update-snapshots' -e ' -u ' \
	"$REPO_DIR/tests" "$REPO_DIR/deploy" "$REPO_DIR/frontend" "$REPO_DIR/.github" \
	"$REPO_DIR"/*.md 2>/dev/null |
	grep -v '/node_modules/' |
	grep -v "^${REPO_DIR}/tests/visual/approval_test.sh:" |
	grep -v "^${REPO_DIR}/tests/visual/approval-guard.sh:" |
	grep -- '--project' || true)
none "no approval carries --project" "$pair"

log "5. Both viewports have a baseline for every route"
# The stale-baseline half of pd-4tce is what §1 defends. This is the missing
# half: a route recorded at one viewport and never at the other. routes.spec.ts
# already fails on a +page.svelte with no routes.json entry; nothing compared
# the two baseline directories against each other.
projects=$(grep -o "name: '[a-z0-9-]*'" "$SCRIPT_DIR/playwright.config.ts" | sed "s/name: '//; s/'//")
if [ -z "$projects" ]; then
	bad "read the viewports out of playwright.config.ts"
else
	ok "viewports: $(echo "$projects" | tr '\n' ' ')"
	missing=""
	ids=$(python3 -c "import json,sys; print('\n'.join(r['id'] for r in json.load(open(sys.argv[1]))['routes']))" \
		"$SCRIPT_DIR/routes.json")
	for p in $projects; do
		for id in $ids; do
			[ -f "$SCRIPT_DIR/baselines/$p/$id.png" ] || missing="${missing}${missing:+$'\n'}$p/$id.png"
		done
	done
	none "every route has a baseline at every viewport" "$missing"

	# And the inverse: a PNG no route claims is an orphan left by a rename,
	# which routes.json's own $fields note warns about and nothing checked.
	orphans=""
	for p in $projects; do
		for f in "$SCRIPT_DIR/baselines/$p/"*.png; do
			stem=$(basename "$f" .png)
			echo "$ids" | grep -qx "$stem" || orphans="${orphans}${orphans:+$'\n'}$p/$stem.png"
		done
	done
	none "no orphaned baseline" "$orphans"
fi

printf '\n%s\n' "----------------------------------------"
echo "passed: $pass   failed: $fail"
[ "$fail" -eq 0 ] || exit 1
