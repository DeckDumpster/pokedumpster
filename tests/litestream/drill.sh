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
#   A. a tenant's live database is deleted    -> restore-litestream.sh <inst> <tenant>
#   B. a tenant needs rolling back in time    -> restore-litestream.sh --at=<T> ...
#   C. the whole data volume is lost          -> enumerate the tenants FROM S3,
#                                                restore each one
#
# It runs the real scripts (deploy/restore-litestream.sh, deploy/backup-check.sh)
# against a real Quadlet sidecar unit on a real data volume, in place — deleting
# the live file and restoring over it, which is what an incident actually looks
# like. The property under test is ISOLATION: after each restore, every other
# tenant's database is byte-identical, its replica is still current, and it still
# restores to its own data.
#
# Scenario C also settles the question this bead was given: the rejected
# libSQL/sqld path had a DR gap where the namespace registry was not backed up,
# so recovery had to re-declare the namespaces before any data would come back.
# File-per-tenant has no registry to lose — §7 recovers a wiped volume knowing
# only the bucket, listing the tenants out of S3 itself.
#
# Prod-safe by default: its own instance name, its own volume, its own throwaway
# MinIO, its own podman secret, its own config dir. It touches no pkdump-prod
# unit, no pkdump-prod-data volume and no real bucket unless DRILL_REAL_S3=1 —
# and that mode writes only under its own hardcoded pd-v8zf-drill/ prefix, which
# it deletes on the way out.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

LITESTREAM_IMAGE=${LITESTREAM_IMAGE:-docker.io/litestream/litestream:latest}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
AWSCLI_IMAGE=${AWSCLI_IMAGE:-docker.io/amazon/aws-cli:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# The addressing under test is the deploy scripts' own.
# shellcheck source=deploy/litestream-lib.sh
. "${REPO_DIR}/deploy/litestream-lib.sh"

INSTANCE=${DRILL_INSTANCE:-drill}
case "$INSTANCE" in
	prod|"") echo "refusing to drill instance '${INSTANCE}'" >&2; exit 1 ;;
esac
VOLUME="pkdump-${INSTANCE}-data"
SIDECAR="pkdump-litestream-${INSTANCE}"
CONF_DIR="${HOME}/.config/pkdump/${INSTANCE}"
SECRET_NAME="pkdump-${INSTANCE}-s3-bootstrap"
QUADLET_DIR="${HOME}/.config/containers/systemd"

MINIO_CTR="pkdump-${INSTANCE}-minio"
MINIO_PORT=${MINIO_PORT:-39931}

# FOUR tenants, and the victim is #2 — a restore that grabbed the first or the
# last prefix would still look right with one or two.
TENANTS=(alpha bravo charlie delta)
VICTIM=bravo
WRITER=delta   # keeps taking writes while the victim is being recovered

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
sidecar_stop() { systemctl --user stop "${SIDECAR}.service"; sleep 2; }
sidecar_start() {
	systemctl --user reset-failed "${SIDECAR}.service" 2>/dev/null || true
	systemctl --user start "${SIDECAR}.service"
	sleep "${1:-8}"
}

mount_point() { podman volume inspect -f '{{.Mountpoint}}' "$VOLUME"; }
tenant_file() { printf '%s/tenants/%s.sqlite' "$(mount_point)" "$1"; }
file_hash() { sha256sum "$(tenant_file "$1")" 2>/dev/null | cut -d' ' -f1 || echo '<missing>'; }
# The rows, independent of page layout and of Litestream's own bookkeeping
# tables. "Untouched" has to mean the data, not just the bytes.
rows_hash() { sqlite3 "file:$(tenant_file "$1")?mode=ro" 'SELECT id, printing_id, phase FROM collection ORDER BY id;' 2>/dev/null | sha256sum | cut -d' ' -f1; }
rows_count() { sqlite3 "file:$(tenant_file "$1")?mode=ro" 'SELECT count(*) FROM collection;' 2>/dev/null || echo '<no db>'; }
rows_owner() { sqlite3 "file:$(tenant_file "$1")?mode=ro" "SELECT DISTINCT substr(printing_id, 1, instr(printing_id,'-')-1) FROM collection;" 2>/dev/null || echo '<no db>'; }

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
	local url deadline
	url="$(tenant_replica_url "$1")"
	deadline=$(( SECONDS + ${2:-60} ))
	while [ "$SECONDS" -lt "$deadline" ]; do
		if ls_cli ltx -level all "$url" 2>/dev/null | grep -q .; then return 0; fi
		sleep 2
	done
	return 1
}

# ── 1. the S3 target ────────────────────────────────────────────────────────
if [ -n "${DRILL_REAL_S3:-}" ]; then
	SRC="${DRILL_SOURCE_INSTANCE:-prod}"
	log "1. REAL bucket (instance '${SRC}'), throwaway prefix"
	[ -f "${HOME}/.config/pkdump/${SRC}/litestream.env" ] || { echo "no litestream.env for '${SRC}'" >&2; exit 1; }
	set -a; . "${HOME}/.config/pkdump/${SRC}/litestream.env"; set +a
	# The drill writes ONLY under its own prefix, never the instance's.
	DRILL_PREFIX="pd-v8zf-drill/${INSTANCE}"
	export LITESTREAM_S3_PATH="${DRILL_PREFIX}/tenants"
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
echo "  ${#TENANTS[@]} tenants on ${VOLUME}: ${TENANTS[*]}"

# The shipped Quadlet sidecar unit, installed the way deploy/setup.sh installs it.
mkdir -p "$QUADLET_DIR"
sed -e "s|{{INSTANCE}}|${INSTANCE}|g" -e "s|{{REPO_DIR}}|${REPO_DIR}|g" \
	"${REPO_DIR}/deploy/pkdump-litestream.container" > "${QUADLET_DIR}/${SIDECAR}.container"
systemctl --user daemon-reload
sidecar_start 3
check "the shipped sidecar unit is running" "active" \
	"$(systemctl --user is-active "${SIDECAR}.service" || true)"

replicating=0
for t in "${TENANTS[@]}"; do
	wait_for_replica "$t" 90 && replicating=$((replicating + 1)) || echo "  ${t} never reached S3"
done
check "every tenant is replicating from the one sidecar" "4" "$replicating"

# A marker between two write phases, for the point-in-time restore in §5.
sleep 2
MARKER=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 3
write_phase late
sleep 10
check "each tenant has both write phases locally" "2 2 2 2" \
	"$(for t in "${TENANTS[@]}"; do printf '%s ' "$(rows_count "$t")"; done | sed 's/ $//')"

# ── 3. the state the drill will hold the restore against ────────────────────
log "3. record every tenant's state with the sidecar stopped"
# Stopped first so nothing is mid-checkpoint: from here to the post-restore
# measurement, the ONLY thing that runs against this volume is the restore.
sidecar_stop
declare -A BEFORE_FILE BEFORE_ROWS
for t in "${TENANTS[@]}"; do
	BEFORE_FILE[$t]="$(file_hash "$t")"
	BEFORE_ROWS[$t]="$(rows_hash "$t")"
	echo "  ${t}  file ${BEFORE_FILE[$t]:0:12}  rows ${BEFORE_ROWS[$t]:0:12}  ($(rows_count "$t") rows)"
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

bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$VICTIM"

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
	[ "$(file_hash "$t")" = "${BEFORE_FILE[$t]}" ] || { echo "  FAIL  ${t}'s file changed"; fail=$((fail + 1)); continue; }
	[ "$(rows_hash "$t")" = "${BEFORE_ROWS[$t]}" ] || { echo "  FAIL  ${t}'s rows changed"; fail=$((fail + 1)); continue; }
	untouched=$((untouched + 1))
done
check "every bystander tenant is BYTE-IDENTICAL across the restore" "2" "$untouched"
check "and ${WRITER}'s own write during the restore is still there" "3" "$(rows_count "$WRITER")"

sidecar_start
# The neighbours' replicas must also survive: restoring one tenant neither
# consumes nor invalidates another's stream.
ls_cli restore -integrity-check full -o /out/charlie-after.sqlite "$(tenant_replica_url charlie)" >/dev/null 2>&1 || true
check "an untouched tenant still restores to current from its own replica" "2" \
	"$(sqlite3 "${WORK}/out/charlie-after.sqlite" 'SELECT count(*) FROM collection;' 2>/dev/null || echo '<no db>')"
check "and it is charlie's data, not ${VICTIM}'s" "charlie" \
	"$(sqlite3 "${WORK}/out/charlie-after.sqlite" "SELECT DISTINCT substr(printing_id,1,instr(printing_id,'-')-1) FROM collection;" 2>/dev/null || echo '<no db>')"
# The write taken while the sidecar was down is not lost: it is replicated when
# the sidecar resumes. This is the whole cost of one sidecar for N tenants.
ls_cli restore -integrity-check full -o /out/writer-after.sqlite "$(tenant_replica_url "$WRITER")" >/dev/null 2>&1 || true
check "a write made while the sidecar was down reached S3 once it resumed" "3" \
	"$(sqlite3 "${WORK}/out/writer-after.sqlite" 'SELECT count(*) FROM collection;' 2>/dev/null || echo '<no db>')"

# ── 5. RESTORE.md scenario B — roll ONE tenant back in time ─────────────────
log "5. scenario B: point-in-time restore of ${VICTIM} to ${MARKER}"
sidecar_stop
# The last moment at which ${VICTIM}'s replica still holds the CURRENT state —
# used below to prove the rollback itself is undoable.
PRE_ROLLBACK=$(date -u +%Y-%m-%dT%H:%M:%SZ)
for t in "${TENANTS[@]}"; do BEFORE_FILE[$t]="$(file_hash "$t")"; BEFORE_ROWS[$t]="$(rows_hash "$t")"; done

bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --at="$MARKER" "$INSTANCE" "$VICTIM"

sidecar_stop
check "${VICTIM} rolled back to the marker (early phase only)" "1" "$(rows_count "$VICTIM")"
check "and it is still ${VICTIM}'s data" "$VICTIM" "$(rows_owner "$VICTIM")"
untouched=0
for t in "${TENANTS[@]}"; do
	[ "$t" = "$VICTIM" ] && continue
	[ "$(file_hash "$t")" = "${BEFORE_FILE[$t]}" ] && [ "$(rows_hash "$t")" = "${BEFORE_ROWS[$t]}" ] \
		&& untouched=$((untouched + 1)) || echo "  FAIL  ${t} changed during a point-in-time restore of ${VICTIM}"
done
check "a point-in-time rollback of one tenant leaves the others at CURRENT" "3" "$untouched"

# A rollback is not a one-way door, and the runbook says so — but only because
# this is checked. Once the sidecar comes back it replicates the ROLLED-BACK
# database, so from that moment "latest" IS the rollback: recovering the state
# the operator just discarded means asking for a timestamp from before it. The
# replica is append-only (the rollback lands as a new txid, it does not rewrite
# history), so that state is still there.
sidecar_start
sidecar_stop
bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$VICTIM"
sidecar_stop
check "after the rollback replicates, 'latest' is the rolled-back state" "1" "$(rows_count "$VICTIM")"

bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes --at="$PRE_ROLLBACK" "$INSTANCE" "$VICTIM"
sidecar_stop
check "and the discarded state comes back from just before the rollback" "2" \
	"$(rows_count "$VICTIM")"
check "still ${VICTIM}'s own data" "$VICTIM" "$(rows_owner "$VICTIM")"
sidecar_start

# ── 6. the freshness checker agrees, per tenant ─────────────────────────────
log "6. deploy/backup-check.sh: every tenant, including the restored one"
# The dead-man's switch is what tells the operator backups RESUMED after a
# restore. Point its ping at a closed port: a failed ping only warns, so this
# exercises the real S3 freshness query without needing a monitor.
BC_OUT="$(PKDUMP_BACKUP_PING_URL=http://127.0.0.1:1/drill \
	bash "${REPO_DIR}/deploy/backup-check.sh" "$INSTANCE" 2>&1)" && BC_RC=0 || BC_RC=$?
printf '%s\n' "$BC_OUT" | sed 's/^/    /'
check "backup-check reports every tenant fresh after the restore" "0" "$BC_RC"
check "and it checked all four" "4" "$(printf '%s\n' "$BC_OUT" | grep -c "OK — newest S3 replica" || true)"

# ── 7. RESTORE.md scenario C — the whole volume is gone ─────────────────────
log "7. scenario C: total loss — the bucket IS the tenant registry"
# The question this section settles: after losing the box, how does the operator
# know WHICH tenants to restore? The rejected libSQL path needed a namespace
# registry that bottomless did not back up. Here the tenant list is derivable
# from the replica prefixes alone, because directory mode names each prefix after
# the file. Nothing local is consulted — the volume is wiped first.
sidecar_stop
rm -rf "${MP}/tenants" "${MP}/shared.sqlite"
check "the data volume is empty" "0" "$(find "${MP}" -name '*.sqlite' | grep -c . || true)"

mapfile -t DISCOVERED < <(aws_cli s3 ls "s3://${LITESTREAM_S3_BUCKET}/${LITESTREAM_S3_PATH}/" 2>/dev/null \
	| awk '/PRE/ {print $2}' | sed 's|\.sqlite/$||' | sort)
check "every tenant is discoverable from S3 alone" "${TENANTS[*]}" "${DISCOVERED[*]-}"

for t in "${DISCOVERED[@]}"; do
	bash "${REPO_DIR}/deploy/restore-litestream.sh" --yes "$INSTANCE" "$t" >/dev/null
done
check "all four restored onto a bare volume" "2 2 2 3" \
	"$(for t in "${TENANTS[@]}"; do printf '%s ' "$(rows_count "$t")"; done | sed 's/ $//')"
check "and each one is itself" "alpha bravo charlie delta" \
	"$(for t in "${TENANTS[@]}"; do printf '%s ' "$(rows_owner "$t")"; done | sed 's/ $//')"
check "the catalog was NOT restored (it is not backed up — pkdump setup rebuilds it)" "absent" \
	"$([ -e "${MP}/shared.sqlite" ] && echo present || echo absent)"

log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — one tenant restores in place, in time, and from nothing, and the"
echo "         other tenants are byte-identical every time."
