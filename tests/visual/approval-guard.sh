#!/usr/bin/env bash
# A baseline approval covers every viewport. Sourced, never executed.
#
# ── WHY THIS EXISTS (pd-tf4h, pd-4tce) ──────────────────────────────────────
# Every route in routes.json is screenshotted at BOTH viewports, and a
# rendering change reaches both by default: a button that becomes a spacer is
# a spacer at 1440 and at 768. So an approval that names one project is not a
# smaller approval — it is a *stale baseline*, and the tier stays green on the
# branch that made it, because the run that would have failed is the one whose
# project was filtered out. The next branch to run the browser tier wears it.
#
# That is not hypothetical. pd-0o5m replaced the manual-price "$" button with
# a spacer for catalog printings and re-recorded the mobile-768 pair only.
# desktop-1440/card-detail-owned-vintage.png was left stale, the epic branch
# went red on it days later, and diagnosing it (pd-4tce) cost a P1 and a
# reproduction run — 128 pixels, identical across Playwright's retry, so not
# even a flake anybody could dismiss.
#
# The README used to *recommend* this: `--update-snapshots --project=desktop-1440`
# was the documented way to approve a subset. It is gone, because there was
# never anything to buy with it. Baselines are deterministic — back-to-back
# runs against one instance differ by zero pixels — so a full `--update`
# rewrites the unmoved PNGs with the bytes they already hold and `git status`
# shows nothing for them. A viewport filter therefore saves no review and no
# diff; its only effect is the half it leaves behind.
#
# Narrowing an approval to a ROUTE is a different thing and stays allowed:
# `-g collection` runs that route in every project, so the two viewports move
# together, which is the whole property being defended.
#
# Usage — first act of any script that invokes Playwright:
#   . "${SCRIPT_DIR}/approval-guard.sh"
#   pkdump_visual_approval_guard "$@"
#
# tests/visual/approval_test.sh proves the behaviour AND asserts on the tree,
# so a second entry point cannot reintroduce the bypass one reasonable-looking
# line at a time. Hermetic and sub-second; it runs in deploy/ci.sh's lint tier.

# Refuses a viewport-restricted approval. Silent for everything else.
pkdump_visual_approval_guard() {
	local approving=false project="" want_project=false a

	for a in "$@"; do
		if [ "$want_project" = true ]; then
			project="$a"
			want_project=false
			continue
		fi
		case "$a" in
			# `--update-snapshots=none` is Playwright's "write nothing" mode.
			# It is not an approval, so it is not this guard's business.
			--update-snapshots=none) ;;
			-u | --update-snapshots | --update-snapshots=*) approving=true ;;
			--project=*) project="${a#--project=}" ;;
			--project) want_project=true ;;
		esac
	done

	# A `--project` with no value is malformed; Playwright will say so better
	# than this guard would. Refuse only the thing this guard is here for.
	if [ "$approving" = true ] && [ -n "$project" ]; then
		cat >&2 <<-MSG
			ERROR: refusing to approve baselines for one viewport (--project=${project}).

			  A rendering change reaches every viewport, so an approval that names one
			  leaves the others stale — green on this branch, red on the next one to run
			  the browser tier. That is pd-4tce, and it cost a P1 to diagnose.

			  Approve every viewport:
			      bash tests/visual/run.sh --update

			  Narrowing to a ROUTE is fine — it keeps the viewports in lockstep:
			      bash tests/visual/run.sh --update -g collection

			  See tests/visual/approval-guard.sh for why there is nothing to buy here.
		MSG
		return 1
	fi

	return 0
}
