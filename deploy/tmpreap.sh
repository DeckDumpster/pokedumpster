#!/usr/bin/env bash
#
# Reap abandoned Claude Code session scratchpads (pd-xgh6).
#
# Every Claude Code session gets a private directory
#
#     $TMPDIR/claude-<uid>/<cwd-slug>/<session-uuid>/{scratchpad,tasks}
#
# and NOTHING ever collects it. On the deployment box that grew to 42G of a 49G
# filesystem: 2261 session directories against 46 running claude processes
# holding FIVE distinct session ids. It is not a leak in any one rig — the
# consumers are every agent on the machine — and it is not visible from any
# rig's own tree, which is why it went uncollected for months.
#
# WHY THIS LIVES IN POKEDUMPSTER, which owns none of that data:
#
#   * /tmp is its own LVM volume here, so neither $HOME nor the container store
#     can see it fill. pd-20ia is what made deploy/ci.sh measure it, and at 817M
#     free ci.sh correctly REFUSES TO START — CI on this box is blocked by a
#     filesystem no pokedumpster process writes more than a few megabytes to.
#   * this repo already owns the host-wide disk units on this box.
#     pkdump-diskcheck is explicitly not per-instance and not about
#     pokedumpster's data either; it is the same job one step earlier (say the
#     disk is filling) and this is the step that stops it filling.
#   * there is no rig here that owns the reaper. Re-filing it at the town level
#     is how it stays unwritten.
#
# It is installed with the rest of the host-wide units and therefore behind
# pd-onyd's entitlement guard: a polecat worktree running setup.sh cannot arm
# it, only the checkout that deploys this box can.
#
# ── WHAT IT WILL AND WILL NOT REMOVE ────────────────────────────────────────
#
# A session directory is reaped only when ALL THREE hold:
#
#   1. its name is a session UUID, and it sits at exactly <root>/<slug>/<uuid>.
#      Anything else under the root is left alone and counted — the root also
#      holds unrelated caches (uv-cache-<agent>, 579M of it here), and a reaper
#      that decided what those were would be a different, worse tool.
#   2. NO LIVE PROCESS holds that session id. Read from the process table
#      (CLAUDE_CODE_SESSION_ID in the environment, a uuid on the command line
#      such as `--resume <id>`, or a cwd inside the directory) — never from a
#      timestamp, because a long-running session can sit quiet for days.
#   3. nothing inside it has been modified since the cutoff, PKDUMP_TMPREAP_AGE_DAYS
#      days ago (3).
#      Redundant with (2) by design: it is the margin for a session that has
#      started and not yet exported its id, and for whatever the process table
#      cannot see.
#
# THIS DOES NOT COST ANYBODY A --resume. The bead assumed it did; on this box it
# does not, and the difference is which directory holds what. The transcript is
# `~/.claude/projects/<slug>/<session>.jsonl`, and the persisted tool-result
# bodies are in `<slug>/<session>/tool-results/` BESIDE it, under $HOME. What is
# under $TMPDIR is `scratchpad/` and `tasks/*.output` — the working files of a
# process that is running. Resuming a session reads none of it. Verified before
# this was written; if that layout ever changes, this script is wrong and the
# check in (2) is not what saves you.
#
# ── THE VACUITY GUARD ───────────────────────────────────────────────────────
#
# The liveness set is the only thing standing between this and deleting a
# running agent's working directory, so a run that CANNOT BUILD ONE removes
# nothing and exits 1. Concretely: claude processes exist and not one of them
# yielded a session id. That is indistinguishable from "every session is dead"
# by looking at the answer, which is exactly why it has to be asked as a
# separate question. An unreadable /proc is the same refusal.
#
# Usage:
#   tmpreap.sh              reap, print what went
#   tmpreap.sh --dry-run    print what WOULD go, remove nothing
#
# Env (host-wide ~/.config/pkdump/alerts.env, shared with diskcheck.sh):
#   PKDUMP_TMPREAP_ROOT       scratchpad root (default $TMPDIR/claude-<uid>)
#   PKDUMP_TMPREAP_AGE_DAYS   idle days before a dead session is reaped (3)
#   PKDUMP_TMPREAP_PROC       process table to read (default /proc)
#
# That last one is WHERE the process table is, never WHETHER to consult one:
# there is deliberately no way to hand this script a liveness set, or to tell it
# to skip the check. tests/deploy/run.sh §17 builds a fake /proc, which is the
# only way to state "a live session survives" and "a broken signal refuses" as
# tests rather than as comments — the real one has this box's own sessions in it.
#
# Exit: 0 ran (whether or not it reclaimed anything), 1 refused to act.
set -euo pipefail

ALERTS_ENV="${PKDUMP_ALERTS_ENV:-${HOME}/.config/pkdump/alerts.env}"
[ -f "$ALERTS_ENV" ] && { set -a; . "$ALERTS_ENV"; set +a; }

ROOT="${PKDUMP_TMPREAP_ROOT:-${TMPDIR:-/tmp}/claude-$(id -u)}"
ROOT="${ROOT%/}"
AGE_DAYS="${PKDUMP_TMPREAP_AGE_DAYS:-3}"
PROC="${PKDUMP_TMPREAP_PROC:-/proc}"

# The grace window as an INSTANT, resolved once, rather than as `find -mtime
# -$AGE_DAYS`. `-mtime -0` is not "no grace at all": GNU find's day arithmetic
# makes it match a directory created seconds ago and not one created two minutes
# ago, so the one setting an operator would reach for to say "reap everything
# dead" behaves differently depending on how fresh "fresh" is. A cutoff has no
# such edge — nothing is newer than now — and it is also the thing worth
# printing, since "modified since <timestamp>" is a claim somebody can check.
CUTOFF="$(date -d "${AGE_DAYS} days ago" +%Y-%m-%dT%H:%M:%S 2>/dev/null || true)"
[ -n "$CUTOFF" ] || {
    echo "tmpreap: PKDUMP_TMPREAP_AGE_DAYS='${AGE_DAYS}' is not a number of days" >&2
    exit 1
}

DRY_RUN=0
case "${1:-}" in
    --dry-run) DRY_RUN=1 ;;
    "") ;;
    *) echo "tmpreap: unknown argument '$1' (only --dry-run)" >&2; exit 1 ;;
esac

# A session id, and the only name this script will ever remove.
UUID_RE='[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'

if [ ! -d "$ROOT" ]; then
    echo "tmpreap: no scratchpad root at ${ROOT} — nothing to do"
    exit 0
fi

# --- The liveness set -------------------------------------------------------
#
# Three signals, unioned. Any one of them keeps a session. /proc entries come
# and go while this loop runs, so every read is allowed to fail — what is NOT
# allowed is for all of them to fail silently, which is the guard below.

[ -d "$PROC" ] || { echo "tmpreap: no process table at ${PROC} — refusing to act" >&2; exit 1; }

# One pass, two answers, from the same read — asking the process table twice
# lets the count and the ids come from different moments, and the guard below
# is precisely a comparison between them. Lines are tagged: `claude` for a
# process that IS one, `id <uuid>` for a session something is holding.
# Every read here is best-effort — a pid can exit mid-loop, and /proc/1 belongs
# to root — so the redirection itself is what has to be silenced, not just the
# command's stderr. `cmd < f 2>/dev/null` does not do that: the shell reports
# the failed open before the second redirection is in effect.
readnul() { { tr '\0' '\n' < "$1"; } 2>/dev/null || true; }

scan_proc() {
    local p argv
    for p in "$PROC"/[0-9]*; do
        [ -d "$p" ] || continue
        argv="$(readnul "${p}/cmdline")"
        # argv[0] only. The tmux and `sh -c` wrappers that START a session carry
        # `claude` deeper in their arguments and are not one; counting those
        # would make the guard fire on a box where nothing is running.
        case "${argv%%$'\n'*}" in
            claude|*/claude) echo claude ;;
        esac
        readnul "${p}/environ" |
            sed -n 's/^CLAUDE_CODE_SESSION_ID=/id /p' || true
        printf '%s\n' "$argv" | grep -oiE "$UUID_RE" | sed 's/^/id /' || true
        readlink "${p}/cwd" 2>/dev/null |
            grep -oiE "$UUID_RE" | sed 's/^/id /' || true
    done
}

SCAN="$(scan_proc)"
mapfile -t LIVE < <(printf '%s\n' "$SCAN" | sed -n 's/^id //p' | tr 'A-Z' 'a-z' | sort -u)
CLAUDE_PROCS="$(printf '%s\n' "$SCAN" | grep -c '^claude$' || true)"

if [ "${#LIVE[@]}" -eq 0 ] && [ "$CLAUDE_PROCS" -gt 0 ]; then
    echo "tmpreap: ${CLAUDE_PROCS} claude process(es) running but NOT ONE yielded a" >&2
    echo "  session id. The liveness signal is broken, and a broken one looks exactly" >&2
    echo "  like 'every session is dead'. Removing nothing." >&2
    exit 1
fi

declare -A IS_LIVE=()
for id in ${LIVE[@]+"${LIVE[@]}"}; do IS_LIVE["$id"]=1; done

echo "tmpreap: ${ROOT} — ${#LIVE[@]} live session(s), ${CLAUDE_PROCS} claude process(es), idle since ${CUTOFF}"

# --- The sweep --------------------------------------------------------------
#
# Confined to <root>/<slug>/<uuid>, one level of slug and one of session, and
# every candidate is re-checked against the root prefix and the name shape
# immediately before it is removed. A path that reaches the removal and fails
# either is FATAL, not skipped: it means the enumeration above disagrees with
# the confinement, and continuing would be acting on a disagreement.

reaped=0; kept_live=0; kept_fresh=0; skipped=0; bytes=0

while IFS= read -r -d '' dir; do
    name="$(basename "$dir")"
    if ! grep -qiE "^${UUID_RE}$" <<<"$name"; then
        skipped=$((skipped + 1))
        continue
    fi
    id="$(printf '%s' "$name" | tr 'A-Z' 'a-z')"
    if [ -n "${IS_LIVE[$id]:-}" ]; then
        kept_live=$((kept_live + 1))
        continue
    fi
    # Anything modified since the cutoff keeps the whole directory.
    if [ -n "$(find "$dir" -newermt "$CUTOFF" -print -quit 2>/dev/null)" ]; then
        kept_fresh=$((kept_fresh + 1))
        continue
    fi

    case "$dir" in
        "${ROOT}"/*/*/*) echo "tmpreap: BUG — ${dir} is below the session level" >&2; exit 1 ;;
        "${ROOT}"/*/*) : ;;
        *) echo "tmpreap: BUG — ${dir} is outside ${ROOT}" >&2; exit 1 ;;
    esac

    kb="$(du -sk "$dir" 2>/dev/null | cut -f1)"
    : "${kb:=0}"
    bytes=$((bytes + kb))
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "  would reap ${dir#"$ROOT"/} (${kb}K)"
    else
        rm -rf -- "$dir"
        echo "  reaped ${dir#"$ROOT"/} (${kb}K)"
    fi
    reaped=$((reaped + 1))
done < <(find "$ROOT" -mindepth 2 -maxdepth 2 -type d -print0 2>/dev/null)

# A slug directory with no sessions left is litter of the same kind; rmdir only,
# so one that still holds anything at all survives.
if [ "$DRY_RUN" -eq 0 ]; then
    find "$ROOT" -mindepth 1 -maxdepth 1 -type d -empty -exec rmdir {} + 2>/dev/null || true
fi

verb="reaped"; [ "$DRY_RUN" -eq 1 ] && verb="would reap"
printf 'tmpreap: %s %d session(s), %dM — kept %d live, %d recently used, %d not a session dir\n' \
    "$verb" "$reaped" "$((bytes / 1024))" "$kept_live" "$kept_fresh" "$skipped"
