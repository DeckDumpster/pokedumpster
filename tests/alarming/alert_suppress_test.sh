#!/usr/bin/env bash
# Unit test for alert.sh's repeat suppression (pd-hqdt).
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
# A pager a human has learned to ignore. pkdump-value-snapshots@prod failed the
# same way four nights running and pushed four byte-identical pages:
#
#   PokeDumpster value snapshots PARTIAL (prod)
#   Skipped: collection. The run completed for everyone else; see journalctl ...
#
# Every one was correct. Not one was actionable. What they bought was a channel
# that gets swiped away without reading, and the next thing it carried — the
# sidecar's rootless-netns failure, a real outage — reached nobody.
#
# tests/alarming/run.sh proves every layer FIRES, which is why it cannot catch
# this: firing four times is exactly what it asserts. So this gate asserts the
# opposite half — what does NOT get sent — and, harder, everything that must
# still get sent anyway. A suppressor that is wrong in the silent direction is
# indistinguishable from a backup that quietly stopped running, so the sections
# that matter most here are §2, §3 and §5.
#
# Hermetic and sub-second: no podman, no systemd, no network beyond a local
# recorder on 127.0.0.1, and a state dir under mktemp — the real one in
# ~/.local/state is never read or written. deploy/ci.sh runs it in the lint
# tier beside tests/alarming/journal_summary_test.sh.
#
#   bash tests/alarming/alert_suppress_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

WORK="$(mktemp -d /tmp/pkdump-alertsup.XXXXXX)"
STATE="${WORK}/state"
SINK_PID=""
# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	local rc=$?
	[ -n "$SINK_PID" ] && kill "$SINK_PID" >/dev/null 2>&1
	rm -rf "$WORK"
	return $rc
}
trap cleanup EXIT

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

# ── the harness ─────────────────────────────────────────────────────────────
SINK_PORT=${SINK_PORT:-$(free_port)}
SINK_LOG="${WORK}/sink.jsonl"
: >"$SINK_LOG"
python3 "${SCRIPT_DIR}/sink.py" "$SINK_PORT" "$SINK_LOG" >"${WORK}/sink.err" 2>&1 &
SINK_PID=$!
for _ in $(seq 40); do
	curl -fsS -m 2 "http://127.0.0.1:${SINK_PORT}/ready" >/dev/null 2>&1 && break
	sleep 0.25
done
: >"$SINK_LOG"

# Every alert in this file goes through here: real alert.sh, real curl, real
# sink. The assertions are all "what did the sink receive".
send() { # send <title> <message>   -> alert.sh's own exit status
	PUSHOVER_TOKEN=t PUSHOVER_USER=u \
		PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}/pushover" \
		PKDUMP_ALERT_STATE_DIR="$STATE" \
		bash "${REPO_DIR}/deploy/alert.sh" "$1" "$2" >>"${WORK}/alert.log" 2>&1
}
pushes() { grep -c '"path": "/pushover"' "$SINK_LOG" 2>/dev/null || true; }
sent() { grep -c -- "$1" "$SINK_LOG" 2>/dev/null || true; }
reset() {
	: >"$SINK_LOG"
	rm -rf "$STATE"
}

# The four pages, verbatim.
VS_TITLE="PokeDumpster value snapshots PARTIAL (prod)"
VS_MSG="Skipped: collection. The run completed for everyone else; see journalctl --user -u pkdump-value-snapshots@prod.service"

# ── 1. the four nights ──────────────────────────────────────────────────────
log "1. four identical failures page ONCE, and the first page says so"
reset
send "$VS_TITLE" "$VS_MSG"
send "$VS_TITLE" "$VS_MSG"
send "$VS_TITLE" "$VS_MSG"
send "$VS_TITLE" "$VS_MSG"
check "four identical alerts sent exactly one page" "1" "$(pushes)"
check "and it was the FIRST one, not the last" "1" "$(sent 'value snapshots PARTIAL')"
# The one misreading worse than the noise: silence read as "it stopped".
check "the page tells the reader it will not repeat" "1" \
	"$(sent 'you will NOT be paged again')"
check "and names the window it is quoting" "1" "$(sent 'suppressed for 24h')"
# Suppression is a decision, not a delivery failure. The caller's own error
# handling (value-snapshots.sh prints "reached nobody" on non-zero) must not
# fire for a page that was deliberately withheld.
send "$VS_TITLE" "$VS_MSG"
check "a suppressed alert exits 0, so no caller reports it as undelivered" "0" "$?"
check "and it said why, in the journal" "yes" \
	"$(grep -q 'alert.sh: SUPPRESSED' "${WORK}/alert.log" && echo yes || echo no)"

# ── 2. a changed signature is never suppressed ──────────────────────────────
log "2. a CHANGED alert pages immediately, however recent its neighbour"
reset
send "$VS_TITLE" "$VS_MSG"
send "$VS_TITLE" "Skipped: alice bob. The run completed for everyone else; see journalctl --user -u pkdump-value-snapshots@prod.service"
check "a different set of skipped tenants is a different page" "2" "$(pushes)"
check "and the new one is the one that arrived second" "1" "$(sent 'alice bob')"

# A different unit reporting the same words must not be silenced by its
# neighbour: identity lives in the title, and the title is matched exactly.
reset
send "PokeDumpster unit FAILED: pkdump-derive@prod.service" "rootless netns: no such file or directory"
send "PokeDumpster unit FAILED: pkdump-value-snapshots@prod.service" "rootless netns: no such file or directory"
check "the same failure in two different units is two pages" "2" "$(pushes)"

# Severity lives in the title too, and an escalation must always ring.
reset
send "PokeDumpster LOW DISK (85%)" "/dev/sda1 85% on box — over 80% threshold"
send "PokeDumpster LOW DISK (99%)" "/dev/sda1 99% on box — over 80% threshold"
check "disk 85% -> 99% pages again: title digits are NOT normalised" "2" "$(pushes)"

# ── 3. the numbers that move on every occurrence of ONE failure ─────────────
log "3. the same failure whose message carries a moving number is still one page"
reset
send "PokeDumpster backup STALE (prod)" "Litestream S3 freshness check failed: the user registry: newest S3 replica write is 65h old (> 36h threshold)"
send "PokeDumpster backup STALE (prod)" "Litestream S3 freshness check failed: the user registry: newest S3 replica write is 89h old (> 36h threshold)"
check "a stale backup getting staler is not four days of pages" "1" "$(pushes)"
# The cost of that, stated: only WORDS distinguish two failures of one caller.
send "PokeDumpster backup STALE (prod)" "Litestream S3 freshness check failed: the collection: newest S3 replica write is 89h old (> 36h threshold)"
check "but a different tenant in the same shape still pages" "2" "$(pushes)"

# ── 4. the window ───────────────────────────────────────────────────────────
log "4. once the window is spent, the same failure pages again"
reset
send "$VS_TITLE" "$VS_MSG"
check "the first page went" "1" "$(pushes)"
# Age the stamp rather than sleeping: the window is 24h and the assertion is
# about arithmetic, not about waiting for a clock.
for f in "$STATE"/*; do printf '%s\n' "$(($(date +%s) - 90000))" >"$f"; done
send "$VS_TITLE" "$VS_MSG"
check "a stamp older than the window suppresses nothing" "2" "$(pushes)"

# ── 5. everything undecidable PAGES ─────────────────────────────────────────
log "5. fail toward paging — the rule alert-gate.sh is built on"

# A clock that jumped backwards leaves a stamp in the future. Age < 0 is not a
# fresh page; it is an unusable answer, so it sends.
reset
send "$VS_TITLE" "$VS_MSG"
for f in "$STATE"/*; do printf '%s\n' "$(($(date +%s) + 3600))" >"$f"; done
send "$VS_TITLE" "$VS_MSG"
check "a stamp from the future (clock skew) pages rather than silences" "2" "$(pushes)"

# A stamp that says nothing readable is the same class of answer.
reset
send "$VS_TITLE" "$VS_MSG"
for f in "$STATE"/*; do printf 'corrupt\n' >"$f"; done
send "$VS_TITLE" "$VS_MSG"
check "an unreadable stamp pages rather than silences" "2" "$(pushes)"

# No writable state dir: nothing can be remembered, so nothing is withheld.
# A regular file where the directory should be — ENOTDIR for root and everyone
# else alike, unlike a permissions trick.
: >"$SINK_LOG"
: >"${WORK}/not-a-dir"
PUSHOVER_TOKEN=t PUSHOVER_USER=u \
	PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}/pushover" \
	PKDUMP_ALERT_STATE_DIR="${WORK}/not-a-dir/alerts" \
	bash "${REPO_DIR}/deploy/alert.sh" "$VS_TITLE" "$VS_MSG" >/dev/null 2>&1
PUSHOVER_TOKEN=t PUSHOVER_USER=u \
	PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}/pushover" \
	PKDUMP_ALERT_STATE_DIR="${WORK}/not-a-dir/alerts" \
	bash "${REPO_DIR}/deploy/alert.sh" "$VS_TITLE" "$VS_MSG" >/dev/null 2>&1
check "an unwritable state dir suppresses nothing at all" "2" "$(pushes)"
check "and the page carries no promise it cannot keep" "0" \
	"$(sent 'you will NOT be paged again')"

# The explicit opt-out, which is what makes alarm-status.sh --verify honest.
reset
PUSHOVER_TOKEN=t PUSHOVER_USER=u \
	PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}/pushover" \
	PKDUMP_ALERT_STATE_DIR="$STATE" PKDUMP_ALERT_NO_SUPPRESS=1 \
	bash "${REPO_DIR}/deploy/alert.sh" "$VS_TITLE" "$VS_MSG" >/dev/null 2>&1
PUSHOVER_TOKEN=t PUSHOVER_USER=u \
	PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}/pushover" \
	PKDUMP_ALERT_STATE_DIR="$STATE" PKDUMP_ALERT_NO_SUPPRESS=1 \
	bash "${REPO_DIR}/deploy/alert.sh" "$VS_TITLE" "$VS_MSG" >/dev/null 2>&1
check "PKDUMP_ALERT_NO_SUPPRESS=1 sends every time" "2" "$(pushes)"

# An unconfigured channel is still a FAILURE, not a suppressed duplicate — the
# credential check runs first on purpose (pd-1717).
reset
PUSHOVER_TOKEN= PUSHOVER_USER= \
	PUSHOVER_API_URL="http://127.0.0.1:${SINK_PORT}/pushover" \
	PKDUMP_ALERT_STATE_DIR="$STATE" \
	bash "${REPO_DIR}/deploy/alert.sh" "$VS_TITLE" "$VS_MSG" >/dev/null 2>&1
check "no credentials still exits non-zero, duplicate or not" "1" "$?"
check "and nothing was sent" "0" "$(pushes)"

# ── 6. a dropped page must be free to try again ─────────────────────────────
log "6. the window opens on DELIVERY, not on the attempt"
reset
DEAD_PORT="$(free_port)"
PUSHOVER_TOKEN=t PUSHOVER_USER=u \
	PUSHOVER_API_URL="http://127.0.0.1:${DEAD_PORT}/pushover" \
	PKDUMP_ALERT_STATE_DIR="$STATE" \
	bash "${REPO_DIR}/deploy/alert.sh" "$VS_TITLE" "$VS_MSG" >/dev/null 2>&1
# curl's own status, propagated: 7 for a refused connection. The assertion is
# that it is not 0 — a suppressed page and a dropped page must never look alike.
check "a push that could not be delivered fails, as it always did" "no" \
	"$([ "$?" -eq 0 ] && echo yes || echo no)"
check "and left no stamp behind" "0" "$(find "$STATE" -type f 2>/dev/null | wc -l | tr -d ' ')"
send "$VS_TITLE" "$VS_MSG"
check "so the retry is not mistaken for a repeat" "1" "$(pushes)"

# ── 7. the notice survives a page long enough to be trimmed ─────────────────
log "7. the notice is paid for out of the 900-byte budget, not after it"
reset
LONG="CAUSE-IS-HERE $(head -c 2000 /dev/zero | tr '\0' 'x') BOILERPLATE-IS-HERE"
send "PokeDumpster unit FAILED: pkdump-litestream-prod.service" "$LONG"
check "the head of the message still leads (pd-pwk8 holds)" "1" "$(sent 'CAUSE-IS-HERE')"
check "the tail is still what got trimmed" "0" "$(sent 'BOILERPLATE-IS-HERE')"
check "and the notice is on the page rather than trimmed off it" "1" \
	"$(sent 'you will NOT be paged again')"
# Pushover rejects a body over 1024 characters outright, and a rejected page is
# a page nobody got. Measure what the sink actually received.
message_bytes() { # <marker|""> -> bytes of the message part the sink recorded
	python3 -c '
import json, sys
want = sys.argv[2]
for line in open(sys.argv[1], encoding="utf-8"):
    rec = json.loads(line)
    if rec["path"] != "/pushover":
        continue
    body = rec["body"]
    start = body.index("name=\"message\"") + len("name=\"message\"")
    chunk = body[start:].split("\r\n--", 1)[0].strip("\r\n")
    # The notice is preceded by the two blank-line bytes it opens with, so the
    # message the trim actually produced is two shorter than where it starts.
    marker = "\n\n(Repeats of this same alert"
    print(len(chunk.encode("utf-8")) if want == "all"
          else chunk.index(marker) if marker in chunk else -1)
    break
' "$SINK_LOG" "$1" 2>/dev/null
}
BODY_BYTES="$(message_bytes all)"
check "and the whole message stays inside Pushover's cap" "yes" \
	"$([ -n "$BODY_BYTES" ] && [ "$BODY_BYTES" -le 1024 ] && echo yes || echo no)"

# The trim cuts by BYTES, every headline this repo writes carries an em-dash,
# and half a character is a request Pushover rejects outright (pd-pwk8). That
# assertion lives next door — but its 895-byte pad was measured against a
# 900-byte budget, and the notice is now paid for out of the same budget. So
# measure the budget this page actually got, and cut on it.
BUDGET="$(message_bytes notice)"
check "the notice was appended, not squeezed in" "yes" \
	"$([ -n "$BUDGET" ] && [ "$BUDGET" -gt 0 ] && echo yes || echo no)"
reset
PAD="$(head -c "$((BUDGET - 5))" /dev/zero | tr '\0' 'y')"
send "PokeDumpster unit FAILED: pkdump-derive@prod.service" "${PAD}—tail"
# The sink decodes with errors="replace" and records with json.dumps, so a byte
# that is not valid UTF-8 lands in the log as the escape text 'ufffd' — exactly
# the payload Pushover would have rejected. Match on 'ufffd' alone: json.dumps
# writes ONE backslash, and a pattern spelling two ('\\\\ufffd') matches nothing at
# all — which is how the same assertion next door came to pass vacuously.
check "a cut that lands mid-character still drops the character, not half of it" "0" \
	"$(grep -cF 'ufffd' "$SINK_LOG" || true)"
check "and the notice survives the cut it was budgeted for" "1" \
	"$(sent 'you will NOT be paged again')"

# ── 8. THE RATCHET: shipped defaults ────────────────────────────────────────
log "8. armed by default, at 24h, and recorded only after delivery"
check "the default window is 24h and needs no configuration" "1" \
	"$(grep -c 'PKDUMP_ALERT_SUPPRESS_SECONDS:-86400' "${REPO_DIR}/deploy/alert.sh" || true)"
check "alarm-status.sh --verify opts out, so arming cannot self-silence" "1" \
	"$(grep -c 'PKDUMP_ALERT_NO_SUPPRESS=1 bash "${SCRIPT_DIR}/alert.sh"' "${REPO_DIR}/deploy/alarm-status.sh" || true)"
# Order is the invariant §6 rests on, and it is one line-swap away from being
# lost: a stamp written above the curl turns one dropped page into a whole
# window of silence.
CURL_LINE="$(grep -n '^curl -fsS' "${REPO_DIR}/deploy/alert.sh" | head -n1 | cut -d: -f1)"
STAMP_LINE="$(grep -n '^if \[ -n "\$STAMP" \]' "${REPO_DIR}/deploy/alert.sh" | head -n1 | cut -d: -f1)"
check "the stamp is written below the curl, never above it" "yes" \
	"$([ -n "$CURL_LINE" ] && [ -n "$STAMP_LINE" ] && [ "$STAMP_LINE" -gt "$CURL_LINE" ] && echo yes || echo no)"

# ── RESULT ──────────────────────────────────────────────────────────────────
log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
