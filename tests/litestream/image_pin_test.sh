#!/usr/bin/env bash
# Unit test for the Litestream image pin and the log level it depends on (pd-pfxf).
#
# THE BUG. Eight places named `docker.io/litestream/litestream:latest`. On
# 2026-08-31 that tag moved from 0.5.16 to 0.5.17, this box pulled it, and the
# next morning three container gates failed together — a txid advance that never
# happened, a drill that checked 0 of 4 tenants, and a correspondence wait that
# timed out. Nothing in the tree had changed, so it read as networking
# flakiness. What actually moved was one line of upstream's logging:
#
#   fix(logging): downgrade replica sync messages from INFO to DEBUG
#
# deploy/backup-check.sh reads a tenant's two TXIDs out of exactly that message,
# because the 0.5 CLI cannot resolve a `dir:` entry (see sidecar_position's note,
# and deploy/litestream.yml's `logging:` block for the three 0.5.17 alternatives
# that were measured and rejected). At the default level the message no longer
# exists, so backup-check judges no tenant at all inside its 1800s grace and
# pages for every tenant forever after it. Prod's backup verification would have
# stopped verifying, and the only warning was a red test tier.
#
# So the fix has TWO halves, and this holds both:
#
#   §1 nothing under deploy/ or tests/ rides `:latest`.
#   §2 every literal version in the tree agrees with the ONE in
#      deploy/litestream-lib.sh. A Quadlet unit cannot source a shell library,
#      so that copy is unavoidable — this is what makes it a second's failure
#      instead of an instance whose sidecar and whose freshness check disagree.
#   §3 the shipped config asks for a level at which the message is emitted, and
#      §4 backup-check is still the thing that reads it. Either half alone is
#      useless: pinned at a version that does not emit it is silence, and asking
#      for debug while `:latest` roams is the same bug waiting for a retag.
#
# Deliberately hermetic — no podman, no network, no pull — so it runs in the
# sub-second lint tier, long before the twenty-minute container gates it
# protects. tests/litestream/run.sh §3b is the runtime half: it asserts the
# RUNNING sidecar really does emit the line, which no grep of the tree can.
#
#   bash tests/litestream/image_pin_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

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
# "Something is unpinned" is useless without saying which line.
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
log() { printf '\n=== %s ===\n' "$*"; }

# WHAT IS IN SCOPE. Everything a deployment or a gate actually runs, stated as
# two directories rather than as the eight files that were wrong on the day —
# the next reference somebody adds is the one that would leak. deep-dives/ is
# excluded on purpose: it is a frozen research record that says which version it
# measured (v0.5.11), and rewriting it would falsify the record rather than fix
# anything. tests/alarming/fixtures/ likewise — those are captured journals of
# what a real box once printed, not instructions to run an image.
#
# And this file itself, which has to quote the bad string in order to describe
# the bug. Excluded BY PATH, one file, rather than by teaching the pattern to
# skip comments — a rule that ignores comments would also ignore a commented-out
# `Image=` line somebody meant to restore.
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
scoped_files() {
	find "${REPO_DIR}/deploy" "${REPO_DIR}/tests" \
		-path '*/tests/alarming/fixtures' -prune -o \
		-path "$SELF" -prune -o \
		-type f -print | sort
}

LIB="${REPO_DIR}/deploy/litestream-lib.sh"
YML="${REPO_DIR}/deploy/litestream.yml"
UNIT="${REPO_DIR}/deploy/pkdump-litestream.container"
CHECKER="${REPO_DIR}/deploy/backup-check.sh"

log "0. the files this is about are all there"
for f in "$LIB" "$YML" "$UNIT" "$CHECKER"; do
	check "${f#"${REPO_DIR}/"} exists" "yes" "$([[ -f "$f" ]] && echo yes || echo no)"
done

log "1. nothing a deployment or a gate runs rides a moving tag"
MOVING="$(scoped_files | xargs grep -nE 'litestream/litestream:(latest|[0-9]+\.[0-9]+)([^.0-9]|$)' /dev/null 2>/dev/null)"
none "no litestream/litestream:latest (or a truncated version) under deploy/ or tests/" "$MOVING"

log "2. one version, and every copy of it agrees"
# The single source of truth, read the way its callers read it.
# shellcheck source=deploy/litestream-lib.sh
. "$LIB"
check "deploy/litestream-lib.sh declares a version" "yes" \
	"$([[ "${PKDUMP_LITESTREAM_VERSION:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] && echo yes || echo no)"
check "…and builds the image reference from it" \
	"docker.io/litestream/litestream:${PKDUMP_LITESTREAM_VERSION:-<unset>}" \
	"${PKDUMP_LITESTREAM_IMAGE:-<unset>}"

# Every literal reference in scope, wherever it is. Asserted over the TREE and
# not over a list of files, for the same reason §1 is: a copy nobody remembered
# is exactly the failure being prevented.
LITERALS="$(scoped_files | xargs grep -hoE 'docker\.io/litestream/litestream:[0-9][^"'"'"' ]*' /dev/null 2>/dev/null | sort -u)"
check "at least one literal reference exists to check" "yes" \
	"$([[ -n "$LITERALS" ]] && echo yes || echo no)"
DISAGREE="$(grep -vxF "docker.io/litestream/litestream:${PKDUMP_LITESTREAM_VERSION:-}" <<<"$LITERALS")"
none "every literal image reference names the pinned version" "$DISAGREE"

# The Quadlet unit is the copy that cannot be avoided — systemd does not source
# shell — so it is named explicitly as well as covered by the sweep above. If
# this ever becomes substitutable, delete this check, not the one above it.
check "the shipped sidecar unit names the pinned image" "1" \
	"$(grep -cxF "Image=${PKDUMP_LITESTREAM_IMAGE}" "$UNIT")"

log "3. the shipped config asks for a level that emits the message"
# `logging.level` must be debug or trace: measured 2026-09-02, 0.5.17 emits
# msg="replica sync" at DEBUG and nothing at INFO. 0.5.16 emits it at both, so
# asking for it costs nothing on the older image and is what makes the newer one
# work at all.
LEVEL="$(sed -n '/^logging:/,/^[^ #]/p' "$YML" | sed -n 's/^[[:space:]]\+level:[[:space:]]*\([a-z]\+\).*/\1/p' | head -1)"
check "deploy/litestream.yml sets logging.level" "yes" \
	"$([[ -n "$LEVEL" ]] && echo yes || echo no)"
check "…to a level at which msg=\"replica sync\" is emitted" "yes" \
	"$(case "$LEVEL" in debug | trace) echo yes ;; *) echo no ;; esac)"

log "4. …and backup-check is still the thing that reads it"
# The other end of the contract. A checker that stopped parsing this message
# would make §3 a rule protecting nothing, and the log volume debug costs would
# be paid for no reason.
check "deploy/backup-check.sh parses msg=\"replica sync\"" "yes" \
	"$(grep -qF 'msg=\"replica sync\"' "$CHECKER" && echo yes || echo no)"
check "…for both txid.replica and txid.db" "yes" \
	"$(grep -q 'txid\\.replica=' "$CHECKER" && grep -q 'txid\\.db=' "$CHECKER" && echo yes || echo no)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — litestream is pinned to ${PKDUMP_LITESTREAM_VERSION} everywhere, and the"
echo "         shipped config asks for the log level backup-check reads."
