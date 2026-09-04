#!/usr/bin/env bash
# ci-treewatch.sh — void a CI run whose worktree moved underneath it (pd-vnbc).
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
# On 2026-08-11 a polecat's `deploy/ci.sh` died at `cargo clippy` with seven
# compile errors naming symbols that existed nowhere in the branch under test.
# The errors were mutually inconsistent: two lines came from master's data.rs,
# a third carried a call signature from before the landing zone. No single
# commit in the repository has ever looked like that. `cargo test` had passed
# on the same tree minutes earlier and clippy passes on it now.
#
# The git reflog, timestamped inside the CI window, had the answer:
#
#   082c9e0 HEAD@{18:15:08}: rebase (start): checkout origin/master
#   154bf1b HEAD@{18:15:09}: rebase (pick): docs: the retention number...
#   dd00201 HEAD@{18:15:09}: rebase (abort): returning to refs/heads/polecat/...
#
# Something outside the polecat rebased its LIVE worktree onto origin/master,
# replayed eleven commits, hit a conflict and aborted — inside about one
# second, while clippy was reading the files. clippy compiled a tree that was
# half master and half the branch. The abort put everything back, which is why
# it never reproduced and read as a phantom.
#
# No step of this suite moves this checkout's HEAD — the two gates that drive
# git destructively (`ci-cache.sh --self-test`, `tests/ci/treewatch_test.sh`)
# do it inside throwaway repos they create with `mktemp -d`, and §10 of that
# gate holds the rule so this sentence cannot quietly stop being true. So the
# actor is the surrounding agent machinery, and it cannot be fixed from here.
# What CAN be fixed from here is the forty minutes the next person spends
# believing the compiler.
#
# ── WHAT IT WATCHES, AND WHY THAT AND NOT SOMETHING ELSE ────────────────────
#
# 1. THE REFLOG, NOT THE HEAD SHA. The observed injury was a rebase that
#    ABORTED, so HEAD ended exactly where it started; comparing HEAD at the
#    start of the run with HEAD at the end sees nothing at all. The reflog is
#    the only durable record that anything happened, and it is append-only, so
#    an operation that undoes itself still leaves its two lines behind.
#    tests/ci/treewatch_test.sh §3 is that exact case, and it is the one case
#    that separates this guard from the obvious wrong version of it.
#
# 2. BY BYTE LENGTH OF THE PER-WORKTREE REFLOG FILE. O(1), so it is affordable
#    at every step boundary, which is what narrows "the tree moved" down to a
#    named step rather than a forty-minute window. The file is per worktree —
#    `git rev-parse --git-path logs/HEAD` resolves it correctly for a linked
#    worktree, which every polecat checkout is — so a rebase in a NEIGHBOURING
#    worktree does not, and must not, trip this.
#
# 3. THE WORKING TREE'S DIRT IS REPORTED, NEVER JUDGED. A CI run legitimately
#    writes to its own checkout: `cargo test` regenerates the ts-rs bindings,
#    npm populates node_modules. Failing on `git status` would hand this guard
#    a false positive on its first contact with real work, which is how a
#    guard earns an exemption list and then earns being ignored. Dirt is
#    printed as context when the reflog has ALREADY moved, and at no other
#    time.
#
# 4. A RUN THAT STARTS MID-REBASE DOES NOT START. There is no coherent tree to
#    test and no result worth waiting for.
#
# What this does not see: a bare `git checkout -- <path>` or `git restore`,
# which rewrite files without touching HEAD. Said out loud rather than rounded
# off. Everything that moves HEAD — rebase, checkout, reset, merge, and
# `git stash push`, which resets internally — appends here and is caught.
#
# ── WHAT IT DOES ABOUT IT ───────────────────────────────────────────────────
# Aborts, at the first step boundary after the movement, with exit 9.
#
# Not "warns and carries on": once HEAD has moved, the steps that ALREADY RAN
# are the ones whose results cannot be trusted, and no amount of further
# running recovers them. And not "reports the step that failed": a red from a
# mutated tree is not a red, and a GREEN from one is worse. Exit 9 says the
# run describes nothing — a third answer, distinct from pass and from fail,
# because the first question it should provoke ("what moved my worktree?") is
# different from the first question either of those provoke.
#
# Usage:
#   . "${SCRIPT_DIR}/ci-treewatch.sh"
#   pkdump_treewatch_begin
#   pkdump_treewatch_check "before step foo" || exit "$PKDUMP_TREEWATCH_EXIT"
#   pkdump_treewatch_verdict "$rc"    # in the EXIT trap; prints the banner

# The third answer. One spelling, shared by ci.sh and by the gate that asserts
# on it.
PKDUMP_TREEWATCH_EXIT=9

# Unswallowable if diagnostics.sh is loaded, plain stderr if it is not. This
# guard's whole value is being READ, and ci.sh's steps redirect freely.
declare -F diag >/dev/null 2>&1 || diag() { printf '%s\n' "$*" >&2; }

PKDUMP_TREEWATCH_ACTIVE=""
PKDUMP_TREEWATCH_LOG=""
PKDUMP_TREEWATCH_SIZE=0
PKDUMP_TREEWATCH_HEAD=""
PKDUMP_TREEWATCH_REF=""
PKDUMP_TREEWATCH_TRIPPED=""

# Length of this worktree's HEAD reflog, or 0 where there is none. ONE spelling,
# spent by begin and by every check: two would be two chances for the baseline
# and the comparison to measure subtly different things, and the guard would
# then be wrong in whichever direction nobody tested. `2>/dev/null` after the
# redirect does not silence a redirect that itself fails, hence the -f test.
pkdump_treewatch_size() {
    if [ -f "$PKDUMP_TREEWATCH_LOG" ]; then wc -c < "$PKDUMP_TREEWATCH_LOG"; else echo 0; fi
}

# Is a rebase/merge/bisect half-finished right now? Names it, or prints nothing.
pkdump_treewatch_in_progress() {
    local d
    for d in rebase-merge rebase-apply MERGE_HEAD CHERRY_PICK_HEAD BISECT_LOG; do
        if [ -e "$(git rev-parse --git-path "$d" 2>/dev/null)" ]; then printf '%s ' "$d"; fi
    done
    return 0
}

# Arm the watch. Refuses a checkout that is already mid-rebase; skips, loudly
# and successfully, anywhere there is no git worktree to watch.
pkdump_treewatch_begin() {
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "    treewatch: not a git worktree — nothing to watch"
        return 0
    fi

    local busy
    busy="$(pkdump_treewatch_in_progress)"
    if [ -n "$busy" ]; then
        diag ""
        diag "!! REFUSING TO START: this checkout is mid-operation (${busy% })."
        diag "!! There is no coherent tree here to test. Finish or abort it first."
        return 1
    fi

    PKDUMP_TREEWATCH_LOG="$(git rev-parse --git-path logs/HEAD)"
    # Absent is a legitimate starting state (core.logAllRefUpdates off, or a
    # brand-new worktree). Recorded as zero, so a reflog that APPEARS during
    # the run reads as movement — which it is.
    PKDUMP_TREEWATCH_SIZE="$(pkdump_treewatch_size)"
    PKDUMP_TREEWATCH_HEAD="$(git rev-parse HEAD 2>/dev/null || echo '<none>')"
    PKDUMP_TREEWATCH_REF="$(git symbolic-ref --short -q HEAD || echo '<detached>')"
    PKDUMP_TREEWATCH_ACTIVE=1
    PKDUMP_TREEWATCH_TRIPPED=""
    echo "    treewatch: armed on ${PKDUMP_TREEWATCH_REF} @ ${PKDUMP_TREEWATCH_HEAD:0:12}"
}

# Render the reflog bytes appended since begin, one readable line each.
pkdump_treewatch_entries() {
    local line meta msg ts new n idx
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        meta="${line%%$'\t'*}"
        # The very first entry a worktree writes carries no message and so no
        # tab; `${line#*\t}` would then echo the whole line back.
        if [ "$line" = "$meta" ]; then msg="(no message)"; else msg="${line#*$'\t'}"; fi
        # meta is: <old> <new> <name> <email> <unix-ts> <tz>
        # shellcheck disable=SC2086
        set -- $meta
        new="$2"; n=$#; idx=$((n - 1)); ts="${!idx}"
        printf '!!     %s  %s  %s\n' \
            "$(date -u -d "@${ts}" '+%Y-%m-%d %H:%M:%SZ' 2>/dev/null || echo "$ts")" \
            "${new:0:12}" "$msg"
    done < <(tail -c "+$((PKDUMP_TREEWATCH_SIZE + 1))" "$PKDUMP_TREEWATCH_LOG" 2>/dev/null)
}

# 0 if the worktree has not moved, 1 if it has. Prints the evidence once — the
# same reflog excerpt that took a human an afternoon to go and find.
pkdump_treewatch_check() {
    [ -n "$PKDUMP_TREEWATCH_ACTIVE" ] || return 0
    [ -z "$PKDUMP_TREEWATCH_TRIPPED" ] || return 1

    local now
    now="$(pkdump_treewatch_size)"
    if [ "$now" = "$PKDUMP_TREEWATCH_SIZE" ]; then return 0; fi

    PKDUMP_TREEWATCH_TRIPPED=1
    local head_now ref_now busy dirt
    head_now="$(git rev-parse HEAD 2>/dev/null || echo '<none>')"
    ref_now="$(git symbolic-ref --short -q HEAD || echo '<detached>')"
    busy="$(pkdump_treewatch_in_progress)"
    dirt="$(git status --porcelain 2>/dev/null | wc -l)"

    diag ""
    diag "!! ================================================================"
    diag "!! THE GIT WORKTREE MOVED UNDERNEATH THIS RUN  (pd-vnbc)"
    diag "!! ================================================================"
    diag "!!   detected : ${1:-unspecified}"
    diag "!!   branch   : ${PKDUMP_TREEWATCH_REF} -> ${ref_now}"
    diag "!!   HEAD     : ${PKDUMP_TREEWATCH_HEAD:0:12} -> ${head_now:0:12}"
    if [ "$PKDUMP_TREEWATCH_HEAD" = "$head_now" ]; then
        diag "!!              (unchanged — so whatever moved it put it back, which"
        diag "!!               is exactly the case a HEAD comparison cannot see)"
    fi
    if [ -n "$busy" ]; then diag "!!   NOW MID-OPERATION: ${busy% }"; fi
    diag "!!   uncommitted files now: ${dirt}"
    diag "!!"
    diag "!!   HEAD reflog entries appended since this run began:"
    pkdump_treewatch_entries | while IFS= read -r l; do diag "$l"; done
    diag "!! ----------------------------------------------------------------"
    return 1
}

# The closing statement, for the EXIT trap. Takes the status the run would
# otherwise have reported, because "it also failed" and "it otherwise passed"
# need different last sentences and BOTH of them are void.
pkdump_treewatch_verdict() {
    local rc="${1:-0}"
    diag "!! This run's result describes no state this code was ever in."
    if [ "$rc" -eq 0 ]; then
        diag "!! It was about to report PASS. That pass is not evidence of anything:"
        diag "!! the gates that ran before the movement were reading a different tree."
    else
        diag "!! It failed, but NOT necessarily because of your change — a mutated"
        diag "!! tree compiles to errors that name symbols no commit ever had."
    fi
    diag "!!"
    diag "!! No step of this suite moves THIS checkout's HEAD — the gates that"
    diag "!! drive git do it inside throwaway repos under mktemp. So the process"
    diag "!! that moved it is outside the suite. Find it, stop it touching a"
    diag "!! checkout it does not own, then re-run this suite from the top."
    diag "!! Exiting ${PKDUMP_TREEWATCH_EXIT} — void, which is neither pass nor fail."
    diag "!! ================================================================"
}
