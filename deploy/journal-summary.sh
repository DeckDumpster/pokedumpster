#!/usr/bin/env bash
#
# Turn a failed unit's journal tail into ONE readable page (pd-pwk8).
#
# Layer 2 (pkdump-alert@.service) used to pipe the raw journal tail straight
# into alert.sh, which kept the LAST 900 bytes. The last lines of a failed
# unit's journal are systemd's own boilerplate, and for a podman-backed unit
# the lines before those are podman's event log — a container id followed by
# every OCI label on the image. The real page Ryan got on 2026-08-12 read:
#
#   ...org.opencontainers.image.source=https://github.com/benbjohnson/lite...
#   backup-check: STALE — the user registry: newest S3 replica write is 65h old
#   pkdump-backup-check@prod.service: Main process exited, code=exited, status=1/FAILURE
#   pkdump-backup-check@prod.service: Failed with result 'exit-code'.
#   pkdump-backup-check@prod.service: Triggering OnFailure= dependencies.
#
# The one line that says WHAT WENT WRONG is in there, and on a phone it is not
# what you see. The budget was never the problem; WHICH 900 bytes was.
#
# So: drop the manager's boilerplate and podman's event log, keep the service's
# own stdout/stderr, and LEAD with the most informative line of it. The exit
# status stays, as a short suffix rather than as the body.
#
# Usage:
#   journalctl --user -u <unit> -n 80 -o short-iso --no-pager \
#       | journal-summary.sh <unit> | alert.sh "<title>"
#
#   journal-summary.sh <unit>      # stdin is a terminal: fetch the tail itself,
#                                  #   for "show me the page this WOULD send"
#
# Writes the message body to stdout and nothing else — alert.sh is what pushes,
# and it still reads its message on stdin. Both journalctl short formats are
# understood, and a tail with no recognisable prefixes at all (`-o cat`) still
# gets the content-based filtering.
#
# Every branch here ends in a page. That is the one place this repo's "let it
# crash visibly" rule inverts: the caller is a unit that has ALREADY failed, and
# a renderer that exits non-zero on a tail it did not expect turns a failure
# somebody needs to see into silence. So no `-e`: an unparseable journal, an
# empty one, a unit with no output at all — each still produces a message that
# names the unit and how it failed.
set -uo pipefail

UNIT="${1:-}"
if [ -z "$UNIT" ]; then
    echo "usage: journalctl ... | $(basename "$0") <unit>" >&2
    exit 2
fi

# How far back to look when we fetch the tail ourselves. The pipe form gets
# whatever the caller asked journalctl for; the unit asks for the same number.
LINES="${PKDUMP_ALERT_JOURNAL_LINES:-80}"

if [ -t 0 ]; then
    RAW="$(journalctl --user -u "$UNIT" -n "$LINES" -o short-iso --no-pager 2>/dev/null)"
else
    RAW="$(cat)"
fi

# The unit name as a human says it. The title already carries the full one.
SHORT="${UNIT%.service}"

# --- why it failed ----------------------------------------------------------
# Read off the boilerplate we are about to throw away. `Main` for a normal
# service, `Control` for a oneshot's ExecStart.
STATUS="$(printf '%s\n' "$RAW" |
    sed -n 's/.*\(Main\|Control\) process exited, code=exited, status=\([0-9]\{1,\}\).*/\2/p' |
    tail -n1)"
RESULT="$(printf '%s\n' "$RAW" |
    sed -n "s/.*Failed with result '\([a-z-]\{1,\}\)'.*/\1/p" | tail -n1)"

# A tail long enough to lose the boilerplate still has a live unit to ask —
# OnFailure= runs while the unit is still failed.
if [ -z "$STATUS" ] && [ -z "$RESULT" ] && command -v systemctl >/dev/null 2>&1; then
    STATUS="$(systemctl --user show "$UNIT" -p ExecMainStatus --value 2>/dev/null || true)"
    case "$STATUS" in '' | 0) STATUS="" ;; esac
fi

if [ -n "$STATUS" ]; then
    WHY=" (exit ${STATUS})"
elif [ -n "$RESULT" ]; then
    WHY=" (${RESULT})"
else
    WHY=""
fi

# --- the unit's OWN output --------------------------------------------------
# Two filters, deliberately overlapping. The structural one reads the syslog
# identifier out of journalctl's prefix: `systemd` is the manager talking about
# the unit, `podman` is the event log, `conmon` is the runtime's own cgroup
# warnings (which say "Failed to open" and would otherwise BE the headline), and
# everything else is the payload — including a Quadlet container, whose
# identifier is `systemd-<container>` and which must NOT be mistaken for the
# manager. The content one repeats the same
# judgement on the message text, so a tail piped in as `-o cat` (no prefix to
# read) is still filtered, and so a podman that logs its labels through some
# other identifier tomorrow does not put them back on the page.
#
# A run of near-identical lines collapses to one, counted. The sidecar prints a
# heartbeat every second and there is no version of "the last six of those" that
# is worth a fifth of the page.
KEPT="$(printf '%s\n' "$RAW" | awk '
function flush() {
    if (pending != "")
        print (repeats > 1) ? pending "  (x" repeats ")" : pending
    pending = ""; pendkey = ""; repeats = 0
}
{
    line = $0
    sub(/\r$/, "", line)
    if (line ~ /^-- /) next                       # journalctl own hints
    if (line ~ /^[ \t]*$/) next

    ident = ""
    msg = line
    # short-iso:  2026-08-12T17:02:06+0000 host ident[pid]: message
    # short:      Aug 12 17:02:06 host ident[pid]: message
    if (match(line, /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[^ ]+ [^ ]+ [^ ]+: /) ||
        match(line, /^[A-Z][a-z][a-z] +[0-9]+ [0-9][0-9]:[0-9][0-9]:[0-9][0-9] [^ ]+ [^ ]+: /)) {
        prefix = substr(line, 1, RLENGTH)
        msg = substr(line, RLENGTH + 1)
        n = split(prefix, f, " ")
        ident = f[n]
        sub(/:$/, "", ident)
        sub(/\[[0-9]+\]$/, "", ident)
    }

    if (ident == "systemd" || ident == "podman" || ident == "conmon") next

    # podman event log / image metadata, by content.
    if (msg ~ /org\.opencontainers\.|io\.buildah\.|io\.podman\./) next
    if (msg ~ /^[0-9-]+ [0-9:.]+ [^ ]+ UTC m=/) next     # a bare podman event

    # systemd boilerplate, by content.
    if (msg ~ /(Main|Control) process exited, code=/) next
    if (msg ~ /Failed with result .[a-z-]+./) next
    if (msg ~ /Triggering OnFailure=/) next
    if (msg ~ /Consumed .* CPU time/) next
    if (msg ~ /^(Started|Starting|Stopped|Stopping|Finished|Reached|Created slice|Removed slice|Deactivated successfully|Failed to start|Scheduled restart|Start request repeated)/) next

    gsub(/[[:cntrl:]]/, " ", msg)
    if (msg ~ /^[ \t]*$/) next
    # One runaway line must not spend the whole budget on its own.
    if (length(msg) > 240) msg = substr(msg, 1, 237) "..."

    key = msg
    gsub(/[0-9]/, "0", key)                       # a heartbeat differs only in its clock
    if (key == pendkey) { repeats++; next }
    flush()
    pending = msg; pendkey = key; repeats = 1
}
END { flush() }')"

# --- lead with the line that says what went wrong ---------------------------
# The service usually prints its verdict last, right before the manager reacts
# to it, so the last kept line is normally right. Normally is not always: the
# Litestream sidecar prints an INFO line every second, so its last line is a
# heartbeat and the failure is further up. Prefer the newest line that reads
# like a failure, and fall back to the newest line.
ERR_RE='error|fail|fatal|panic|denied|refus|unable|cannot|no such|not found|timed out|timeout|stale|missing|invalid|exception|traceback|abort|corrupt|unauthori[sz]ed|forbidden|expired'

HEADLINE="$(printf '%s\n' "$KEPT" | grep -Ei -- "$ERR_RE" | tail -n1)"
[ -n "$HEADLINE" ] || HEADLINE="$(printf '%s\n' "$KEPT" | grep -v '^[[:space:]]*$' | tail -n1)"

if [ -n "$HEADLINE" ]; then
    printf '%s FAILED%s — %s\n' "$SHORT" "$WHY" "$HEADLINE"
else
    # A unit that failed without saying anything is still a page worth reading:
    # it names the unit, the status, and the fact that there was nothing else.
    printf '%s FAILED%s — nothing but systemd boilerplate in its journal tail\n' \
        "$SHORT" "$WHY"
fi

# Context, oldest first, under the headline — whatever survives the 900-byte cap
# is a bonus, which is why it goes after and not before.
CONTEXT="$(printf '%s\n' "$KEPT" | grep -v '^[[:space:]]*$' | grep -vxF -- "$HEADLINE" | tail -n 6)"
if [ -n "$CONTEXT" ]; then
    printf '\nearlier:\n%s\n' "$CONTEXT"
fi
