#!/usr/bin/env bash
# Unit test for tests/lib/ports.sh (pd-r0ri).
#
# Two halves, and the second is the point of the bead.
#
#   §1-§5 the library does what it claims — the port it hands back is real,
#         free, outside the range the kernel hands out unasked, and never one
#         something is already listening on.
#   §6-§8 THE RATCHET. A picked host port has now been found and fixed in five
#         files (deploy/ci.sh, tests/tenants/handles.sh, and all three of
#         tests/litestream/). Each fix was correct and each time the next file
#         still had it. So the tree itself is asserted on: no harness under
#         tests/ or deploy/ may derive a host port from a hash, a band or a
#         literal, and free_port may exist in exactly one place. A sixth
#         relapse fails HERE, in under a second, instead of in a container gate
#         forty minutes into CI as "address already in use".
#
# Deliberately hermetic — no podman, no network — so deploy/ci.sh can run it
# early beside tests/lib/diagnostics_test.sh.
#
#   bash tests/lib/ports_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=tests/lib/ports.sh
. "${SCRIPT_DIR}/ports.sh"

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
# An assertion that prints the offending lines, because "something in the tree
# picks a port" is useless without saying which line does.
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

# The shell harnesses, minus this file — its detector patterns are written to
# look like the thing they detect.
harnesses() {
	find "${REPO_DIR}/tests" "${REPO_DIR}/deploy" -name '*.sh' -type f \
		! -path "${REPO_DIR}/tests/lib/ports_test.sh" | sort
}

log "1. free_port answers with one port"
PORT_A="$(free_port)"
check "a single line" "1" "$(printf '%s\n' "$PORT_A" | grep -c .)"
check "which is a number" "yes" "$([[ $PORT_A =~ ^[0-9]+$ ]] && echo yes || echo no)"
check "in the unprivileged range" "yes" \
	"$([[ $PORT_A -ge 1024 && $PORT_A -le 65535 ]] && echo yes || echo no)"

log "2. the port it names is actually free"
# The whole contract. Bind it the way a caller would.
BOUND=$(python3 -c '
import socket, sys
s = socket.socket()
try:
    s.bind(("", int(sys.argv[1])))
except OSError as e:
    print("no: %s" % e)
else:
    print("yes")
finally:
    s.close()
' "$PORT_A")
check "it binds" "yes" "$BOUND"

log "3. it is outside the range the kernel hands out unasked"
# The reason this is not `bind(("", 0))`. A gate holds its port for minutes;
# inside the ephemeral range an unrelated outbound socket can be handed the
# same number in the meantime and the later listener bind fails EADDRINUSE.
read -r EPH_LO EPH_HI < <(cat /proc/sys/net/ipv4/ip_local_port_range 2>/dev/null || echo "32768 60999")
if [[ $((65535 - EPH_HI)) -ge 64 ]]; then
	check "above the ephemeral range (${EPH_LO}-${EPH_HI})" "yes" \
		"$([[ $PORT_A -gt $EPH_HI ]] && echo yes || echo no)"
else
	echo "  SKIP  this box hands out everything up to 65535; no band above it"
fi

log "4. it does not hand back a port something is already listening on"
# A listener is held open for the duration, and free_port is asked repeatedly.
# The bind inside free_port is what has to notice; nothing else would.
LISTENER_OUT="$(mktemp "${TMPDIR:-/tmp}/pd-portstest.XXXXXX")"
trap 'rm -f "$LISTENER_OUT"' EXIT
python3 -c '
import socket, sys, time
s = socket.socket()
s.bind(("", 0))
s.listen(1)
print(s.getsockname()[1], flush=True)
time.sleep(30)
' >"$LISTENER_OUT" &
LISTENER_PID=$!
trap 'kill "$LISTENER_PID" 2>/dev/null; rm -f "$LISTENER_OUT"' EXIT
for _ in $(seq 50); do
	[[ -s "$LISTENER_OUT" ]] && break
	sleep 0.1
done
HELD="$(cat "$LISTENER_OUT")"
check "the fixture is listening on a port" "yes" \
	"$([[ $HELD =~ ^[0-9]+$ ]] && echo yes || echo no)"
COLLISIONS=0
for _ in $(seq 30); do
	[[ "$(free_port)" == "$HELD" ]] && COLLISIONS=$((COLLISIONS + 1))
done
check "30 calls never returned it" "0" "$COLLISIONS"
kill "$LISTENER_PID" 2>/dev/null
wait "$LISTENER_PID" 2>/dev/null
trap 'rm -f "$LISTENER_OUT"' EXIT

log "5. successive calls do not repeat"
# Not a guarantee of the contract — a random draw may legitimately repeat — but
# a free_port that returned a constant would pass §1-§4 and still be the bug.
DISTINCT=$(
	for _ in $(seq 20); do free_port; done | sort -u | grep -c .
)
check "20 calls gave more than one number" "yes" \
	"$([[ $DISTINCT -gt 1 ]] && echo yes || echo no)"

log "6. no harness derives a host port from a hash or a band (pd-r0ri)"
# `MINIO_PORT=${MINIO_PORT:-$(( 40000 + 16#${SUFFIX:0:3} % 400 ))}` is the exact
# shape that failed. Any `*PORT=${*:-<default>}` whose default is neither
# free_port nor empty (empty means podman picks and the caller reads it back)
# is a picked port.
DERIVED="$(
	harnesses | xargs grep -nE '[A-Za-z_]*PORT=\$\{[A-Za-z_]+:-[^}]*\}' /dev/null |
		grep -vE ':-\$\(free_port\)\}' | grep -vE ':-\}'
)"
none "every PORT default is free_port or podman's to pick" "$DERIVED"

log "7. and none is a literal or an arithmetic expression"
LITERAL="$(harnesses | xargs grep -nE '[A-Za-z_]*PORT=("|'"'"')?([0-9]{2,}|\$\(\()' /dev/null)"
none "no PORT= assigned a number or a computation" "$LITERAL"

# The other half of the same mistake: the port never named in a variable at all.
# Matches a published `HOST:CONTAINER` whose host side is a literal; the correct
# forms (`-p "127.0.0.1:$VAR:9000"`, `-p "127.0.0.1::8080"`) have no digits
# immediately before the container port.
PUBLISHED="$(harnesses | xargs grep -nE -- '(-p|--publish)[= ]+"?[0-9.]*[0-9]:[0-9]+' /dev/null)"
none "no container publishes a literal host port" "$PUBLISHED"

log "8. free_port is defined exactly once (pd-r0ri)"
# The bead behind this file: the same eight lines had been copied into two
# harnesses and were about to go into two more. A copy is how the fix stops
# reaching the next file.
DEFS="$(harnesses | xargs grep -ln '^free_port() {' /dev/null)"
check "one definition" "${REPO_DIR}/tests/lib/ports.sh" "$DEFS"
# And every harness that calls it sources the library rather than trusting that
# something earlier in the process did.
CALLERS="$(harnesses | xargs grep -l 'free_port' /dev/null | grep -v '/tests/lib/ports.sh$')"
UNSOURCED=""
while IFS= read -r f; do
	[[ -z "$f" ]] && continue
	grep -q 'tests/lib/ports.sh"' "$f" || UNSOURCED+="${f}"$'\n'
done <<<"$CALLERS"
none "every caller sources tests/lib/ports.sh" "${UNSOURCED%$'\n'}"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — host ports come from the kernel, in one place, in every harness."
