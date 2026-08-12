#!/usr/bin/env bash
# Parallel CI gate runner (pd-2nl9, item 4 of pd-6onp).
#
# deploy/ci-parallel.sh runs eleven container gates three at a time. Making a
# test suite faster is only ever worth doing if it stays exactly as capable of
# going red, so the properties this file asserts are the ones whose failure
# would be invisible in a green run:
#
#   A FAILURE IS NEVER MASKED — §2. Sequentially, the first failing gate ended
#   the run. In parallel a failing gate finishes beside passing ones, and the
#   danger is that its status is lost in the shuffle: the run must go red, the
#   summary must name it, and every other gate must still have run.
#
#   OUTPUT SURVIVES CONCURRENCY — §3. Three gates writing to one terminal
#   shred each other's output, and a shredded log is a gate nobody can
#   diagnose. Each gate's output has to come out whole and contiguous.
#
#   THE CAP IS REAL — §1. Not "we pass a number" but: six gates, three at a
#   time, never a fourth, and the three genuinely overlap.
#
#   THE DISK FLOOR TRIPS, AND IT HOLDS BEFORE IT ABORTS — §4/§5. This is a 15G
#   box that also runs prod, and the whole reason the floor moved inside the
#   dispatch loop is that three concurrent gates can eat what one could not.
#   §4 puts the REAL deploy/diskcheck.sh against a floor no disk can meet and
#   asserts the run refuses and says which gates never started. §5 proves the
#   other branch: below the floor with a gate still running, the runner WAITS
#   for that gate to give its space back rather than aborting a suite over a
#   moment of pressure.
#
#   ci.sh STILL RUNS EVERY GATE — §6. The refactor's own failure mode: a gate
#   that got queued nowhere runs never, and nothing else would notice. Every
#   gate script is located in a pkdump_par_add line, inside a real tier's
#   guard, and the queue is run exactly once.
#
# Hermetic: no podman, no network, no compilation. A couple of seconds, so it
# runs in the lint tier beside the other harness self-tests.
#
#   bash tests/ci/parallel_test.sh
set -uo pipefail # NOT -e: a failed assertion must be reported, not fatal

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
LIB="${REPO_DIR}/deploy/ci-parallel.sh"
CI_SH="${REPO_DIR}/deploy/ci.sh"
SELECT="${REPO_DIR}/deploy/ci-select.sh"

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

WORK="$(mktemp -d /tmp/pd-partest.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# A driver script, so every case runs the real library in its own shell with
# `set -e` on — the way deploy/ci.sh sources it — rather than in this one.
drive() { # drive <body> ; prints the run's output, sets DRIVE_RC
	local body="$1"
	cat >"${WORK}/drive.sh" <<EOF
set -euo pipefail
. "${LIB}"
pkdump_par_reset
${body}
rc=0
pkdump_par_run || rc=\$?
echo "DRIVE_RC=\${rc}"
echo "DRIVE_FAILED=\${PKDUMP_PAR_FAILED:-}"
EOF
	bash "${WORK}/drive.sh" 2>&1
}
rc_of() { printf '%s\n' "$1" | sed -n 's/^DRIVE_RC=//p'; }
failed_of() { printf '%s\n' "$1" | sed -n 's/^DRIVE_FAILED=//p'; }

# The real floor gate reads the host's alerts.env, which on a configured box
# would override the threshold these tests set. Point it at an empty file.
: >"${WORK}/alerts.env"
export PKDUMP_ALERTS_ENV="${WORK}/alerts.env"

# ---------------------------------------------------------------------------
log "1. The cap is real: six gates, three at a time, and they overlap"

# Every gate stamps the interval it was alive for, so the concurrency is read
# off the gates themselves rather than off the runner's own bookkeeping. The
# maximum overlap across those intervals is then the answer to both halves of
# the question — never above the cap, and actually reaching it.
#
# Milliseconds, not nanoseconds: `sort -n` and awk carry ~19 significant
# digits, which is exactly where a nanosecond epoch sits, and an overlap count
# computed from silently-rounded keys would be a test that lies.
STAMPS="${WORK}/stamps"
# A gate: stamp, wait until three gates have started, hold for a beat, stamp.
# The hold is what makes the overlap measurable — without it a gate can be gone
# before the next is dispatched, and six uncapped gates would look like three.
# The barrier is what makes it deterministic rather than a race with `sleep`.
cat >"${WORK}/gate.sh" <<EOF
L="\$1"
date +%s%3N > "${STAMPS}/\$L.start"
n=0
for _ in \$(seq 1 60); do
	n=\$(find "${STAMPS}" -name "*.start" | wc -l)
	[ "\$n" -ge 3 ] && break
	sleep 0.05
done
[ "\$n" -ge 3 ] || { echo "barrier never reached (\$n started)"; exit 1; }
sleep 0.4
date +%s%3N > "${STAMPS}/\$L.end"
EOF

# Max overlap across every recorded interval.
overlap() {
	local f l
	for f in "$STAMPS"/*.start; do
		l="$(basename "$f" .start)"
		[ -f "${STAMPS}/${l}.end" ] || continue
		echo "$(cat "$f") 1"
		echo "$(cat "${STAMPS}/${l}.end") -1"
		# -1 before +1 at the same millisecond: an interval that ENDS as
		# another begins is not an overlap, and counting it as one would
		# manufacture concurrency the runner never allowed.
	done | sort -n -k1 -k2 | awk '{n+=$2; if (n>m) m=n} END {print m+0}'
}

rm -rf "$STAMPS"
mkdir -p "$STAMPS"
body=""
for g in g1 g2 g3 g4 g5 g6; do
	body+="pkdump_par_add ${g} bash ${WORK}/gate.sh ${g}"$'\n'
done
out="$(PKDUMP_CI_JOBS=3 drive "$body")"
check "six gates at cap 3 all pass" "0" "$(rc_of "$out")"
check "all six gates ran" "6" "$(find "$STAMPS" -name '*.end' | wc -l)"
check "three ran at once, and never a fourth" "3" "$(overlap)"

# The cap is honoured downwards too — 1 is a serial run, which is the first
# thing to reach for when a parallel run misbehaves. Plain gates here: at a cap
# of 1 the barrier above could never be met, and a deadlock is not what is
# being asserted.
rm -rf "$STAMPS"
mkdir -p "$STAMPS"
body=""
for g in s1 s2 s3; do
	body+="pkdump_par_add ${g} bash -c 'date +%s%3N > ${STAMPS}/${g}.start; sleep 0.2; date +%s%3N > ${STAMPS}/${g}.end'"$'\n'
done
out="$(PKDUMP_CI_JOBS=1 drive "$body")"
check "at cap 1 every gate still runs" "0" "$(rc_of "$out")"
check "at cap 1 nothing overlaps — it is a serial run" "1" "$(overlap)"

# ---------------------------------------------------------------------------
log "2. A failing gate is never masked by the ones beside it"

RAN="${WORK}/ran"
mkdir -p "$RAN"
body=""
for g in ok-one ok-two ok-three; do
	body+="pkdump_par_add ${g} bash -c 'touch ${RAN}/${g}; exit 0'"$'\n'
done
body+="pkdump_par_add bad-one bash -c 'touch ${RAN}/bad-one; echo the-real-reason; exit 7'"$'\n'
for g in ok-four ok-five; do
	body+="pkdump_par_add ${g} bash -c 'touch ${RAN}/${g}; exit 0'"$'\n'
done
out="$(PKDUMP_CI_JOBS=3 drive "$body")"

check "one failure among five passes makes the run red" "1" "$(rc_of "$out")"
check "the failure is named" "bad-one" "$(failed_of "$out")"
check "the summary marks it FAIL" "1" \
	"$(printf '%s\n' "$out" | grep -cE '^ +FAIL +bad-one')"
check "and the log says FAILED with the name" "1" \
	"$(printf '%s\n' "$out" | grep -c 'FAILED: bad-one')"
# The other half: a failure must not cancel its neighbours either.
check "every other gate still ran" "6" "$(find "$RAN" -type f | wc -l)"
check "the passing gates are still marked ok" "5" \
	"$(printf '%s\n' "$out" | grep -cE '^ +ok +ok-')"
# The gate's own words survive, not just its status.
check "the failing gate's output is printed" "1" \
	"$(printf '%s\n' "$out" | grep -c 'the-real-reason')"
check "its exit status is reported, not flattened to 1" "1" \
	"$(printf '%s\n' "$out" | grep -c 'bad-one: FAIL (.*exit 7)')"

# TWO failures: neither may hide the other. Sequentially only the first was
# ever reported; that is the behaviour change worth pinning down.
body="pkdump_par_add bad-a bash -c 'exit 1'"$'\n'"pkdump_par_add bad-b bash -c 'exit 1'"$'\n'
body+="pkdump_par_add fine bash -c 'exit 0'"$'\n'
out="$(PKDUMP_CI_JOBS=3 drive "$body")"
check "two failures are both reported" "bad-a bad-b" \
	"$(failed_of "$out" | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ $//')"

# ---------------------------------------------------------------------------
log "3. Output comes out whole and contiguous, never interleaved"

# Each gate prints a numbered block with a per-gate marker, sleeping between
# lines so that unbuffered concurrent writes WOULD interleave.
body=""
for g in a b c; do
	body+="pkdump_par_add gate-${g} bash -c 'for i in 1 2 3 4 5; do echo \"${g}-line-\$i\"; sleep 0.05; done'"$'\n'
done
out="$(PKDUMP_CI_JOBS=3 drive "$body")"
check "all three gates pass" "0" "$(rc_of "$out")"
for g in a b c; do
	# Every line present...
	check "gate-${g}: all five lines present" "5" \
		"$(printf '%s\n' "$out" | grep -c "^${g}-line-")"
	# ...and contiguous: the five lines occupy five consecutive positions.
	span="$(printf '%s\n' "$out" | grep -n "^${g}-line-" | cut -d: -f1 |
		awk 'NR==1{f=$1} {l=$1} END {print l-f+1}')"
	check "gate-${g}: its lines are contiguous, nothing wedged between" "5" "$span"
	# ...and in order.
	check "gate-${g}: in order" "1" \
		"$(printf '%s\n' "$out" | grep "^${g}-line-" | sort -c 2>/dev/null && echo 1 || echo 0)"
done

# ---------------------------------------------------------------------------
log "4. The disk floor TRIPS — the real guard, against an impossible floor"

# deploy/diskcheck.sh --floor, unmodified, with a threshold no filesystem can
# meet. Nothing is mocked here: this is the guard that runs in CI, refusing.
RAN2="${WORK}/ran2"
mkdir -p "$RAN2"
body=""
for g in one two three; do
	body+="pkdump_par_add ${g} bash -c 'touch ${RAN2}/${g}'"$'\n'
done
out="$(PKDUMP_DISK_FLOOR_GB=999999999 PKDUMP_CI_JOBS=3 drive "$body")"
check "below the floor with nothing running, the run is red" "1" "$(rc_of "$out")"
check "and NOTHING was dispatched — the disk is not filled to find out" "0" \
	"$(find "$RAN2" -type f | wc -l)"
check "the refusal names the floor" "1" \
	"$(printf '%s\n' "$out" | grep -c 'below the disk floor')"
check "the refusal lists every gate that never started" "3" \
	"$(printf '%s\n' "$out" | sed -n '/never started/,$p' | grep -cE '^ +(one|two|three)$')"
# The floor is the one in deploy/diskcheck.sh, so its own diagnosis comes too.
check "the guard's own message is shown, not swallowed" "1" \
	"$(printf '%s\n' "$out" | grep -c 'floor 999999999G')"

# ---------------------------------------------------------------------------
log "5. Below the floor with a gate running, it HOLDS instead of aborting"

# A real disk cannot be made to cross a threshold on cue, and the branch worth
# proving is the one that waits. So the gate itself is stubbed — the SAME
# interface deploy/diskcheck.sh presents — and it answers "no room" from its
# second call until a running gate has finished and released.
cat >"${WORK}/floor-stub.sh" <<EOF
#!/usr/bin/env bash
# Stands in for deploy/diskcheck.sh --floor. Says yes once (so the first gate
# gets dispatched), then no until \$RELEASE exists — which the first gate
# creates as it finishes, standing in for a teardown returning its space.
N="\$(cat "${WORK}/floor.n" 2>/dev/null || echo 0)"
N=\$((N + 1)); echo "\$N" > "${WORK}/floor.n"
[ "\$N" -le 1 ] && exit 0
[ -e "${WORK}/released" ] && exit 0
echo "stub: below the floor" >&2
exit 1
EOF
: >"${WORK}/floor.n"
rm -f "${WORK}/released"
RAN3="${WORK}/ran3"
mkdir -p "$RAN3"

body="pkdump_par_add first bash -c 'touch ${RAN3}/first; sleep 0.3; touch ${WORK}/released'"$'\n'
body+="pkdump_par_add second bash -c 'touch ${RAN3}/second'"$'\n'
body+="pkdump_par_add third bash -c 'touch ${RAN3}/third'"$'\n'
out="$(PKDUMP_PAR_DISKCHECK="${WORK}/floor-stub.sh" PKDUMP_CI_JOBS=3 drive "$body")"

check "the run finishes green — pressure is not a failure" "0" "$(rc_of "$out")"
check "every gate ran, none abandoned" "3" "$(find "$RAN3" -type f | wc -l)"
check "the hold is announced, naming the gate held" "1" \
	"$(printf '%s\n' "$out" | grep -c '\[hold\]  second')"
check "and counted in the summary" "1" \
	"$(printf '%s\n' "$out" | grep -c 'held by the disk floor')"

# ---------------------------------------------------------------------------
log "6. Degenerate inputs and the cap's own bounds"

out="$(drive "")"
check "an empty queue is a no-op, not a failure" "0" "$(rc_of "$out")"
check "and it says so" "1" "$(printf '%s\n' "$out" | grep -c 'no gates queued')"

# A label becomes a filename and a column in the summary; anything else is a
# caller bug, refused at the point of the mistake.
bash -c ". '${LIB}'; pkdump_par_reset; pkdump_par_add 'Bad Label' true" >/dev/null 2>&1
check "a label that is not [a-z0-9-] is refused" "2" "$?"
bash -c ". '${LIB}'; pkdump_par_reset; pkdump_par_add ok-label" >/dev/null 2>&1
check "a gate with no command is refused" "2" "$?"

body="pkdump_par_add solo bash -c 'exit 0'"$'\n'
check "a non-numeric cap is refused" "1" "$(rc_of "$(PKDUMP_CI_JOBS=lots drive "$body")")"
check "a cap of zero is refused" "1" "$(rc_of "$(PKDUMP_CI_JOBS=0 drive "$body")")"

out="$(PKDUMP_CI_JOBS=99 drive "$body")"
check "a cap above the ceiling still runs" "0" "$(rc_of "$out")"
check "...but is clamped, out loud" "1" \
	"$(printf '%s\n' "$out" | grep -c 'above the 4 this box is sized for')"

# ---------------------------------------------------------------------------
log "7. deploy/ci.sh queues every gate, under a real tier, and runs the queue once"

# THE refactor's failure mode. A gate that stopped being invoked would leave no
# trace in a green run, so each one is located by its own path.
GATES="tests/litestream/run.sh tests/litestream/drill.sh tests/alarming/run.sh
tests/litestream/recreate.sh tests/tenants/upgrade.sh tests/tenants/handles.sh
tests/schema-version/run.sh tests/lake/run.sh tests/lake/prices.sh
tests/lake/value_snapshots.sh tests/refresh/tenant_bytes.sh"

for g in $GATES; do
	check "${g} is queued exactly once" "1" \
		"$(grep -c "pkdump_par_add [a-z-]* bash \"\$REPO_DIR/${g}\"" "$CI_SH")"
	# And not ALSO run in place — queued and run would be the gate twice, which
	# on these gates means two instances fighting over one set of names.
	check "${g} is not also invoked directly" "0" \
		"$(grep -cE "^ *bash \"\\\$REPO_DIR/${g}\"" "$CI_SH")"
done

# Every queued gate sits inside a tier guard, and the tier is a real one — a
# gate queued outside every guard would run even for a docs-only PR, which is
# the cost pd-s2mj removed.
CANON="$(bash "$SELECT" --all-tiers | tr '\n' ' ')"
QUEUED_TIERS="$(awk '
	/^ *if tier -?q? ?[a-z]+;/ { t = $3; sub(/^-q$/, "", t); if (t == "") t = $4; sub(/;$/, "", t) }
	/pkdump_par_add / && !/^ *#/ { print (t == "" ? "NONE" : t) }
' "$CI_SH" | sort -u)"
for t in $QUEUED_TIERS; do
	check "gates are queued under the real tier '${t}'" "yes" \
		"$([[ " ${CANON} " == *" ${t} "* ]] && echo yes || echo no)"
done
check "every queued gate is inside a tier guard" "0" \
	"$(printf '%s\n' "$QUEUED_TIERS" | grep -cx NONE)"

# One queue, run once. Twice would run every gate twice; never, not at all.
check "the queue is run exactly once" "1" \
	"$(grep -cE '^ *pkdump_par_run' "$CI_SH")"
check "ci.sh sources the runner" "1" \
	"$(grep -c '\. "\$SCRIPT_DIR/ci-parallel.sh"' "$CI_SH")"
check "the floor is measured on both disks ci.sh cares about" "1" \
	"$(grep -c 'PKDUMP_PAR_DISK_PATHS=("\$HOME" "\${PKDUMP_STORE_ROOT:-\$HOME}")' "$CI_SH")"
# A cancelled run must not leave gates behind holding containers.
check "the EXIT trap takes the wave down with it" "1" \
	"$(grep -c 'pkdump_par_kill_all' "$CI_SH")"

# What makes running these at the same time safe at all: not one of them uses a
# fixed global name. Every one derives its containers, volumes and image tags
# from this checkout's path — the property concurrent polecats already relied
# on, and the one a new gate is most likely to forget.
for g in $GATES; do
	check "${g} names its resources per-checkout" "yes" \
		"$(grep -qE 'sha1sum' "${REPO_DIR}/${g}" && echo yes || echo no)"
done

# ---------------------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
