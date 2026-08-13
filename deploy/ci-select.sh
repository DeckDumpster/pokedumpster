#!/usr/bin/env bash
#
# Which CI tiers a set of changed paths requires (pd-s2mj, item 3 of pd-6onp).
#
# Sourced by deploy/ci.sh; also runnable directly, which is how
# tests/ci/select_test.sh asserts on it:
#
#   printf '%s\n' README.md | bash deploy/ci-select.sh      # -> lint
#   bash deploy/ci-select.sh --all-tiers                    # -> the full list
#
# ── THE RULE ────────────────────────────────────────────────────────────────
#
#   docs      (**.md, docs/**, wiki/**)  -> nothing beyond the always-on tiers
#   frontend  (frontend/**)              -> frontend + container + browser
#   anything else                        -> EVERYTHING
#
# Each changed path contributes its requirement and the requirements are
# UNIONED, so a PR touching a doc and a component runs the frontend set, and a
# PR touching a component and a crate runs everything. There is no subtraction
# anywhere in this file: a path can only ever add tiers.
#
# ── IT FAILS CLOSED, AND THAT IS THE WHOLE DESIGN ───────────────────────────
#
# Exactly two patterns are recognised. Everything else — a new top-level
# directory, a Cargo.toml, a workflow file, a path this file has never heard of
# — lands in the default arm and runs the full suite. Adding a bucket is a
# deliberate edit here with a test beside it; the cost of forgetting is a slow
# CI run, never a missed gate.
#
# The same rule covers the degenerate inputs. No paths at all (an empty list, a
# diff that failed, a caller that passed nothing) is not "nothing to test", it
# is "we do not know what changed" — and that runs everything.
#
# ── WHY frontend/** IS NOT SUBDIVIDED (pd-tf4h) ─────────────────────────────
#
# The obvious refinement — routes trigger the browser tier, everything else
# under frontend/ does not — is exactly the mistake pd-tf4h already cost this
# repo once: visual baselines were not re-recorded alongside a change because
# the change did not look like it touched the UI. It did. A design token, a
# shared component in $lib/components/ui/, a rule in tokens.css or app.css
# repaints every route at once, which is the widest possible screenshot diff
# and the one least likely to be predicted from the file name.
#
# So the frontend bucket is whole-directory and always carries the browser
# tier. Any change under frontend/ screenshots every route. The container tier
# rides along because the browser tier drives a real instance and the SvelteKit
# build is baked into the image — screenshotting the previous build would be
# worse than not screenshotting at all.
#
# ── SELECTION IS OPT-IN, PER RUN ────────────────────────────────────────────
#
# deploy/ci.sh runs every tier unless a caller hands it an explicit list of
# changed paths. A developer, a polecat, `workflow_dispatch`, and any future
# push-triggered run therefore get the full suite by construction: skipping
# something takes an affirmative act, and the act is auditable in the log.

# The canonical tier list, in the order deploy/ci.sh executes them. This is the
# single source of truth for the names; ci.sh is asserted against it by
# tests/ci/select_test.sh, so a tier renamed here and not there fails in a
# second rather than silently never running.
PKDUMP_CI_ALL_TIERS="lint rust deploy frontend container litestream tenants browser schema lake refresh"

# Always, whatever changed. Hermetic, no compilation, seconds: the shell-harness
# self-tests and `cargo fmt --check`. These are the "lint tiers only" a
# docs-only PR runs — and they are what makes a docs-only run still capable of
# going red.
PKDUMP_CI_ALWAYS_TIERS="lint"

# What a change under frontend/ can reach. See the pd-tf4h note above before
# taking anything out of this list.
PKDUMP_CI_FRONTEND_TIERS="frontend container browser"

# What a change under lake/ can reach. The PYTHON tree only: it builds its own job
# image from lake/Containerfile and lake/pyproject.toml, and nothing outside it
# consumes it — the `pkdump_lake` references in crates/ are the RUST crate
# `pkdump-lake`, which is a different thing and lives under crates/.
#
# All four lake gates sit behind `if tier lake` in deploy/ci.sh — run.sh, prices.sh,
# value_snapshots.sh and derive.sh — so this bucket runs every test that exercises
# the lakehouse, including the Rust offline derive it does not strictly need.
# Conservative on purpose: the cheap direction to be wrong in.
#
# NOT extended to crates/pkdump-lakehouse/: the app Containerfile BUILDS and SHIPS
# pkdump-lake-derive, and pkdump-cli and pkdump-derive both depend on the
# pkdump-lake crate. A change there reaches the app image, so it still runs
# everything until that coupling is actually broken.
PKDUMP_CI_LAKE_TIERS="lake"

# Print the canonical list, one tier per line.
pkdump_ci_all_tiers() {
    # shellcheck disable=SC2086  # word splitting is the point
    printf '%s\n' $PKDUMP_CI_ALL_TIERS
}

# Classify ONE path: prints `docs`, `frontend`, or `other`.
#
# `other` is the default arm, and every unrecognised path reaches it.
pkdump_ci_classify_path() {
    local path="$1"
    # `git diff --name-only` emits repo-relative paths, but a caller composing
    # the list by hand may not; normalise the one harmless prefix and let
    # anything else stay unrecognised.
    path="${path#./}"
    case "$path" in
        # Documentation, at any depth. A .md file under frontend/ is still a
        # .md file — it is read by people, not by vite.
        *.md | docs/* | wiki/*) printf 'docs\n' ;;
        frontend/*) printf 'frontend\n' ;;
        # Checked after *.md, so lake/README.md is still docs.
        lake/*) printf 'lake\n' ;;
        *) printf 'other\n' ;;
    esac
}

# Read changed paths on stdin, one per line; print the selected tiers, one per
# line, in canonical order.
#
# A path containing a newline splits into two lines here, neither of which
# matches a bucket — so it runs everything, which is the correct answer for a
# path this file cannot read.
pkdump_ci_select_tiers() {
    local selected=" ${PKDUMP_CI_ALWAYS_TIERS} "
    local saw_path=0 run_all=0 path t

    while IFS= read -r path; do
        [ -n "$path" ] || continue
        saw_path=1
        case "$(pkdump_ci_classify_path "$path")" in
            docs) ;;
            frontend) selected="${selected}${PKDUMP_CI_FRONTEND_TIERS} " ;;
            lake) selected="${selected}${PKDUMP_CI_LAKE_TIERS} " ;;
            # Deliberately no early exit: stdin is drained so a producer on the
            # other end of a pipe never takes a SIGPIPE for telling us the
            # truth.
            *) run_all=1 ;;
        esac
    done

    # Nothing at all is not "nothing to test" — it is "we do not know".
    [ "$saw_path" -eq 1 ] || run_all=1

    if [ "$run_all" -eq 1 ]; then
        pkdump_ci_all_tiers
        return 0
    fi

    # The browser tier drives the container the container tier starts. Nothing
    # in the buckets above can currently violate that, which is exactly when an
    # invariant is worth writing down — the next edit to a bucket list is the
    # one that would.
    case "$selected" in
        *" browser "*)
            case "$selected" in
                *" container "*) ;;
                *)
                    echo "ci-select: browser selected without container — refusing to guess" >&2
                    return 1
                    ;;
            esac
            ;;
    esac

    for t in $PKDUMP_CI_ALL_TIERS; do
        case "$selected" in
            *" $t "*) printf '%s\n' "$t" ;;
        esac
    done
}

# Is a tier in the selection? Reads $PKDUMP_CI_SELECTED, space-separated, which
# the caller sets from pkdump_ci_select_tiers.
#
#   0  selected — run it
#   1  not selected — skip it
#   2  NOT A TIER AT ALL
#
# The third answer is why this is a function rather than a `case`. A guard
# written against a tier name that does not exist would otherwise read as "not
# selected" and skip its gate on every run forever, which is precisely the
# failure this whole mechanism has to be incapable of. The caller is expected to
# treat 2 as fatal; deploy/ci.sh does, and tests/ci/select_test.sh asserts it.
pkdump_ci_tier_selected() {
    local tier="$1"
    case " $PKDUMP_CI_ALL_TIERS " in
        *" $tier "*) ;;
        *) return 2 ;;
    esac
    case " ${PKDUMP_CI_SELECTED:-} " in
        *" $tier "*) return 0 ;;
    esac
    return 1
}

# CLI mode. Sourcing this file defines the functions and runs nothing.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --all-tiers) pkdump_ci_all_tiers ;;
        "" | --tiers) pkdump_ci_select_tiers ;;
        *)
            echo "usage: ci-select.sh [--tiers|--all-tiers]   (paths on stdin)" >&2
            exit 2
            ;;
    esac
fi
