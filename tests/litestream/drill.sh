#!/usr/bin/env bash
# Container-tier gate (pd-v8zf): the MULTI-TENANT DR DRILL — restore ONE tenant
# in place, through the shipped runbook, and prove the other tenants were not
# touched.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/litestream/drill.sh          # ~3min, throwaway MinIO, full teardown
#   KEEP=1 bash tests/litestream/drill.sh   # leave the drill instance up for poking
#   DRILL_REAL_S3=1 bash tests/litestream/drill.sh
#                                           # against the REAL bucket of instance
#                                           #   $DRILL_SOURCE_INSTANCE (default prod)
#                                           #   under a throwaway prefix. Needs creds
#                                           #   + network; NOT part of ci.sh.
#
# ── HOW THIS DIFFERS FROM tests/litestream/run.sh ───────────────────────────
# run.sh proves the shipped CONFIG replicates every tenant and the shipped
# ADDRESSING restores the right one, out of place, from a bare `litestream`
# invocation. This drills the OPERATOR PROCEDURE instead — deploy/RESTORE.md's
# three scenarios, executed as written:
#
#   A. a tenant's live database is deleted    -> restore-litestream.sh <inst> <db-id>
#   B. a tenant needs rolling back in time    -> restore-litestream.sh --at=<T> ...
#   C. the whole data volume is lost          -> restore the REGISTRY first, read
#                                                the tenant list out of it, then
#                                                restore each database it names
#
# ── THE RECOVERY MATRIX (pd-7f46) ───────────────────────────────────────────
# A restore that only ever recovers a live, first, never-renamed tenant is a
# restore tested in the one state a real box is least likely to be in. §7-§10
# walk the states THIS design creates, and every assertion is "can this
# person's collection be brought back", never "the sidecar is active":
#
#   * a NON-FIRST tenant — §4 onwards; the victim is #2 of 4, so an off-by-one
#     in the addressing fails here instead of passing on tenant #1.
#   * after a RENAME (§7) — the handle is a label and the database_id is the
#     identity, so the file, the prefix and the recovery window do not move.
#   * a DETACHED tenant (§8) — detach-not-delete is only justified if the kept
#     data comes back, and if the surviving row still says whose it was.
#   * a RECREATED handle — NOT here. tests/litestream/recreate.sh is the whole
#     proof of it (pd-pm7b) and deploy/ci.sh runs it; duplicating it here would
#     give two half-proofs instead of one.
#   * THE LOAD-BEARING NEGATIVE (§10) — restoring the tenant files WITHOUT the
#     registry must FAIL. It is the section that turns "restore the registry
#     first" from a documented wish into a tested invariant, and it shows the
#     failure first: every prefix the bucket offers, restored onto a
#     registry-less volume, comes back complete and healthy and anonymous, and
#     `pkdump tenant list` cannot name a single one of them.
#
# It runs the real scripts (deploy/restore-litestream.sh, deploy/backup-check.sh)
# against a real Quadlet sidecar unit on a real data volume, in place — deleting
# the live file and restoring over it, which is what an incident actually looks
# like. The property under test is ISOLATION: after each restore, every other
# tenant's database holds exactly the data it held, its replica is still current,
# and it still restores to its own data. NOT byte-identical — see the comment on
# `logical_dump` for why that is a claim SQLite never made (pd-zk0c).
#
# Scenario C also settles the question the epic keeps returning to: the rejected
# libSQL/sqld path had a DR gap where the namespace registry was not backed up,
# so recovery had to re-declare the namespaces before any data would come back.
# We have a registry now (pd-fci1) — a handle -> database_id table — so the only
# honest answer is to put it in the replicated set and RESTORE FROM IT. §7 wipes
# the volume, shows what the bucket alone can say (four anonymous ids, not one
# name), restores the registry FIRST, and recovers every tenant by handle from
# what it says. That ordering is the finding, not a detail: the reverse order
# hands you every byte and no way to attribute any of it.
#
# Prod-safe by default: its own instance name, its own volume, its own throwaway
# MinIO, its own podman secret, its own config dir. It touches no pkdump-prod
# unit, no pkdump-prod-data volume and no real bucket unless DRILL_REAL_S3=1 —
# and that mode writes only under its own hardcoded pd-v8zf-drill/ prefix, which
# it deletes on the way out.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
AWSCLI_IMAGE=${AWSCLI_IMAGE:-docker.io/amazon/aws-cli:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# The addressing under test is the deploy scripts' own.
# shellcheck source=deploy/litestream-lib.sh
. "${REPO_DIR}/deploy/litestream-lib.sh"
# The Litestream version too, from that same file (pd-pfxf). A gate that
# pulled `:latest` would test whatever the registry pushed last night and
# not what any box actually runs — which is exactly how a 0.5.16 -> 0.5.17
# retag turned three gates red on a change that touched none of them.
LITESTREAM_IMAGE="${LITESTREAM_IMAGE:-$PKDUMP_LITESTREAM_IMAGE}"
# ...and so is the container store. Already active when deploy/ci.sh ran this,
# in which case activation is a no-op; standalone it honours PKDUMP_STORE_ROOT
# the same way setup.sh does. Without it the drill's sidecar unit would look for
# its image in a store the drill's own podman calls never wrote to (pd-fite).
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
pkdump_store_activate

# Host ports, from the kernel rather than picked (pd-r0ri). One definition for
# every harness; see tests/lib/ports.sh for what a picked one costs.
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

# Waiting for CONDITIONS rather than for the clock (pd-86er). This drill stops
# and starts the sidecar seventeen times; at a fixed two seconds down and eight
# up that was ~104 seconds a run asserting nothing but how fast this box is.
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"

# "Does this replica hold data?" — the one definition. `ltx … | grep -q .` reads
# a prefix that has never been written to as FULL, because litestream prints its
# column header regardless; §2's `every tenant is replicating from the one
# sidecar` therefore passed the instant S3 answered, whether or not a byte had
# been replicated. See tests/lib/litestream.sh (pd-reyy).
# shellcheck source=tests/lib/litestream.sh
. "${REPO_DIR}/tests/lib/litestream.sh"

# PER-CHECKOUT by default, for the reason deploy/ci.sh derives its container
# instance the same way: the swarm runs several polecats per rig, each from its
# own worktree, and every one of them runs deploy/ci.sh — which runs this. With a
# fixed `drill` instance, run B's opening `podman volume rm -f` deleted run A's
# data volume between A creating it and A using it ("Error: no such volume
# pkdump-drill-data", observed 2026-08-08). Everything else here — the volume,
# the sidecar unit, the config dir, the podman secret, the MinIO container — is
# named after the instance, so making the instance unique makes all of them so.
INSTANCE=${DRILL_INSTANCE:-drill-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}
case "$INSTANCE" in
	prod|"") echo "refusing to drill instance '${INSTANCE}'" >&2; exit 1 ;;
esac
VOLUME="pkdump-${INSTANCE}-data"
SIDECAR="pkdump-litestream-${INSTANCE}"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
SECRET_NAME="pkdump-${INSTANCE}-s3-bootstrap"
QUADLET_DIR="${HOME}/.config/containers/systemd"

MINIO_CTR="pkdump-${INSTANCE}-minio"
# Published on the host, so it comes from the kernel and not from the checkout
# hash. A band gave this gate one deterministic number with no second chance at
# it — see tests/lib/ports.sh and pd-r0ri.
MINIO_PORT=${MINIO_PORT:-$(free_port)}

# FOUR tenants, and the victim is #2 — a restore that grabbed the first or the
# last prefix would still look right with one or two.
#
# A tenant here is TWO identifiers, because that is what the product is becoming
# (pd-fci1): a HANDLE that a person answers to, and an opaque DATABASE ID that
# names their file and their replica prefix. The drill uses opaque ids on
# purpose. With `tenants/alpha.sqlite` on disk, "restore the registry and check
# every tenant is reachable by handle" proves nothing — the filename already
# said `alpha`. With `tenants/01k2c7...sqlite` on disk, the registry is the ONLY
# thing that can attribute the file, which is precisely the property §7 tests.
#
# The ids are the shape pd-zr9n actually mints: 26 characters of UPPERCASE
# Crockford base32 (0-9 A-Z less I, L, O and U), which is what
# validate_database_id accepts on both sides of the fence. Fixed rather than
# generated so a failure is reproducible, with a distinguishable tail so a FAIL
# line still names the tenant it is about — but the CASE is not a detail to be
# tidied: `01J8…` and `01j8…` are one file on a case-insensitive filesystem and
# two S3 prefixes, and taking the lowercase shortcut here is what let this drill
# pass while deploy/litestream-lib.sh still rejected every real id (pd-vgof).
#
# TENANTS is the list of FIXTURE LABELS, not of handles, and after §7 the two
# are deliberately not the same thing: a rename changes the handle and touches
# nothing else, so the label keeps naming the same database, the same replica
# prefix, and the rows inside it (every row is `<label>-card-…`). HANDLE is the
# handle each label currently answers to, read off the registry the way anything
# else would have to. Nothing here derives a path or a URL from a handle.
TENANTS=(alpha bravo charlie delta)
declare -A DB_ID=(
	[alpha]=01K2C7HQ8N0000000000000AAA
	[bravo]=01K2C7HQ8N0000000000000BBB
	[charlie]=01K2C7HQ8N0000000000000CCC
	[delta]=01K2C7HQ8N0000000000000DDD
)
declare -A HANDLE=([alpha]=alpha [bravo]=bravo [charlie]=charlie [delta]=delta)
VICTIM=bravo
WRITER=delta      # keeps taking writes while the victim is being recovered
RENAMED=charlie   # renamed in §7, then restored under the new handle
DETACHED=alpha    # detached in §8, then restored from its replica
RENAMED_TO=charlize

WORK=${WORK:-$(mktemp -d /tmp/pkdump-drill.XXXXXX)}

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	local rc=$?
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving the '${INSTANCE}' instance up and WORK=$WORK in place."
		echo "  systemctl --user stop ${SIDECAR}.service"
		echo "  rm -f ${QUADLET_DIR}/${SIDECAR}.container && systemctl --user daemon-reload"
		echo "  podman rm -f --ignore ${MINIO_CTR}; podman volume rm -f ${VOLUME}"
		echo "  podman secret rm ${SECRET_NAME}; rm -rf ${CONF_DIR} ${WORK}"
		return $rc
	fi
	systemctl --user stop "${SIDECAR}.service" >/dev/null 2>&1 || true
	# Real-bucket mode leaves objects behind; delete them while the credentials
	# are still wired up (before the config dir and the secret go away).
	if [[ -n "${DRILL_REAL_S3:-}" && -n "${LITESTREAM_S3_BUCKET:-}" ]]; then
		echo "==> removing the throwaway prefix s3://${LITESTREAM_S3_BUCKET}/${DRILL_PREFIX:-}"
		aws_cli s3 rm --recursive "s3://${LITESTREAM_S3_BUCKET}/${DRILL_PREFIX}" >/dev/null 2>&1 || true
		echo "    objects remaining under it: $(aws_cli s3 ls --recursive "s3://${LITESTREAM_S3_BUCKET}/${DRILL_PREFIX}" 2>/dev/null | grep -c . || true)"
	fi
	rm -f "${QUADLET_DIR}/${SIDECAR}.container"
	systemctl --user daemon-reload >/dev/null 2>&1 || true
	podman rm -f --ignore "$MINIO_CTR" >/dev/null 2>&1 || true
	podman volume rm -f "$VOLUME" >/dev/null 2>&1 || true
	podman secret rm "$SECRET_NAME" >/dev/null 2>&1 || true
	rm -rf "$CONF_DIR" "$WORK"
	return $rc
}
trap cleanup EXIT

# aws_cli <args...> — the AWS CLI wearing the same credentials the sidecar and
# restore-litestream.sh wear (assume-role profile + bootstrap secret).
aws_cli() {
	local endpoint=()
	[ -n "${LITESTREAM_S3_ENDPOINT:-}" ] && endpoint=(--endpoint-url "$LITESTREAM_S3_ENDPOINT")
	podman run --rm \
		-v "${CONF_DIR}/aws/config:/aws/config:ro" \
		--secret "${SECRET_NAME},type=mount,target=/aws/credentials" \
		-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
		-e AWS_PROFILE="${AWS_PROFILE:-pkdump}" -e AWS_REGION="${LITESTREAM_S3_REGION}" \
		"$AWSCLI_IMAGE" "$@" "${endpoint[@]}"
}

# ls_cli <args...> — litestream, credentialled the same way. Used for read-only
# replica queries and for out-of-place verification restores (the "inspect before
# you commit" half of RESTORE.md scenario B).
ls_cli() {
	podman run --rm --user 0:0 -v "${WORK}/out:/out:Z" \
		-v "${CONF_DIR}/aws/config:/aws/config:ro" \
		--secret "${SECRET_NAME},type=mount,target=/aws/credentials" \
		-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials \
		-e AWS_PROFILE="${AWS_PROFILE:-pkdump}" \
		"$LITESTREAM_IMAGE" "$@"
}

# The drill stops and starts the sidecar far more often than an operator would,
# and its unit is deliberately rate-limited (StartLimitBurst=5 / 300s) so a
# crash-loop pages. reset-failed clears that counter — the same thing
# restore-litestream.sh does before its own restart.
# Quadlet names the container after the unit, and runs it with --rm — so this
# name is what `podman logs` reads, and its log starts empty on every start.
SIDECAR_CTR="systemd-${SIDECAR}"
sidecar_running() {
	[ "$(podman inspect -f '{{.State.Status}}' "$SIDECAR_CTR" 2>/dev/null || echo gone)" = running ]
}
# WAIT for the process to be gone, not for two seconds (pd-86er). `systemctl
# stop` already returns when the UNIT has deactivated; what the sleep covered
# was the container behind it, and every caller stops the sidecar precisely so
# that nothing is writing to the volume while it measures.
sidecar_stopped() { ! sidecar_running; }
sidecar_stop() {
	systemctl --user stop "${SIDECAR}.service"
	wait_until 30 0.25 sidecar_stopped || true
}
# ...and on the way up, wait for LITESTREAM rather than for systemd. `systemctl
# start` returns as soon as the unit is active, which is why this was a sleep at
# all: the thing worth waiting for is the replicator opening the databases, and
# it says so in its own log. A sidecar that is not running gets no wait — the
# caller's own check is what should report that, not a stall here.
#
# READY MEANS "HAS REPORTED A REPLICA POSITION", not "has named a file" (pd-pfxf).
# Every caller restarts this sidecar in order to ask deploy/backup-check.sh
# something, and that script's only source of a tenant's position is the
# `msg="replica sync"` line — which arrives seconds AFTER the `initialized db`
# line that a bare `.sqlite` match was satisfied by. §6 therefore ran against a
# sidecar that had been up one second, every tenant came back "not judged", and
# the drill reported 0 of 4 checked. Waiting for the condition the caller
# actually depends on removes the race rather than winning it on a fast box.
sidecar_ready() {
	sidecar_running || return 0
	logs_match "$SIDECAR_CTR" 'msg="replica sync"'
}
sidecar_start() {
	systemctl --user reset-failed "${SIDECAR}.service" 2>/dev/null || true
	systemctl --user start "${SIDECAR}.service"
	wait_until 60 0.25 sidecar_ready || true
}

# restored_rows <file> <replica-url> — restore out-of-place and count the rows;
# 0 when nothing restores yet, so it is safe to poll on. `-o` refuses to
# overwrite, and a poll asks repeatedly by definition, so the file goes first.
#
# This is the ground truth for "the write reached S3", and the three places
# below that used to sleep for replication now wait on it instead.
restored_rows() {
	local n
	rm -f "${WORK}/out/$1"
	ls_cli restore -o "/out/$1" "$2" >/dev/null 2>&1 || true
	# Always a number, whatever happened: the callers compare numerically, and an
	# empty string there is a shell error rather than "not replicated yet".
	n="$(sqlite3 -cmd '.timeout 5000' "${WORK}/out/$1" 'SELECT count(*) FROM collection;' 2>/dev/null)" || n=0
	printf '%s' "${n:-0}"
}

mount_point() { podman volume inspect -f '{{.Mountpoint}}' "$VOLUME"; }
# Everything below takes a HANDLE and goes through the id, because that is the
# indirection under test. Nothing in the drill derives a path from a handle
# except via DB_ID — the same way nothing in the product will, once the resolver
# reads the registry.
db_id() { printf '%s' "${DB_ID[$1]}"; }
tenant_file() { printf '%s/tenants/%s.sqlite' "$(mount_point)" "$(db_id "$1")"; }
registry_file() { printf '%s/registry.sqlite' "$(mount_point)"; }
# The rows, independent of page layout and of Litestream's own bookkeeping
# tables. "Untouched" has to mean the data, not just the bytes.
rows_hash() { sqlite3 "file:$(tenant_file "$1")?mode=ro" 'SELECT id, printing_id, phase FROM collection ORDER BY id;' 2>/dev/null | sha256sum | cut -d' ' -f1; }
rows_count() { sqlite3 "file:$(tenant_file "$1")?mode=ro" 'SELECT count(*) FROM collection;' 2>/dev/null || echo '<no db>'; }
rows_owner() { sqlite3 "file:$(tenant_file "$1")?mode=ro" "SELECT DISTINCT substr(printing_id, 1, instr(printing_id,'-')-1) FROM collection;" 2>/dev/null || echo '<no db>'; }

# ── "UNTOUCHED" MEANS THE DATA, AND ONLY THE DATA (pd-zk0c) ─────────────────
# There is no byte-equality assertion in this file, and there must not be one
# again. SQLite does not promise a stable file image across the operations a
# running box performs: a WAL checkpoint folds committed frames back into the
# main file, the freelist gets reused, pages get reordered. Every one of those
# rewrites the file and changes not one fact in it.
#
# That is not a hypothesis. §8 used to assert byte-identity and failed on it,
# and three measurements settled why:
#
#   * instrumenting deploy/restore-litestream.sh showed the bystanders'
#     sha256 identical BEFORE the restore, after the atomic swap with every
#     service still down, after the sidecar was restarted, and at the point
#     the drill measures. The restore path never touched them.
#   * the bytes had already moved BEFORE the restore ran — during ordinary
#     instance operation between the snapshot and the comparison.
#   * reproduced in isolation: against a database left with committed frames
#     in an uncheckpointed WAL (what killing the sidecar leaves behind),
#     `pkdump tenant list` — which opens each tenant READ_WRITE to read its
#     schema version — checkpoints on close and rewrites the main file. Row
#     count before and after: identical.
#
# So the old assertion forbade something the engine never offered, and it
# failed on the *drill's own* use of the product. It would have gone on
# failing intermittently until everyone learned to ignore this gate. The
# property it meant to state is real and is kept: restoring one tenant must
# not change any OTHER tenant's DATA.
#
# Database-level, not application-level, and deliberately: the app's read path
# would be the stronger statement in general, but `Database::open` re-applies
# the schema and adopts `user_version` — it WRITES to the file it is asked
# about, so measuring a bystander with it would be the measurement changing
# the thing measured. (`pkdump export --json` is out for a second reason:
# pd-ndkg records that it carries `_litestream_lock`/`_litestream_seq` in the
# envelope, which differ across a restart for exactly the benign reason this
# comment is about.) The tenant fixtures are also seeded with sqlite3 rather
# than the app, so the app has no more authority over them than sqlite3 does.
#
# Read-only throughout, for the same reason: opening one of these read-write
# would checkpoint it, and the measurement would move what it is measuring.
logical_dump() { # logical_dump <tenant> — canonical text of everything it MEANS
	local f uri tbl
	f="$(tenant_file "$1")"
	[ -e "$f" ] || { echo 'DATABASE MISSING'; return; }
	uri="file:${f}?mode=ro"
	# The engine's own verdict on the file, first, so a database that came back
	# corrupt is a difference rather than a silently equal pile of rows.
	echo "INTEGRITY $(sqlite3 -cmd '.timeout 5000' "$uri" 'PRAGMA integrity_check;' 2>&1 | tr '\n' ' ')"
	# Schema included: a restore that dropped a table off a neighbour would
	# otherwise read as "no rows changed".
	sqlite3 -cmd '.timeout 5000' "$uri" "
		SELECT 'SCHEMA ' || type || ' ' || name || ' ' || replace(COALESCE(sql, ''), char(10), ' ')
		  FROM sqlite_master
		 WHERE name NOT LIKE '\_litestream\_%' ESCAPE '\'
		 ORDER BY type, name;" 2>&1
	# Then every row of every table, sorted — so the comparison is of the facts
	# held, not of the order SQLite happens to hold them in.
	while IFS= read -r tbl; do
		echo "TABLE ${tbl}"
		sqlite3 -cmd '.timeout 5000' "$uri" "SELECT * FROM \"${tbl}\";" 2>&1 | sort | sed 's/^/  /'
	done < <(sqlite3 -cmd '.timeout 5000' "$uri" "
		SELECT name FROM sqlite_master
		 WHERE type = 'table' AND name NOT LIKE '\_litestream\_%' ESCAPE '\'
		 ORDER BY name;" 2>/dev/null)
}
# Snapshot every tenant's logical content, to be held the next restore against.
snapshot_logical() {
	local t
	mkdir -p "${WORK}/logical"
	for t in "${TENANTS[@]}"; do logical_dump "$t" > "${WORK}/logical/${t}"; done
}
# ...and the comparison. Prints `unchanged`/`changed` on stdout so it can be
# read by `check`, and the actual difference on stderr, because "changed" on
# its own is the diagnostic-free failure that made the old assertion so
# expensive to read.
logical_unchanged() { # logical_unchanged <tenant>
	local t=$1 now="${WORK}/logical/${1}.now"
	logical_dump "$t" > "$now"
	if cmp -s "${WORK}/logical/${t}" "$now"; then echo unchanged; return; fi
	{
		echo "    DIAG ${t}: logical content differs from the snapshot —"
		diff "${WORK}/logical/${t}" "$now" | head -20 | sed 's/^/      /'
	} >&2
	echo changed
}

# The real CLI, pointed at the drill's own data volume. §7 and §8 change registry
# state — a rename and a detach — and those are product operations: asserting
# them against a hand-written UPDATE would prove something about this file
# instead of about `pkdump tenant`. The rest of the drill still seeds with
# sqlite3, because the backup layer is what it is drilling.
PKDUMP_BIN=${PKDUMP_BIN:-${CARGO_TARGET_DIR:-${REPO_DIR}/target}/debug/pkdump}
pk() { PKDUMP_HOME="$(mount_point)" "$PKDUMP_BIN" "$@"; }
# The registry's own answer to "which database does this handle name?", asked of
# the live registry through the CLI. Row state is a separate column, so the two
# questions are two functions rather than one with a flag.
#
# HANDLE / DATABASE ID / CREATED / STATE / RETIRED, whitespace-separated — so
# $4 is the state only because CREATED is a single token (see the seed in §2). A
# row whose database is missing gets `(DATABASE MISSING)` appended, which is why
# nothing here counts columns from the end.
registry_id() { pk tenant list | awk -v h="$1" -v s="${2:-active}" '$1 == h && $4 == s {print $2}'; }
registry_state() { pk tenant list | awk -v i="$1" '$2 == i {print $4}'; }
# ...and asked of a registry FILE, which is what §9 has to do: it is reading a
# registry that was just restored onto a bare volume, before anything is running.
registry_file_id() { # registry_file_id <handle>
	sqlite3 -cmd '.timeout 5000' "$(registry_file)" \
		"SELECT database_id FROM user WHERE handle = '$1';" 2>/dev/null
}

write_phase() { # write_phase <phase>
	local t
	for t in "${TENANTS[@]}"; do
		[ -e "$(tenant_file "$t")" ] || continue
		sqlite3 "$(tenant_file "$t")" \
			"INSERT INTO collection (printing_id, acquired_at, source, phase)
			 VALUES ('$t-card-$1', datetime('now'), 'drill', '$1');"
	done
}

wait_for_replica() { # wait_for_replica <tenant> [seconds]
	local url
	url="$(tenant_replica_url "$(db_id "$1")")"
	# One second, not two: the body is itself a `podman run` costing the better
	# part of one, so anything longer was latency (pd-86er). The caller's bound is
	# unchanged. What the body ASKS changed with pd-reyy — it waits for LTX files
	# now, not for S3 to answer at all.
	wait_until "${2:-60}" 1 replica_holds_data ls_cli "$url"
}

if [ ! -x "$PKDUMP_BIN" ]; then
	echo "==> building pkdump (${PKDUMP_BIN} not present)..."
	( cd "$REPO_DIR" && cargo build --quiet --bin pkdump )
fi

# ── 1. the S3 target ────────────────────────────────────────────────────────
if [ -n "${DRILL_REAL_S3:-}" ]; then
	SRC="${DRILL_SOURCE_INSTANCE:-prod}"
	log "1. REAL bucket (instance '${SRC}'), throwaway prefix"
	[ -f "${HOME}/.config/pkdump/${SRC}/litestream.env" ] || { echo "no litestream.env for '${SRC}'" >&2; exit 1; }
	set -a; . "${HOME}/.config/pkdump/${SRC}/litestream.env"; set +a
	# The drill writes ONLY under its own prefix, never the instance's.
	DRILL_PREFIX="pd-v8zf-drill/${INSTANCE}"
	export LITESTREAM_S3_PATH="${DRILL_PREFIX}/tenants"
	# Beside the tenants prefix, never inside it — §7 lists that parent prefix
	# to enumerate tenants, and a registry underneath would read as one more.
	export LITESTREAM_S3_REGISTRY_PATH="${DRILL_PREFIX}/registry.sqlite"
	export LITESTREAM_S3_ENDPOINT=""
	echo "  bucket ${LITESTREAM_S3_BUCKET}, prefix ${LITESTREAM_S3_PATH} (deleted on exit)"
else
	log "1. throwaway MinIO"
	AKID="drill$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
	SECRET_KEY="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
	DRILL_PREFIX="drill"
	export LITESTREAM_S3_BUCKET=pkdump-drill
	export LITESTREAM_S3_REGION=us-west-2
	export LITESTREAM_S3_PATH="${DRILL_PREFIX}/tenants"
	# Beside the tenants prefix, never inside it — §7 lists that parent prefix
	# to enumerate tenants, and a registry underneath would read as one more.
	export LITESTREAM_S3_REGISTRY_PATH="${DRILL_PREFIX}/registry.sqlite"
	# Rootless Podman puts these containers on slirp4netns, where a container
	# cannot reach a port the host bound to 127.0.0.1 — and restore-litestream.sh
	# deliberately runs its restore container with no --network, exactly as an
	# operator would. So MinIO is published on the host interface that
	# host.containers.internal names, with credentials generated per run.
	export LITESTREAM_S3_ENDPOINT="http://host.containers.internal:${MINIO_PORT}"
	mkdir -p "$WORK/minio"
	podman rm -f --ignore "$MINIO_CTR" >/dev/null 2>&1 || true
	podman run -d --name "$MINIO_CTR" -p "${MINIO_PORT}:9000" \
		-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET_KEY" \
		-v "$WORK/minio:/data:Z" "$MINIO_IMAGE" server /data >/dev/null
	for _ in $(seq 60); do
		curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null 2>&1 && break
		sleep 1
	done
	curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null
	echo "  minio up on ${MINIO_PORT}, bucket ${LITESTREAM_S3_BUCKET}"
fi
export AWS_PROFILE=pkdump
mkdir -p "$WORK/out"

# ── 2. an instance shaped exactly like a deployed one ───────────────────────
log "2. instance '${INSTANCE}': data volume, backup config, sidecar unit"
podman secret rm "$SECRET_NAME" >/dev/null 2>&1 || true   # a KEEP=1 run may have left one
mkdir -p "${CONF_DIR}/aws"
chmod 700 "$CONF_DIR" "${CONF_DIR}/aws"
if [ -n "${DRILL_REAL_S3:-}" ]; then
	# The real thing: the instance's assume-role profile and its bootstrap key,
	# copied into the drill instance's own config so nothing here writes to the
	# source instance's files.
	cp "${HOME}/.config/pkdump/${SRC}/aws/config" "${CONF_DIR}/aws/config"
	podman secret inspect --showsecret -f '{{.SecretData}}' "pkdump-${SRC}-s3-bootstrap" \
		| podman secret create "$SECRET_NAME" - >/dev/null
else
	# MinIO cannot assume a role, so the profile carries the static test key
	# directly. This is the ONE deviation from production credentials; the
	# assume-role path is verified against the real bucket by DRILL_REAL_S3=1
	# (and by pd-fof4).
	printf '[profile pkdump]\nregion=%s\n' "$LITESTREAM_S3_REGION" > "${CONF_DIR}/aws/config"
	printf '[pkdump]\naws_access_key_id=%s\naws_secret_access_key=%s\n' "$AKID" "$SECRET_KEY" \
		| podman secret create "$SECRET_NAME" - >/dev/null
fi
cat > "${CONF_DIR}/litestream.env" <<EOF
LITESTREAM_S3_BUCKET=${LITESTREAM_S3_BUCKET}
LITESTREAM_S3_REGION=${LITESTREAM_S3_REGION}
LITESTREAM_S3_PATH=${LITESTREAM_S3_PATH}
LITESTREAM_TENANTS_DIR=/data/tenants
LITESTREAM_REGISTRY_DB=/data/registry.sqlite
LITESTREAM_S3_REGISTRY_PATH=${LITESTREAM_S3_REGISTRY_PATH}
LITESTREAM_S3_ENDPOINT=${LITESTREAM_S3_ENDPOINT}
AWS_PROFILE=pkdump
EOF
chmod 600 "${CONF_DIR}/litestream.env"
[ -n "${DRILL_REAL_S3:-}" ] || aws_cli s3 mb "s3://${LITESTREAM_S3_BUCKET}" >/dev/null 2>&1 || true

podman volume rm -f "$VOLUME" >/dev/null 2>&1 || true
podman volume create "$VOLUME" >/dev/null
MP="$(mount_point)"
mkdir -p "${MP}/tenants"
# The catalog sits beside tenants/, is reproducible from upstream, and must not
# be replicated — a decoy here proves the glob cannot reach it.
sqlite3 "${MP}/shared.sqlite" 'CREATE TABLE cards (id TEXT);'
for t in "${TENANTS[@]}"; do
	sqlite3 "$(tenant_file "$t")" \
		"PRAGMA journal_mode=WAL;
		 CREATE TABLE collection (
		     id          INTEGER PRIMARY KEY AUTOINCREMENT,
		     printing_id TEXT NOT NULL,
		     acquired_at TEXT NOT NULL,
		     source      TEXT NOT NULL,
		     phase       TEXT NOT NULL);" >/dev/null
done
# Every row names its own tenant, so a restore that hands back a neighbour's
# collection is caught by READING THE DATA, not by trusting the prefix.
write_phase early

# The user registry (pd-nd6w). At the data ROOT, deliberately outside tenants/,
# holding the only statement anywhere of which file belongs to whom.
#
# Applied from the SHIPPED schema rather than a copy of it, so this fixture
# cannot drift away from the table the product actually writes. Seeded with
# sqlite3 rather than the app for the same reason the tenant databases are: the
# drill exercises the BACKUP layer and should not need a build to run.
sqlite3 "$(registry_file)" 'PRAGMA journal_mode=WAL;' >/dev/null
sqlite3 "$(registry_file)" < "${REPO_DIR}/crates/pkdump-db/src/schema_registry.sql"
# RFC3339, with no space in it, because that is what `registry::insert` writes —
# and because `pkdump tenant list` prints the registry as a whitespace-separated
# table, so a `datetime('now')` fixture (which has a space) would silently shift
# every column after CREATED and make the STATE this drill reads back the time.
for t in "${TENANTS[@]}"; do
	sqlite3 "$(registry_file)" \
		"INSERT INTO user (handle, database_id, created_at, state)
		 VALUES ('${HANDLE[$t]}', '$(db_id "$t")', strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'active');"
done
echo "  ${#TENANTS[@]} tenants on ${VOLUME}, named by opaque database id:"
for t in "${TENANTS[@]}"; do echo "    ${HANDLE[$t]} -> tenants/$(db_id "$t").sqlite"; done
echo "  registry.sqlite at the data root holds that mapping and nothing else does"

# The shipped Quadlet sidecar unit, installed the way deploy/setup.sh installs it.
mkdir -p "$QUADLET_DIR"
sed -e "s|{{INSTANCE}}|${INSTANCE}|g" -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
	"${REPO_DIR}/deploy/pkdump-litestream.container" > "${QUADLET_DIR}/${SIDECAR}.container"
pkdump_store_stamp_unit "${QUADLET_DIR}/${SIDECAR}.container"
systemctl --user daemon-reload
sidecar_start
check "the shipped sidecar unit is running" "active" \
	"$(systemctl --user is-active "${SIDECAR}.service" || true)"

replicating=0
for t in "${TENANTS[@]}"; do
	wait_for_replica "$t" 90 && replicating=$((replicating + 1)) || echo "  ${t} never reached S3"
done
check "every tenant is replicating from the one sidecar" "4" "$replicating"

# The registry rides the SAME sidecar — no second process, no second unit, no
# config edit beyond the entry deploy/litestream.yml already ships.
REG_URL="$(registry_replica_url)"
reg_replicating=no
registry_replicating() {
	replica_holds_data ls_cli "$REG_URL" || return 1
	reg_replicating=yes
}
wait_until 90 1 registry_replicating || true
check "the registry is replicating from that same sidecar" "yes" "$reg_replicating"
# Beside the tenants prefix, never inside it: §7 lists that parent prefix to
# enumerate tenants, and a registry underneath it would read as one more tenant.
check "the registry's prefix is NOT under the tenants prefix" "outside" \
	"$(case "$LITESTREAM_S3_REGISTRY_PATH" in "${LITESTREAM_S3_PATH%/}"/*) echo inside ;; *) echo outside ;; esac)"

# A marker between two write phases, for the point-in-time restore in §5.
#
# WAIT for the early phase to be IN the replica before taking the marker, rather
# than sleeping and hoping (pd-86er). `wait_for_replica` above only proves the
# replica holds LTX files at all; §5 rolls ${VICTIM} back TO this instant and
# expects the early row to still be there, so what has to be true here is that
# the row itself has landed.
VICTIM_URL="$(tenant_replica_url "$(db_id "$VICTIM")")"
early_replicated() { [ "$(restored_rows victim-early.sqlite "$VICTIM_URL")" -ge 1 ]; }
wait_until 120 2 early_replicated || true

MARKER=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# The one sleep in this file that is not waiting for a condition, and it stays:
# `date -u +…%SZ` truncates to the second, so the marker names the whole second
# it was taken in and the late writes have to land in a LATER one — otherwise a
# restore at the marker may legitimately include them. The wait is on the
# clock's resolution itself; there is nothing to poll.
sleep 2
write_phase late
# ...and the far side of the marker, on the same terms: the late phase is in the
# replica when a restore hands back both rows. Asked of the two tenants §4 and §5
# actually restore — ${VICTIM}, whose stream has to extend past the marker for
# the rollback to have anything to roll back FROM, and charlie, the bystander
# whose replica must still be at CURRENT. Each of these is a container start, so
# asking all four would be paying twice for one fact: the write phase is a single
# loop and the sidecar syncs every database on the same interval.
late_replicated() {
	local t
	for t in "$VICTIM" charlie; do
		[ "$(restored_rows "late-${t}.sqlite" "$(tenant_replica_url "$(db_id "$t")")")" -ge 2 ] || return 1
	done
}
wait_until 180 2 late_replicated || true
check "each tenant has both write phases locally" "2 2 2 2" \
	"$(for t in "${TENANTS[@]}"; do printf '%s ' "$(rows_count "$t")"; done | sed 's/ $//')"

# ── 3. the state the drill will hold the restore against ────────────────────
log "3. record every tenant's state with the sidecar stopped"
# Stopped first so nothing is mid-checkpoint: from here to the post-restore
# measurement, the ONLY thing that runs against this volume is the restore.
sidecar_stop
declare -A BEFORE_ROWS
snapshot_logical
for t in "${TENANTS[@]}"; do
	BEFORE_ROWS[$t]="$(rows_hash "$t")"
	echo "  ${t}  rows ${BEFORE_ROWS[$t]:0:12}  ($(rows_count "$t") rows)"
done

# ── 4. RESTORE.md scenario A — a tenant's live database is gone ─────────────
log "4. scenario A: delete ${VICTIM}'s live database, restore it in place"
rm -f "$(tenant_file "$VICTIM")" "$(tenant_file "$VICTIM")-wal" "$(tenant_file "$VICTIM")-shm"
check "${VICTIM}'s database really is gone" "gone" \
	"$([ -e "$(tenant_file "$VICTIM")" ] && echo present || echo gone)"

# One sidecar means a restore pauses replication for EVERYONE, briefly. The
# runbook says nothing is lost in that window — so ${WRITER} takes a write while
# the sidecar is down, and §4 below checks that write reached S3 afterwards.
sqlite3 "$(tenant_file "$WRITER")" \
	"INSERT INTO collection (printing_id, acquired_at, source, phase)
	 VALUES ('${WRITER}-card-during', datetime('now'), 'drill', 'during');"

bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$(db_id "$VICTIM")"

# Measure with the sidecar stopped again: its restart writes Litestream's own
# bookkeeping tables into every database it picks up, which is not the restore's
# doing and must not be confused for it.
sidecar_stop
check "${VICTIM} is back" "2" "$(rows_count "$VICTIM")"
check "and it is ${VICTIM}'s own collection" "$VICTIM" "$(rows_owner "$VICTIM")"
check "restored ${VICTIM} row-for-row" "${BEFORE_ROWS[$VICTIM]}" "$(rows_hash "$VICTIM")"

untouched=0
for t in "${TENANTS[@]}"; do
	{ [ "$t" = "$VICTIM" ] || [ "$t" = "$WRITER" ]; } && continue
	[ "$(logical_unchanged "$t")" = unchanged ] && untouched=$((untouched + 1)) \
		|| echo "  FAIL  ${t}'s data changed during a restore of ${VICTIM}"
done
check "every bystander tenant's DATA is untouched across the restore" "2" "$untouched"
check "and ${WRITER}'s own write during the restore is still there" "3" "$(rows_count "$WRITER")"

sidecar_start
# The neighbours' replicas must also survive: restoring one tenant neither
# consumes nor invalidates another's stream.
ls_cli restore -integrity-check full -o /out/charlie-after.sqlite "$(tenant_replica_url "$(db_id charlie)")" >/dev/null 2>&1 || true
check "an untouched tenant still restores to current from its own replica" "2" \
	"$(sqlite3 "${WORK}/out/charlie-after.sqlite" 'SELECT count(*) FROM collection;' 2>/dev/null || echo '<no db>')"
check "and it is charlie's data, not ${VICTIM}'s" "charlie" \
	"$(sqlite3 "${WORK}/out/charlie-after.sqlite" "SELECT DISTINCT substr(printing_id,1,instr(printing_id,'-')-1) FROM collection;" 2>/dev/null || echo '<no db>')"
# The write taken while the sidecar was down is not lost: it is replicated when
# the sidecar resumes. This is the whole cost of one sidecar for N tenants.
#
# WAIT for the catch-up instead of assuming eight seconds of sidecar uptime
# covered it (pd-86er). The assertion below is unchanged and still the thing
# being proved — a replica that never catches up fails it, boundedly, and the
# poll is what makes the wait proportional to the box rather than to a guess.
WRITER_URL="$(tenant_replica_url "$(db_id "$WRITER")")"
writer_caught_up() { [ "$(restored_rows writer-probe.sqlite "$WRITER_URL")" -ge 3 ]; }
wait_until 120 2 writer_caught_up || true
rm -f "${WORK}/out/writer-after.sqlite"
ls_cli restore -integrity-check full -o /out/writer-after.sqlite "$WRITER_URL" >/dev/null 2>&1 || true
check "a write made while the sidecar was down reached S3 once it resumed" "3" \
	"$(sqlite3 "${WORK}/out/writer-after.sqlite" 'SELECT count(*) FROM collection;' 2>/dev/null || echo '<no db>')"

# ── 5. RESTORE.md scenario B — roll ONE tenant back in time ─────────────────
log "5. scenario B: point-in-time restore of ${VICTIM} to ${MARKER}"
sidecar_stop
# The last moment at which ${VICTIM}'s replica still holds the CURRENT state —
# used below to prove the rollback itself is undoable.
PRE_ROLLBACK=$(date -u +%Y-%m-%dT%H:%M:%SZ)
snapshot_logical
for t in "${TENANTS[@]}"; do BEFORE_ROWS[$t]="$(rows_hash "$t")"; done

bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --at="$MARKER" "$INSTANCE" "$(db_id "$VICTIM")"

sidecar_stop
check "${VICTIM} rolled back to the marker (early phase only)" "1" "$(rows_count "$VICTIM")"
check "and it is still ${VICTIM}'s data" "$VICTIM" "$(rows_owner "$VICTIM")"
untouched=0
for t in "${TENANTS[@]}"; do
	[ "$t" = "$VICTIM" ] && continue
	[ "$(logical_unchanged "$t")" = unchanged ] && untouched=$((untouched + 1)) \
		|| echo "  FAIL  ${t} changed during a point-in-time restore of ${VICTIM}"
done
check "a point-in-time rollback of one tenant leaves the others at CURRENT" "3" "$untouched"

# A rollback is not a one-way door, and the runbook says so — but only because
# this is checked. Once the sidecar comes back it replicates the ROLLED-BACK
# database, so from that moment "latest" IS the rollback: recovering the state
# the operator just discarded means asking for a timestamp from before it. The
# replica is append-only (the rollback lands as a new txid, it does not rewrite
# history), so that state is still there.
sidecar_start
# The rolled-back database has to REACH the replica before "latest" is the
# rollback — that is the whole claim of the next three lines, and it was a
# fixed eight seconds of sidecar uptime. Poll for the replica to hand back one
# row (pd-86er).
rollback_replicated() { [ "$(restored_rows victim-rollback.sqlite "$VICTIM_URL")" = 1 ]; }
wait_until 120 2 rollback_replicated || true
sidecar_stop
bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$(db_id "$VICTIM")"
sidecar_stop
check "after the rollback replicates, 'latest' is the rolled-back state" "1" "$(rows_count "$VICTIM")"

bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --at="$PRE_ROLLBACK" "$INSTANCE" "$(db_id "$VICTIM")"
sidecar_stop
check "and the discarded state comes back from just before the rollback" "2" \
	"$(rows_count "$VICTIM")"
check "still ${VICTIM}'s own data" "$VICTIM" "$(rows_owner "$VICTIM")"
sidecar_start

# ── 6. the freshness checker agrees, per tenant ─────────────────────────────
log "6. deploy/backup-check.sh: every tenant, including the restored one"
# backup-check.sh sources the HOST-WIDE alerts.env first and the INSTANCE's
# second, so writing the instance's is what makes these arms deterministic on a
# box that has a host-wide one.
#
# The blank Pushover pair is not tidiness. A stale verdict calls deploy/alert.sh,
# which pushes with whatever credentials are in the environment — so without
# this, the arm below that deliberately fails would send a real push
# notification to a real phone on every CI run. A drill does not page anybody.
bc() { # bc <ping url> [tenant...] -> sets BC_OUT / BC_RC
	{
		printf 'PKDUMP_BACKUP_PING_URL=%s\n' "$1"
		printf 'PUSHOVER_TOKEN=\nPUSHOVER_USER=\n'
	} > "${CONF_DIR}/alerts.env"
	shift
	BC_OUT="$(bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" "$@" 2>&1)" && BC_RC=0 || BC_RC=$?
	printf '%s\n' "$BC_OUT" | sed 's/^/    /'
}

# The dead-man's switch is what tells the operator backups RESUMED after a
# restore. Point its ping at a closed port: a failed ping only warns, so this
# exercises the real S3 freshness query without needing a monitor.
bc http://127.0.0.1:1/drill
check "backup-check reports every tenant fresh after the restore" "0" "$BC_RC"
check "and it checked all four" "4" \
	"$(printf '%s\n' "$BC_OUT" | grep -cE "^backup-check: tenant .* OK — replica" || true)"
# The registry is checked too, separately — its silent loss is the one failure
# the per-tenant loop above cannot see — and by a DIFFERENT TEST (pd-me6h):
# correspondence with its replica, not the age of it. A restored registry that
# nobody has written to since is exactly the state freshness got wrong.
check "and it checked the registry as well" "1" \
	"$(printf '%s\n' "$BC_OUT" | grep -c "^backup-check: the user registry OK — replica in correspondence" || true)"

# NO CHECK MAY PASS BY SKIPPING (pd-7f46). With no monitor URL configured this
# script used to print a skip and exit 0 — a pass, to anything reading its exit
# status, having asked S3 nothing at all. Unarming the monitor must cost the
# PING and nothing else.
bc ""
check "with the monitor unarmed, backup-check still verifies against S3" "0" "$BC_RC"
check "and it still checked all four tenants" "4" \
	"$(printf '%s\n' "$BC_OUT" | grep -cE "^backup-check: tenant .* OK — replica" || true)"
check "and the registry" "1" \
	"$(printf '%s\n' "$BC_OUT" | grep -c "^backup-check: the user registry OK — replica in correspondence" || true)"
check "and it says the dead-man's switch is not armed" "1" \
	"$(printf '%s\n' "$BC_OUT" | grep -c "NOT armed" || true)"
check "and it did NOT report skipping" "0" \
	"$(printf '%s\n' "$BC_OUT" | grep -ci "skipping" || true)"

# ...and the zero above is an answer rather than an absence of one: a database
# with nothing under its prefix FAILS, with the monitor still unarmed.
#
# The file is left BRAND NEW on purpose (pd-30yy). Its age is not what the
# judgement turns on and must not be — this drill used to age it with `touch -d
# '3 days ago'`, which sets mtime and not birth time, so once the checker read
# birth time the orphan came back "created 0m ago, not judged" and an
# unreplicated tenant PASSED. Neither timestamp was ever the right question:
# the sidecar is stopped, so nothing is replicating this database, and that is
# the fact the verdict rests on. The matching pair — a sidecar that is UP and has
# never named the file — is tests/alarming/run.sh §5.4.
ORPHAN_ID=01K2C7HQ8N0000000000000EEE
sidecar_stop
sqlite3 "${MP}/tenants/${ORPHAN_ID}.sqlite" 'CREATE TABLE collection (id INTEGER PRIMARY KEY);'
bc "" "$ORPHAN_ID"
check "an unreplicated tenant FAILS the check even with no monitor to alert" "1" "$BC_RC"
check "and it says which one, and why" "1" \
	"$(printf '%s\n' "$BC_OUT" | grep -c "^backup-check: STALE — tenant '${ORPHAN_ID}'" || true)"
rm -f "${MP}/tenants/${ORPHAN_ID}.sqlite"
sidecar_start

# ── 7. recovery after a RENAME ──────────────────────────────────────────────
log "7. ${RENAMED} is renamed to ${RENAMED_TO}, then recovered under the new handle"
# The claim this epic rests on: a handle is a LABEL and the database_id is the
# IDENTITY. A rename is therefore one UPDATE, and the file, the replica prefix
# and the recovery window all stay exactly where they were — which was
# impossible while the name WAS the path (a rename moved the file, so it started
# a new prefix and a new window, and pd-1717's silent replication loss lived in
# that gap).
#
# Asserted through `pkdump tenant rename`, not through an UPDATE of our own: the
# property belongs to the product.
RENAMED_URL_BEFORE="$(tenant_replica_url "$(db_id "$RENAMED")")"
RENAMED_FILE_BEFORE="$(tenant_file "$RENAMED")"
RENAMED_ROWS_BEFORE="$(rows_hash "$RENAMED")"
# One prefix per database, and the count is here so §7 can show that a rename
# did not mint a fifth.
prefix_count() {
	aws_cli s3 ls "s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_PATH}/" 2>/dev/null \
		| grep -c 'PRE' || true
}
PREFIXES_BEFORE="$(prefix_count)"

sidecar_stop
snapshot_logical
for t in "${TENANTS[@]}"; do BEFORE_ROWS[$t]="$(rows_hash "$t")"; done
pk tenant rename "${HANDLE[$RENAMED]}" "$RENAMED_TO" | sed 's/^/    /'
HANDLE[$RENAMED]=$RENAMED_TO

check "the new handle maps to the SAME database id" "$(db_id "$RENAMED")" \
	"$(registry_id "$RENAMED_TO")"
check "and the old handle resolves to nothing at all" "" "$(registry_id "$RENAMED")"
check "the database did not move: same path, still there" "yes" \
	"$([ "$(tenant_file "$RENAMED")" = "$RENAMED_FILE_BEFORE" ] && [ -e "$RENAMED_FILE_BEFORE" ] && echo yes || echo no)"
# And nothing in it changed. This assertion USED to be a file sha256 — "not one
# byte of it changed" — which passed only because a rename happens not to leave
# a checkpoint behind. It is the same claim §8 was failing on and it would have
# failed the first time the surrounding procedure touched the database (pd-zk0c).
check "and nothing in it changed either" "unchanged" "$(logical_unchanged "$RENAMED")"
check "and nothing on disk is named by either handle" "0" \
	"$(find "$MP" -name "${RENAMED}*" -o -name "${RENAMED_TO}*" | grep -c . || true)"
check "the replica prefix is unchanged, so the recovery window is unbroken" "same" \
	"$([ "$(tenant_replica_url "$(db_id "$RENAMED")")" = "$RENAMED_URL_BEFORE" ] && echo same || echo different)"
check "and the rename did not mint a new prefix in the bucket" "$PREFIXES_BEFORE" \
	"$(prefix_count)"

# Now the recovery itself, run the way the runbook says: ask the registry which
# database the person called ${RENAMED_TO} is, then restore THAT.
rm -f "$(tenant_file "$RENAMED")" "$(tenant_file "$RENAMED")-wal" "$(tenant_file "$RENAMED")-shm"
check "${RENAMED_TO}'s live database is gone" "gone" \
	"$([ -e "$(tenant_file "$RENAMED")" ] && echo present || echo gone)"
RENAMED_ID="$(registry_id "$RENAMED_TO")"
RS_OUT="$(bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$RENAMED_ID" 2>&1)"
printf '%s\n' "$RS_OUT" | sed 's/^/    /'
check "the restore attributes the file to the NEW handle" "1" \
	"$(printf '%s\n' "$RS_OUT" | grep -c "Attribution: ${RENAMED_ID} belongs to ${RENAMED_TO} (active)" || true)"
sidecar_stop
check "${RENAMED_TO}'s collection is back, row for row" "$RENAMED_ROWS_BEFORE" "$(rows_hash "$RENAMED")"
# And it is the collection she had BEFORE the rename — the rows still say
# `${RENAMED}-card-…`, because a rename is not a data migration.
check "and it is the same collection she had under her old handle" "$RENAMED" \
	"$(rows_owner "$RENAMED")"
untouched=0
for t in "${TENANTS[@]}"; do
	[ "$t" = "$RENAMED" ] && continue
	[ "$(logical_unchanged "$t")" = unchanged ] && untouched=$((untouched + 1)) \
		|| echo "  FAIL  ${t} changed across a rename + restore of ${RENAMED_TO}"
done
check "the other three tenants are unchanged across all of it" "3" "$untouched"
sidecar_start

# ── 8. recovery of a DETACHED tenant ────────────────────────────────────────
log "8. ${DETACHED} is detached, then her KEPT collection is recovered"
# `pkdump tenant remove` no longer deletes anything: it releases the handle and
# keeps the database and its replica. That is only a defensible default if the
# kept data genuinely comes back — otherwise it is a promise the box cannot
# honour. So: detach, lose the live file, and restore it.
sidecar_stop
snapshot_logical
for t in "${TENANTS[@]}"; do BEFORE_ROWS[$t]="$(rows_hash "$t")"; done
DETACHED_ID="$(db_id "$DETACHED")"
DETACHED_ROWS_BEFORE="$(rows_hash "$DETACHED")"
pk tenant detach "${HANDLE[$DETACHED]}" --yes | sed 's/^/    /'

check "the handle is free the moment it is released" "" "$(registry_id "${HANDLE[$DETACHED]}")"
# The partial unique index doing its work: no live user answers to the name, and
# the surviving row still carries the person's REAL handle, so these bytes stay
# attributable by reading a column rather than by parsing a composite name.
check "the row survives under her real handle, state detached" "$DETACHED_ID" \
	"$(registry_id "${HANDLE[$DETACHED]}" detached)"
check "detach kept the database" "present" \
	"$([ -e "$(tenant_file "$DETACHED")" ] && echo present || echo absent)"
sidecar_start
check "and it is still in the replicated set — the glob does not read state" "replicating" \
	"$(wait_for_replica "$DETACHED" 90 && echo replicating || echo silent)"

# WAIT FOR THE DETACH ITSELF TO REACH S3 before the sidecar stops again.
#
# The detach above was written to the registry while the sidecar was STOPPED, so
# nothing had replicated it yet. The wait immediately above proves the detached
# TENANT is replicating — a different database. Without this, whether §11's
# restore sees the detach comes down to how much Litestream happened to flush in
# the few seconds this sidecar is up, and on a loaded box it flushes nothing:
# §11 then restores the PRE-detach registry and reports
# "expected 3 1, got 4 0" — a state assertion 70 lines away from its own cause.
#
# Asserted rather than `|| true`: if the detach never lands, the honest failure is
# "the registry replica never caught up", not "the states did not survive".
reg_detach_replicated() {
	rm -f "${WORK}/out/reg-detach.sqlite"
	ls_cli restore -o "/out/reg-detach.sqlite" "$REG_URL" >/dev/null 2>&1 || return 1
	[ "$(sqlite3 -cmd '.timeout 5000' "${WORK}/out/reg-detach.sqlite" \
		"SELECT count(*) FROM user WHERE state='detached';" 2>/dev/null || echo 0)" -ge 1 ]
}
wait_until 180 2 reg_detach_replicated || true
check "the detach itself reached the registry replica" "yes" \
	"$(reg_detach_replicated && echo yes || echo no)"

sidecar_stop
rm -f "$(tenant_file "$DETACHED")" "$(tenant_file "$DETACHED")-wal" "$(tenant_file "$DETACHED")-shm"
DS_OUT="$(bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$DETACHED_ID" 2>&1)"
printf '%s\n' "$DS_OUT" | sed 's/^/    /'
# A detached user is attributable, so the restore does not need --unattributed.
# The row is the whole reason detach-not-delete is safe.
check "the restore still knows whose it was" "1" \
	"$(printf '%s\n' "$DS_OUT" | grep -c "Attribution: ${DETACHED_ID} belongs to ${DETACHED} (detached)" || true)"
sidecar_stop
check "a detached tenant's collection comes back row for row" "$DETACHED_ROWS_BEFORE" \
	"$(rows_hash "$DETACHED")"
check "and it is her own data" "$DETACHED" "$(rows_owner "$DETACHED")"
untouched=0
for t in "${TENANTS[@]}"; do
	[ "$t" = "$DETACHED" ] && continue
	[ "$(logical_unchanged "$t")" = unchanged ] && untouched=$((untouched + 1)) \
		|| echo "  FAIL  ${t} changed during a restore of the detached ${DETACHED}"
done
check "and the live tenants are unchanged through it" "3" "$untouched"
sidecar_start

# ── 9. RESTORE.md scenario C — the whole volume is gone ─────────────────────
log "9. scenario C: total loss — restore the REGISTRY FIRST, then the tenants"
# The question this section settles: after losing the box, how does the operator
# know which databases to restore and WHOSE they are?
#
# The rejected libSQL/sqld path had a DR gap where bottomless did not back up the
# namespace registry, so recovery had to re-declare every namespace before any
# data would come back. We do not get to point at that gap unless our own
# registry is IN the replicated set — which is what §2 established, and what this
# section spends.
#
# ORDER IS THE POINT. Restore the registry first and the tenant list, with names
# attached, falls out of it. Restore the tenants first and you have a directory
# of opaque ids: every byte present, nothing attributable. The drill proves the
# second half rather than asserting it — it wipes the volume, then LOOKS at what
# S3 alone can tell you before the registry comes back.
#
# By this point the four tenants are in FOUR DIFFERENT registry states, which is
# the state a box that has been running for a year is actually in and the state
# a fresh fixture never reaches: one live and never touched (${WRITER}), one
# restored twice and rolled back (${VICTIM}), one RENAMED (${RENAMED} ->
# ${RENAMED_TO}), and one DETACHED (${DETACHED}). Total loss has to recover all
# four, and the handle it hands back has to be the CURRENT one.
sidecar_stop
rm -rf "${MP}/tenants" "${MP}/shared.sqlite" "$(registry_file)" "$(registry_file)-wal" "$(registry_file)-shm"
check "the data volume is empty" "0" "$(find "${MP}" -name '*.sqlite' | grep -c . || true)"

# What the bucket knows on its own: four database ids, and not one handle.
mapfile -t PREFIXES < <(aws_cli s3 ls "s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_PATH}/" 2>/dev/null \
	| awk '/PRE/ {print $2}' | sed 's|\.sqlite/$||' | sort)
echo "  the tenant prefixes S3 offers, with the registry gone:"
printf '    %s\n' "${PREFIXES[@]-}"
check "S3 alone yields the right NUMBER of databases" "4" "${#PREFIXES[@]}"
# Against every handle these four have ever answered to, including the one that
# was renamed away and the one that was released.
check "...and not one of them is a handle — the files are anonymous" "0" \
	"$(comm -12 <(printf '%s\n' "${PREFIXES[@]-}" | sort) \
	            <(printf '%s\n%s\n' "${TENANTS[*]}" "${HANDLE[*]}" | tr ' ' '\n' | sort -u) \
	   | grep -c . || true)"

# STEP ONE: the registry. It is restorable with the volume in this state —
# nothing has to exist first, and no tenant has to be restored first.
bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --registry "$INSTANCE"
check "the registry is back on a bare volume, with all four rows" "4" \
	"$(sqlite3 "file:$(registry_file)?mode=ro" "SELECT count(*) FROM user;" 2>/dev/null || echo '<no db>')"
# Three live users and one released handle. The states came back with the rows —
# a restore that flattened them would resurrect a detached user silently.
check "and their states survived the restore" "3 1" \
	"$(sqlite3 "file:$(registry_file)?mode=ro" \
		"SELECT (SELECT count(*) FROM user WHERE state='active') || ' ' ||
		        (SELECT count(*) FROM user WHERE state='detached');" 2>/dev/null || echo '<no db>')"

# STEP TWO: the tenant list comes OUT of the registry, handles and all. This is
# the enumeration the runbook tells the operator to use, and the only one that
# survives opaque ids.
mapfile -t DISCOVERED < <(sqlite3 "file:$(registry_file)?mode=ro" \
	"SELECT handle || ' ' || database_id || ' ' || state FROM user ORDER BY handle;")
echo "  the registry's answer:"
printf '    %s\n' "${DISCOVERED[@]-}"
# The renamed user comes back as ${RENAMED_TO}, not as ${RENAMED}: the registry
# is restored from the replica of the table as it stood, so the CURRENT handle is
# what a total loss hands back.
check "every tenant is reachable BY HANDLE again, under its current handle" \
	"$(printf '%s\n' "${HANDLE[@]}" | sort | tr '\n' ' ' | sed 's/ $//')" \
	"$(printf '%s\n' "${DISCOVERED[@]-}" | awk '{printf "%s ", $1}' | sed 's/ $//')"
# And the registry's answer and the bucket's agree — neither is trusted alone.
# ALL rows, not just the active ones: a prefix the registry cannot account for is
# an orphan, and a detached user's kept database must not read as one.
check "the ids it names are exactly the prefixes in the bucket" "same" \
	"$(cmp -s <(printf '%s\n' "${DISCOVERED[@]-}" | awk '{print $2}' | sort) \
	          <(printf '%s\n' "${PREFIXES[@]-}" | sort) && echo same || echo different)"

# STEP THREE: restore each database the registry named.
for row in "${DISCOVERED[@]}"; do
	bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$(printf '%s' "$row" | awk '{print $2}')" >/dev/null
done
check "all four restored onto a bare volume" "2 2 2 3" \
	"$(for t in "${TENANTS[@]}"; do printf '%s ' "$(rows_count "$t")"; done | sed 's/ $//')"
# The closing claim of the whole scenario: handle -> database_id -> that file,
# and the collection inside it is that person's. Resolved through the restored
# registry, not through a filename — which is the only way it can be resolved
# for the renamed user, whose rows still say what she was called in §2.
resolved=0
for t in "${TENANTS[@]}"; do
	id="$(registry_file_id "${HANDLE[$t]}")"
	owner="$(sqlite3 "file:${MP}/tenants/${id}.sqlite?mode=ro" \
		"SELECT DISTINCT substr(printing_id, 1, instr(printing_id,'-')-1) FROM collection;" 2>/dev/null || echo '<no db>')"
	[ "$owner" = "$t" ] && resolved=$((resolved + 1)) \
		|| echo "  ${HANDLE[$t]} resolved to tenants/${id}.sqlite, which holds '${owner}' (expected ${t})"
done
check "each handle resolves to a database holding THAT person's collection" "4" "$resolved"
check "the catalog was NOT restored (it is not backed up — pkdump setup rebuilds it)" "absent" \
	"$([ -e "${MP}/shared.sqlite" ] && echo present || echo absent)"

# ── 10. THE LOAD-BEARING NEGATIVE ───────────────────────────────────────────
log "10. restoring the tenant files WITHOUT the registry must FAIL"
# §9 restores in the documented order and passes. That is not the same statement
# as "the wrong order fails" — and until this section existed, the wrong order
# SUCCEEDED. Every byte came back, `integrity_check` said ok, and the operator
# was holding a directory of files called 01K2C7HQ8N….sqlite with nobody's name
# on any of them. It reads as a finished recovery. That is the exact shape of the
# failure this project is scar tissue from: every backup-shaped signal green
# while recovery was impossible (pd-1717).
#
# So this section does it in the wrong order on purpose, shows what that gets
# you, and then shows the gate refusing it.
sidecar_stop
rm -rf "${MP}/tenants" "$(registry_file)" "$(registry_file)-wal" "$(registry_file)-shm"
mkdir -p "${MP}/tenants"
check "the volume is bare again, registry included" "0" \
	"$(find "${MP}" -name '*.sqlite' | grep -c . || true)"

# ── the failure, executed. Restore every prefix the bucket offers, straight onto
# the volume, with no registry — which is what "restore the tenant databases"
# means to an operator who has the bucket and has not read the runbook.
for id in "${PREFIXES[@]}"; do
	ls_cli restore -integrity-check full -o "/out/raw-${id}.sqlite" \
		"$(tenant_replica_url "$id")" >/dev/null 2>&1 || true
	cp "${WORK}/out/raw-${id}.sqlite" "${MP}/tenants/${id}.sqlite"
done
healthy=0
for id in "${PREFIXES[@]}"; do
	[ "$(sqlite3 "${MP}/tenants/${id}.sqlite" 'PRAGMA integrity_check;' 2>&1 | head -1)" = ok ] \
		&& healthy=$((healthy + 1))
done
check "all four databases came back COMPLETE and HEALTHY without the registry" "4" "$healthy"
echo "  and this is what the operator is holding:"
find "${MP}/tenants" -name '*.sqlite' -printf '    tenants/%f\n' | sort
# The product's own answer, which is the honest one: it can see four databases
# and cannot name a single one of them.
PL_OUT="$(pk tenant list 2>&1)"
printf '%s\n' "$PL_OUT" | sed 's/^/    /'
check "the box cannot say whose any of them are" "1" \
	"$(printf '%s\n' "$PL_OUT" | grep -c '^No users registered' || true)"
check "...it can only say that nobody claims them" "1" \
	"$(printf '%s\n' "$PL_OUT" | grep -c '^4 database(s) .* that no registered user claims' || true)"
# Asking that question CREATED a registry — an empty one; the accessor opens the
# file lazily and opening it makes it. Delete it again, so the gate below meets
# the real disaster state (no registry at all) rather than a file this line made.
rm -f "$(registry_file)" "$(registry_file)-wal" "$(registry_file)-shm"

# ── the gate. The shipped script refuses, and refuses BEFORE it stops anything:
# a guard that costs an outage to trip is a guard operators route around.
sidecar_start
GATE_OUT="$(bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$(db_id "$VICTIM")" 2>&1)" \
	&& GATE_RC=0 || GATE_RC=$?
printf '%s\n' "$GATE_OUT" | sed 's/^/    /'
check "the shipped restore REFUSES a database the registry cannot name" "1" "$GATE_RC"
check "and names the thing to do first" "1" \
	"$(printf '%s\n' "$GATE_OUT" | grep -c -- '--registry' || true)"
check "and it refused without stopping the sidecar" "active" \
	"$(systemctl --user is-active "${SIDECAR}.service" || true)"

# ── the gate opens for the documented order. Same command, registry first.
bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --registry "$INSTANCE" >/dev/null
GATE_OUT="$(bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$(db_id "$VICTIM")" 2>&1)" \
	&& GATE_RC=0 || GATE_RC=$?
check "with the registry restored FIRST, the same command succeeds" "0" "$GATE_RC"
check "and says whose collection it just recovered" "1" \
	"$(printf '%s\n' "$GATE_OUT" | grep -c "Attribution: $(db_id "$VICTIM") belongs to ${VICTIM} (active)" || true)"

# ── and the one case that is legitimately on the far side of the gate: a PURGED
# database, whose row is gone by design (RESTORE.md scenario B2). It must not be
# unrecoverable — it must be explicit. ${DETACHED} is detached, so she can be
# purged, which is exactly the state pd-pm7b's retention window is a safety net
# for.
sidecar_stop
pk tenant purge "$DETACHED_ID" --yes | sed 's/^/    /'
check "purge destroyed the local database and the row" "gone" \
	"$([ -e "$(tenant_file "$DETACHED")" ] && echo present || echo gone)"
check "and the registry no longer names it" "" "$(registry_state "$DETACHED_ID")"
GATE_OUT="$(bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$DETACHED_ID" 2>&1)" \
	&& GATE_RC=0 || GATE_RC=$?
check "a purged database is refused by default, like any other orphan" "1" "$GATE_RC"
check "and the refusal names --unattributed for exactly this case" "1" \
	"$(printf '%s\n' "$GATE_OUT" | grep -c -- '--unattributed' || true)"
GATE_OUT="$(bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --unattributed "$INSTANCE" "$DETACHED_ID" 2>&1)" \
	&& GATE_RC=0 || GATE_RC=$?
printf '%s\n' "$GATE_OUT" | sed 's/^/    /'
check "and comes back when the operator says so explicitly" "0" "$GATE_RC"
check "with the gate saying out loud that it has no owner" "1" \
	"$(printf '%s\n' "$GATE_OUT" | grep -c 'NOT CHECKED (--unattributed)' || true)"
sidecar_stop
check "the purged collection is genuinely back, and it is hers" "$DETACHED" \
	"$(rows_owner "$DETACHED")"
check "and the box says so: a database no registered user claims" "1" \
	"$(pk tenant list 2>&1 | grep -c "^  ${DETACHED_ID}.sqlite$" || true)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — a collection comes back after a RENAME (same file, same prefix, new"
echo "         handle) and after a DETACH (kept, attributable, restorable); every"
echo "         bystander tenant holds the same data every time; a total loss recovers"
echo "         all four states with the registry FIRST; and restoring the tenant"
echo "         files WITHOUT the registry is REFUSED — after being shown to come"
echo "         back complete, healthy and anonymous, which is what made it worth a"
echo "         gate. Recreated handles: tests/litestream/recreate.sh."
