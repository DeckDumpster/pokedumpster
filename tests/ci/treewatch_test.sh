#!/usr/bin/env bash
# tests/ci/treewatch_test.sh — the guard that voids a run whose worktree moved.
#
# Hermetic, no container, sub-second. Runs in the LINT tier, so a docs-only
# change still proves the guard is wired and still capable of firing.
#
# The case this gate exists for is §3. pd-vnbc's rebase ABORTED, so HEAD ended
# byte-identical to where it started; the obvious implementation of this guard
# — snapshot HEAD, compare HEAD — passes that case green while the run it was
# watching compiled a tree half of master and half of the branch. §3 is that
# exact shape, and it is why the watch is over the reflog.
#
# §7 and §8 assert on the TREE rather than on behaviour: a guard that is
# perfect and unwired is a guard that does nothing, and the way this regresses
# is somebody simplifying `step()` and not noticing what came out of it.
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
FAILS=0
ok() {
    if [ "$2" = "$3" ]; then printf '  ok    %-58s %s\n' "$1" "$2"
    else printf '  FAIL  %-58s got=%s want=%s\n' "$1" "$2" "$3"; FAILS=$((FAILS + 1)); fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# A throwaway repo with a branch that will conflict on rebase, so §3 can drive
# a real rebase that really aborts rather than a simulation of one.
mkrepo() {
    local d="$1"
    git init -q "$d"
    (
        cd "$d" || exit 1
        git config user.email t@t && git config user.name t
        echo base > f && git add -A && git commit -qm base
        git checkout -qb feat && echo feat > f && git add -A && git commit -qm "feat side"
        git checkout -q master 2>/dev/null || git checkout -q main
        echo master > f && git add -A && git commit -qm "master side"
        git checkout -q feat
    )
}

# Run one scenario in its own shell: arm the watch, do <body>, report the
# check's verdict as moved/unmoved. A subshell per case keeps the latch and the
# recorded size from leaking between them.
scenario() {
    local dir="$1" body="$2"
    (
        cd "$dir" || exit 1
        # shellcheck source=deploy/ci-treewatch.sh
        . "${REPO_DIR}/deploy/ci-treewatch.sh"
        pkdump_treewatch_begin > /dev/null 2>&1 || { echo "begin-refused"; exit 0; }
        eval "$body" > /dev/null 2>&1
        if pkdump_treewatch_check "scenario" > /dev/null 2>&1; then echo unmoved; else echo moved; fi
    )
}

echo "== §1 an untouched checkout is not accused =="
# The most important negative in the file. A CI run writes to its own checkout
# constantly — cargo regenerates the ts-rs bindings, npm fills node_modules —
# and a guard that reads any of that as tampering is one that gets switched off
# in a week.
mkrepo "$TMP/quiet" > /dev/null 2>&1
ok "an idle worktree reports unmoved" "$(scenario "$TMP/quiet" 'true')" "unmoved"
ok "writing untracked build output is not movement" \
   "$(scenario "$TMP/quiet" 'mkdir -p target && echo x > target/o && echo y > f')" "unmoved"
ok "a long-running command that touches nothing is not movement" \
   "$(scenario "$TMP/quiet" 'cat f > /dev/null')" "unmoved"

echo ""
echo "== §2 an ordinary commit underneath the run IS movement =="
mkrepo "$TMP/commit" > /dev/null 2>&1
ok "a commit lands in the reflog and trips the watch" \
   "$(scenario "$TMP/commit" 'echo z > g && git add -A && git commit -qm sneaky')" "moved"

echo ""
echo "== §3 pd-vnbc's shape: a rebase that ABORTS, leaving HEAD where it was =="
mkrepo "$TMP/rebase" > /dev/null 2>&1
REBASE_BODY='git rebase master; git rebase --abort'
ok "a rebase that starts and aborts is caught" \
   "$(scenario "$TMP/rebase" "$REBASE_BODY")" "moved"

# ...and the same run, measured the way the naive guard would measure it. This
# assertion is what says §3 is not passing by accident.
mkrepo "$TMP/naive" > /dev/null 2>&1
NAIVE="$(
    cd "$TMP/naive" || exit 1
    before="$(git rev-parse HEAD)"
    git rebase master > /dev/null 2>&1
    git rebase --abort > /dev/null 2>&1
    after="$(git rev-parse HEAD)"
    [ "$before" = "$after" ] && echo unmoved || echo moved
)"
ok "a HEAD-sha comparison of that same rebase sees NOTHING" "$NAIVE" "unmoved"

# git stash resets internally, so it moves HEAD's reflog while leaving the sha
# alone — the shared-stash-stack hazard CLAUDE.md warns about, caught for free.
mkrepo "$TMP/stash" > /dev/null 2>&1
ok "git stash push is caught for the same reason" \
   "$(scenario "$TMP/stash" 'echo dirty > f && git stash push -m x')" "moved"

echo ""
echo "== §4 the diagnostic carries the evidence, not just the verdict =="
mkrepo "$TMP/eviD" > /dev/null 2>&1
EVID="$(
    cd "$TMP/eviD" || exit 1
    . "${REPO_DIR}/deploy/ci-treewatch.sh"
    pkdump_treewatch_begin > /dev/null 2>&1
    git rebase master > /dev/null 2>&1
    git rebase --abort > /dev/null 2>&1
    pkdump_treewatch_check "during cargo clippy" 2>&1
)"
ok "it names the step it was detected at" \
   "$(grep -qF 'during cargo clippy' <<<"$EVID" && echo yes || echo no)" "yes"
ok "it prints the reflog lines, which is the evidence" \
   "$(grep -qF 'rebase (abort)' <<<"$EVID" && echo yes || echo no)" "yes"
ok "it says HEAD came back, so the reader stops doubting the reflog" \
   "$(grep -qF 'put it back' <<<"$EVID" && echo yes || echo no)" "yes"
ok "it cites the bead" \
   "$(grep -qF 'pd-vnbc' <<<"$EVID" && echo yes || echo no)" "yes"

echo ""
echo "== §5 the latch, and the verdict's two endings =="
LATCH="$(
    cd "$TMP/eviD" || exit 1
    . "${REPO_DIR}/deploy/ci-treewatch.sh"
    pkdump_treewatch_begin > /dev/null 2>&1
    git commit -q --allow-empty -m x > /dev/null 2>&1
    pkdump_treewatch_check one > /dev/null 2>&1
    # A second ask must still be nonzero: the EXIT trap asks after step() has,
    # and a watch that forgot would let the trap report a clean run.
    pkdump_treewatch_check two > /dev/null 2>&1 && echo forgot || echo still-tripped
)"
ok "a tripped watch stays tripped for the EXIT trap" "$LATCH" "still-tripped"

VERD_PASS="$(. "${REPO_DIR}/deploy/ci-treewatch.sh"; pkdump_treewatch_verdict 0 2>&1)"
VERD_FAIL="$(. "${REPO_DIR}/deploy/ci-treewatch.sh"; pkdump_treewatch_verdict 1 2>&1)"
ok "a would-be PASS is told its green proves nothing" \
   "$(grep -qi 'about to report PASS' <<<"$VERD_PASS" && echo yes || echo no)" "yes"
ok "a red is told it may not be its own fault" \
   "$(grep -qi 'not necessarily because of your change' <<<"$VERD_FAIL" && echo yes || echo no)" "yes"
ok "the void status has one spelling and it is 9" \
   "$(. "${REPO_DIR}/deploy/ci-treewatch.sh"; echo "$PKDUMP_TREEWATCH_EXIT")" "9"

echo ""
echo "== §6 the edges =="
mkrepo "$TMP/busy" > /dev/null 2>&1
BUSY="$(
    cd "$TMP/busy" || exit 1
    git rebase master > /dev/null 2>&1   # leaves the rebase in progress
    . "${REPO_DIR}/deploy/ci-treewatch.sh"
    pkdump_treewatch_begin > /dev/null 2>&1 && echo started || echo refused
)"
ok "a checkout already mid-rebase refuses to start a run" "$BUSY" "refused"

mkdir -p "$TMP/nogit"
NOGIT="$(
    cd "$TMP/nogit" || exit 1
    . "${REPO_DIR}/deploy/ci-treewatch.sh"
    pkdump_treewatch_begin > /dev/null 2>&1 || { echo refused; exit 0; }
    pkdump_treewatch_check x > /dev/null 2>&1 && echo skipped || echo accused
)"
ok "somewhere with no git worktree it skips rather than refusing" "$NOGIT" "skipped"

# A linked worktree is what every polecat checkout is, and the reflog it must
# watch is its OWN. A neighbour rebasing must not void this run.
mkrepo "$TMP/wt" > /dev/null 2>&1
NEIGHBOUR="$(
    cd "$TMP/wt" || exit 1
    git worktree add -q -f "$TMP/wt-linked" master > /dev/null 2>&1
    cd "$TMP/wt-linked" || exit 1
    . "${REPO_DIR}/deploy/ci-treewatch.sh"
    pkdump_treewatch_begin > /dev/null 2>&1
    ( cd "$TMP/wt" && git commit -q --allow-empty -m neighbour ) > /dev/null 2>&1
    pkdump_treewatch_check x > /dev/null 2>&1 && echo unmoved || echo accused
)"
ok "a NEIGHBOURING worktree's commit does not void this one" "$NEIGHBOUR" "unmoved"

OWN="$(
    cd "$TMP/wt-linked" || exit 1
    . "${REPO_DIR}/deploy/ci-treewatch.sh"
    pkdump_treewatch_begin > /dev/null 2>&1
    git commit -q --allow-empty -m mine > /dev/null 2>&1
    pkdump_treewatch_check x > /dev/null 2>&1 && echo unmoved || echo moved
)"
ok "...but this worktree's own reflog still is watched" "$OWN" "moved"

echo ""
echo "== §7 deploy/ci.sh is actually wired to it =="
CI="${REPO_DIR}/deploy/ci.sh"
ok "ci.sh sources the library" \
   "$(grep -qE '^\. "\$SCRIPT_DIR/ci-treewatch\.sh"' "$CI" && echo yes || echo no)" "yes"
ok "ci.sh arms the watch" \
   "$(grep -qE 'pkdump_treewatch_begin' "$CI" && echo yes || echo no)" "yes"
# The check has to be INSIDE step(), not sprinkled at call sites: a new step is
# added by writing `step "..."`, and nobody adding one will remember a guard.
ok "the check lives inside step(), so every step boundary is covered" \
   "$(grep -qE 'pkdump_treewatch_check' <<<"$(awk '/^step\(\)/,/^}/' "$CI")" && echo yes || echo no)" "yes"
ok "the EXIT trap asks too, so the LAST step is covered as well" \
   "$(grep -qE 'pkdump_treewatch_(check|verdict)' <<<"$(awk '/^cleanup\(\)/,/^}/' "$CI")" && echo yes || echo no)" "yes"
ok "ci.sh exits the void status rather than a literal of its own" \
   "$(grep -qE 'exit "\$?\{?PKDUMP_TREEWATCH_EXIT' "$CI" && echo yes || echo no)" "yes"

echo ""
echo "== §8 the guard cannot be satisfied by a run that never armed it =="
# begin() prints the line below; its ABSENCE from a log is how you tell an old
# run from a watched one. Keep them one string.
ok "arming announces itself in the log" \
   "$(grep -qF 'treewatch: armed on' "${REPO_DIR}/deploy/ci-treewatch.sh" && echo yes || echo no)" "yes"
ok "ci.sh arms it BEFORE the first step() call" \
   "$(awk '/pkdump_treewatch_begin/{b=NR} /^step "/{if(!s)s=NR} END{print (b && s && b < s) ? "yes" : "no"}' "$CI")" "yes"

echo ""
echo "== §9 the REAL deploy/ci.sh arms it, executed rather than read =="
# §7 reads the source; a wiring that is present but unreachable would satisfy
# it. This drives the actual script. PKDUMP_CI_SELECT_ONLY prints the tier plan
# and exits touching nothing — no store, no disk check, no container — so it
# costs about a second, and it exercises pkdump_treewatch_begin for real.
#
# What is NOT asserted end-to-end, said out loud: that a mid-run movement makes
# the real ci.sh exit 9. Producing one deterministically means moving HEAD in
# the window between two step() calls of a live suite, and every way of forcing
# that window open is a bigger fake than the source assertions in §7. The
# library's own behaviour under that movement is §3 and §5.
ARMED="$(
    cd "$REPO_DIR" || exit 1
    env -u PKDUMP_CI_CHANGED_FILES PKDUMP_CI_SELECT_ONLY=1 bash deploy/ci.sh 2>&1
)"
ok "a real run announces the watch before doing anything" \
   "$(grep -qF 'treewatch: armed on' <<<"$ARMED" && echo yes || echo no)" "yes"
# NOT `… | head -1 | grep -qF …`. $ARMED is the whole of a real ci.sh run, and
# under `pipefail` a `grep -q` that exits on its first match SIGPIPEs the writer
# behind it — so the pipeline reports 141 precisely when the assertion HOLDS.
# It passed on master and failed on a branch carrying seven merges, because the
# only thing that changed was that the log outgrew a pipe buffer. Take the first
# line with a bash expansion and match it with a herestring: no pipe, nothing to
# SIGPIPE, no exit status borrowed from a writer that was killed for succeeding.
LANDMARKS="$(grep -nE 'treewatch: armed on|^==> ' <<<"$ARMED")"
ok "...and it is the FIRST thing in the log, ahead of every step" \
   "$(grep -qF 'treewatch' <<<"${LANDMARKS%%$'\n'*}" && echo yes || echo no)" "yes"

echo ""
echo "== §10 the banner's own claim, made mechanical =="
# When this guard fires it tells the reader "no step of this suite moves THIS
# checkout's HEAD, so the process that did is outside the suite". That sentence
# is the whole of the advice, and it is one careless commit from being false —
# at which point the guard sends people hunting a phantom, which is the bug it
# was written to end.
#
# The rule, stated so it cannot be satisfied by an exemption list: a file under
# deploy/ or tests/ that drives git DESTRUCTIVELY must also create a throwaway
# directory to do it in. Two files qualify today and both do; a third that
# rebases the live checkout would not.
DESTRUCTIVE='git rebase|git reset --hard|git reset -q --hard|git stash push|git checkout -qb|git checkout -b'
OFFENDERS=""
while IFS= read -r f; do
    [ -n "$f" ] || continue
    grep -qE "$DESTRUCTIVE" "$f" || continue
    grep -q 'mktemp -d' "$f" || OFFENDERS="${OFFENDERS}${f} "
done < <(find "${REPO_DIR}/deploy" "${REPO_DIR}/tests" -name '*.sh' -type f 2>/dev/null)
ok "every file that drives git destructively does it in a throwaway dir" \
   "${OFFENDERS:-none}" "none"

# ...and the rule has teeth: a file that drives git without one is named.
FAKE="$TMP/fakegate.sh"
printf '#!/usr/bin/env bash\ngit reset --hard origin/master\n' > "$FAKE"
ok "a gate that reset --hard with no throwaway dir would be caught" \
   "$(grep -qE "$DESTRUCTIVE" "$FAKE" && ! grep -q 'mktemp -d' "$FAKE" && echo caught || echo missed)" "caught"

echo ""
if [ "$FAILS" -eq 0 ]; then echo "ALL PASS"; exit 0; fi
echo "$FAILS FAILED"; exit 1
