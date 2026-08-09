#!/usr/bin/env bash
# Container-tier gate (pd-lhih): EXECUTABLE PROOF that pd-pm7b is closed —
# a recreated handle cannot inherit its predecessor's replica.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/litestream/recreate.sh          # ~2min, throwaway MinIO, full teardown
#   KEEP=1 bash tests/litestream/recreate.sh   # leave MinIO + WORK up for poking
#   RECREATE_REAL_S3=1 bash tests/litestream/recreate.sh
#                                              # against the REAL bucket of instance
#                                              #   $RECREATE_SOURCE_INSTANCE (default
#                                              #   prod) under a throwaway prefix.
#                                              #   Needs creds + network; NOT in ci.sh.
#
# ── THE FAILURE THIS EXISTS TO CLOSE ────────────────────────────────────────
# pd-pm7b, in four steps:
#
#   1. create alice; write a recognisable card into her collection.
#   2. remove alice.
#   3. create alice again.
#   4. Under the OLD design her database was `tenants/alice.sqlite` BOTH times,
#      and Litestream derives the replica prefix from the filename — so new
#      alice sat under a stream still holding OLD alice's data for the whole
#      180-day retention window, and a point-in-time restore inside that window
#      handed it back, with PRAGMA integrity_check = ok.
#
# The fix is not a check that refuses that restore. It is that new alice's
# database_id — minted by the registry, never derived from the handle — is a
# DIFFERENT opaque id, so her replica prefix is a different prefix, so there is
# no such restore to refuse.
#
# ── WHAT IS ASSERTED ────────────────────────────────────────────────────────
# §5  the two database_ids differ                       (necessary)
# §5  the two replica prefixes differ                   (necessary)
# §6  no restore of new alice — latest, or point-in-time INSIDE the window at a
#     moment when old alice's card was live — produces that card    (THE PROOF)
# §6  and old alice's card is still sitting in the bucket, restorable by id and
#     healthy — so §6's zeroes are an absence, not an expiry        (non-vacuity)
# §7  the old design, executed beside it: a handle-derived address is unchanged
#     by recreation, and restoring it inside the window DOES hand back the
#     deleted user's card                                    (the control arm)
#
# §7 is the reason the rest is evidence rather than assertion. Without it, "we
# restored and found nothing" is equally consistent with a test that never wrote
# anything worth finding.
#
# The whole run goes through the SHIPPED pieces: `pkdump tenant create/detach/
# purge` mint and retire the ids, deploy/litestream.yml replicates them, and
# deploy/litestream-lib.sh derives every URL that is restored. A test that
# re-derived the addressing itself would agree with a broken restore script.
#
# Prod-safe: its own podman network, its own MinIO, its own temp dir, its own
# $PKDUMP_HOME. Touches no pkdump-* unit, no pkdump-*-data volume and no real
# bucket unless RECREATE_REAL_S3=1 — and that mode writes only under its own
# pd-lhih-recreate/ prefix, which it deletes on the way out.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

# Bare `sqlite3` has NO busy timeout and returns SQLITE_BUSY the moment the
# replicator holds the lock; under `set -e` that takes the whole run down. This
# gate writes to databases while a Litestream container replicates them, so
# contention is the scenario, not an edge case. Same 5s as everywhere else.
sq() { sqlite3 -cmd '.timeout 5000' "$@"; }

LITESTREAM_IMAGE=${LITESTREAM_IMAGE:-docker.io/litestream/litestream:latest}
MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
AWSCLI_IMAGE=${AWSCLI_IMAGE:-docker.io/amazon/aws-cli:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SHIPPED_YML="${REPO_DIR}/deploy/litestream.yml"

# The addressing under test is the deploy scripts' own, sourced from the file
# they source.
# shellcheck source=deploy/litestream-lib.sh
. "${REPO_DIR}/deploy/litestream-lib.sh"

# Host ports, from the kernel rather than picked (pd-r0ri). One definition for
# every harness; see tests/lib/ports.sh for what a picked one costs.
# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"

# PER-CHECKOUT, for the reason deploy/ci.sh derives its container instance the
# same way: the swarm runs several polecats per rig, each from its own worktree,
# and every one of them runs deploy/ci.sh — which runs this. With fixed names,
# run B's opening `podman rm -f` destroys run A's MinIO mid-suite and A fails
# somewhere that looks unrelated.
SUFFIX="${PDRC_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
NET=pdrc-net-$SUFFIX
MINIO_CTR=pdrc-minio-$SUFFIX
LS_CTR=pdrc-litestream-$SUFFIX
LEG_CTR=pdrc-legacy-$SUFFIX
# Published on the host, so it comes from the kernel and not from the suffix.
# It used to be a hash of the checkout modulo a 400-wide band, which is what
# `rootlessport listen tcp 127.0.0.1:40090: bind: address already in use` was —
# a deterministic number this gate had no second chance at (pd-r0ri).
MINIO_PORT=${MINIO_PORT:-$(free_port)}

WORK=${WORK:-$(mktemp -d /tmp/pdrc.XXXXXX)}
mkdir -p "$WORK/data" "$WORK/legacy/tenants" "$WORK/minio" "$WORK/restore"

# The recognisable card. Unique per run so a stale object in a shared bucket
# (real-S3 mode) can never be mistaken for this run's evidence.
MARKER="pd-lhih-old-alice-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
NEW_CARD="pd-lhih-new-alice-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
LEG_MARKER="pd-lhih-old-legacy-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
# Written AFTER each point-in-time marker, so the marker falls strictly inside a
# replicated range and `-timestamp` resolves to a real moment rather than to the
# end of the stream. A PITR that lands on "latest" by accident proves nothing
# about the window — and Litestream says `timestamp does not exist` for an
# instant past the last segment, which would make the arm below untestable.
LATE_CARD="pd-lhih-later-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"

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
differ() { # differ <label> <a> <b> — the assertion is that they are NOT equal
	if [[ "$2" != "$3" ]]; then
		echo "  PASS  $1"
		echo "          old: $2"
		echo "          new: $3"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 — both are $2"
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	local rc=$?
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving containers up and WORK=$WORK in place."
		echo "  podman rm -f --ignore $LS_CTR $LEG_CTR $MINIO_CTR"
		echo "  podman network rm -f $NET; rm -rf $WORK"
		return $rc
	fi
	# Real-bucket mode leaves objects behind; delete them while the credentials
	# are still wired up.
	if [[ -n "${RECREATE_REAL_S3:-}" && -n "${RC_PREFIX:-}" ]]; then
		echo "==> removing the throwaway prefix s3://${LITESTREAM_S3_BUCKET}/${RC_PREFIX}"
		aws_cli s3 rm --recursive "s3://${LITESTREAM_S3_BUCKET}/${RC_PREFIX}" >/dev/null 2>&1 || true
	fi
	# --ignore matters: podman 4.9 given one name it cannot resolve removes
	# NOTHING and still exits 0, which would leave the whole bed running.
	podman rm -f --ignore "$LS_CTR" "$LEG_CTR" "$MINIO_CTR" >/dev/null 2>&1 || true
	podman network rm -f "$NET" >/dev/null 2>&1 || true
	rm -rf "$WORK"
	return $rc
}
trap cleanup EXIT

# ── 1. the S3 target ────────────────────────────────────────────────────────
# The bead asks for real S3 if credentials are available and for a plain
# statement if not. Default is a throwaway MinIO — deploy/ci.sh runs offline and
# on boxes with no bucket — and the RESULT block below says so in as many words
# rather than letting "the backup gate passed" imply otherwise.
if [ -n "${RECREATE_REAL_S3:-}" ]; then
	SRC="${RECREATE_SOURCE_INSTANCE:-prod}"
	SRC_CONF="${HOME}/.config/pkdump/${SRC}"
	log "1. REAL bucket (instance '${SRC}'), throwaway prefix"
	[ -f "${SRC_CONF}/litestream.env" ] || { echo "no litestream.env for '${SRC}'" >&2; exit 1; }
	set -a; . "${SRC_CONF}/litestream.env"; set +a
	RC_PREFIX="pd-lhih-recreate/${SUFFIX}"
	export LITESTREAM_S3_PATH="${RC_PREFIX}/tenants"
	export LITESTREAM_S3_REGISTRY_PATH="${RC_PREFIX}/registry.sqlite"
	export LITESTREAM_S3_ENDPOINT=""
	S3_TARGET="REAL S3 — s3://${LITESTREAM_S3_BUCKET}/${RC_PREFIX} (instance ${SRC})"
	# Real S3 is reached over the default network, with the instance's own
	# assume-role profile and bootstrap secret (never a static key on disk).
	PODMAN_NET=()
	CRED_ARGS=(
		-v "${SRC_CONF}/aws/config:/aws/config:ro"
		--secret "pkdump-${SRC}-s3-bootstrap,type=mount,target=/aws/credentials"
		-e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials
		-e AWS_PROFILE=pkdump
	)
else
	log "1. throwaway MinIO"
	AKID=pdrctestroot
	SECRET=pdrctestsecret123
	RC_PREFIX="rctest"
	export LITESTREAM_S3_BUCKET=pdrc-test
	export LITESTREAM_S3_REGION=us-west-2
	export LITESTREAM_S3_PATH="${RC_PREFIX}/tenants"
	export LITESTREAM_S3_REGISTRY_PATH="${RC_PREFIX}/registry.sqlite"
	export LITESTREAM_S3_ENDPOINT="http://${MINIO_CTR}:9000"
	S3_TARGET="throwaway MinIO on 127.0.0.1:${MINIO_PORT} — NOT real S3"

	podman rm -f --ignore "$LS_CTR" "$LEG_CTR" "$MINIO_CTR" >/dev/null 2>&1 || true
	podman network rm -f "$NET" >/dev/null 2>&1 || true
	podman network create "$NET" >/dev/null
	podman run -d --name "$MINIO_CTR" --network "$NET" \
		-p "127.0.0.1:${MINIO_PORT}:9000" \
		-e MINIO_ROOT_USER="$AKID" -e MINIO_ROOT_PASSWORD="$SECRET" \
		-v "$WORK/minio:/data:Z" \
		"$MINIO_IMAGE" server /data >/dev/null
	for _ in $(seq 60); do
		curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null 2>&1 && break
		sleep 1
	done
	curl -fsS "http://127.0.0.1:${MINIO_PORT}/minio/health/live" >/dev/null
	mkdir -p "$WORK/aws"
	printf '[pkdump]\naws_access_key_id=%s\naws_secret_access_key=%s\n' "$AKID" "$SECRET" >"$WORK/aws/credentials"
	# MinIO auto-creates nothing; the bucket is made by the same litestream
	# image that will write to it, via the AWS CLI below.
	PODMAN_NET=(--network "$NET")
	CRED_ARGS=(
		-v "$WORK/aws:/aws:ro,Z"
		-e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump
	)
	echo "  minio up on 127.0.0.1:${MINIO_PORT}"
fi
export LITESTREAM_TENANTS_DIR=/data/tenants
export LITESTREAM_REGISTRY_DB=/data/registry.sqlite

# aws_cli / ls_run — the AWS CLI and litestream, wearing whichever credentials
# this run's target needs. Everything below addresses S3 through these two.
aws_cli() {
	local endpoint=()
	[ -n "${LITESTREAM_S3_ENDPOINT:-}" ] && endpoint=(--endpoint-url "$LITESTREAM_S3_ENDPOINT")
	podman run --rm "${PODMAN_NET[@]}" "${CRED_ARGS[@]}" \
		-e AWS_REGION="${LITESTREAM_S3_REGION}" \
		"$AWSCLI_IMAGE" "$@" "${endpoint[@]}"
}
ls_run() {
	podman run --rm "${PODMAN_NET[@]}" --user 0 \
		-v "$WORK/restore:/restore:Z" "${CRED_ARGS[@]}" \
		"$LITESTREAM_IMAGE" "$@"
}
[ -n "${RECREATE_REAL_S3:-}" ] || aws_cli s3 mb "s3://${LITESTREAM_S3_BUCKET}" >/dev/null 2>&1 || true
echo "  target: ${S3_TARGET}"

# replica_has_objects <url> — does anything at all live under that prefix?
# Asked through litestream rather than through a bucket listing so it means the
# same thing in both modes.
replica_has_objects() { ls_run ltx -level all "$1" 2>/dev/null | grep -q .; }

wait_for_replica() { # wait_for_replica <url> [seconds]
	local deadline=$(( SECONDS + ${2:-90} ))
	while [ "$SECONDS" -lt "$deadline" ]; do
		replica_has_objects "$1" && return 0
		sleep 2
	done
	return 1
}

restore_to() { # restore_to <out> <url> [flags...] — never aborts the report
	local out="$1" url="$2"; shift 2
	rm -f "${WORK}/restore/${out}"
	ls_run restore "$@" -o "/restore/${out}" "$url" >"${WORK}/restore/${out}.log" 2>&1 || true
}
# How many rows of a given card the restored database holds. A restore that
# produced no file at all answers 0 — which is a PASS for the proof below and a
# FAIL for the non-vacuity check, so the two together still tell them apart.
restored_card_count() { # restored_card_count <file> <printing_id>
	sqlite3 "${WORK}/restore/$1" \
		"SELECT count(*) FROM collection WHERE printing_id = '$2';" 2>/dev/null || echo 0
}
restored_state() { # restored_state <file> — for the diagnostic line
	[ -s "${WORK}/restore/$1" ] || { echo "<no database restored>"; return; }
	sqlite3 "${WORK}/restore/$1" 'PRAGMA integrity_check;' 2>&1 | head -1
}
# Poll the REPLICA for a card rather than sleeping a guessed interval: the claim
# is that the row reaches S3, not that it reaches S3 within N seconds on whatever
# machine deploy/ci.sh is running on today.
wait_for_card() { # wait_for_card <out> <url> <printing_id> [tries]
	local i
	for i in $(seq "${4:-45}"); do
		restore_to "$1" "$2"
		[ "$(restored_card_count "$1" "$3")" = 1 ] && return 0
		sleep 2
	done
	return 1
}
add_card() { # add_card <db file> <printing_id> <note>
	sq "$1" "INSERT INTO collection (printing_id, acquired_at, source, notes)
	         VALUES ('$2', datetime('now'), 'pd-lhih', '$3');" >/dev/null
}

# The real CLI, against a throwaway $PKDUMP_HOME. Minting ids is the mechanism
# under test, so it has to be the product's minting and not a fixture's.
PKDUMP_BIN=${PKDUMP_BIN:-${CARGO_TARGET_DIR:-${REPO_DIR}/target}/debug/pkdump}
if [ ! -x "$PKDUMP_BIN" ]; then
	echo "  building pkdump (${PKDUMP_BIN} not present)..."
	( cd "$REPO_DIR" && cargo build --quiet --bin pkdump )
fi
pk() { PKDUMP_HOME="$WORK/data" "$PKDUMP_BIN" "$@"; }
# `Created user <handle> -> database <ID> at <path>`
created_id() { awk '/^Created user /{print $6; exit}'; }
# The registry's own answer to "where does this handle live now?" — the ACTIVE
# row, because a handle is unique only among live users: every alice who has
# ever been detached keeps her row, and keeps her real name on it.
registry_id() { registry_id_in_state "$1" active; }
# ...and the same question asked of the rows that were released.
detached_ids() { registry_id_in_state "$1" detached; }
registry_id_in_state() {
	pk tenant list | awk -v h="$1" -v s="$2" '$1 == h && $4 == s {print $2}'
}
tenant_file() { printf '%s/data/tenants/%s.sqlite' "$WORK" "$1"; }

# ── 2. create alice, and write the recognisable card ────────────────────────
log "2. create alice; write a recognisable card into her collection"
ID_OLD="$(pk tenant create alice | created_id)"
check "the registry minted a well-formed database id" "ok" \
	"$(validate_database_id "$ID_OLD" >/dev/null 2>&1 && echo ok || echo rejected)"
check "her file is named by that id, not by her handle" "yes" \
	"$([ -f "$(tenant_file "$ID_OLD")" ] && echo yes || echo no)"
check "and nothing on disk is named 'alice'" "0" \
	"$(find "$WORK/data" -name 'alice*' | grep -c . || true)"
add_card "$(tenant_file "$ID_OLD")" "$MARKER" 'old alice'
echo "  alice -> ${ID_OLD}, holding ${MARKER}"

# ── 3. the shipped sidecar replicates her ───────────────────────────────────
log "3. the shipped deploy/litestream.yml replicates her, prefix derived from the id"
podman run -d --name "$LS_CTR" "${PODMAN_NET[@]}" --user 0 \
	-v "$WORK/data:/data:Z" -v "$SHIPPED_YML:/etc/litestream.yml:ro,Z" \
	"${CRED_ARGS[@]}" \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_PATH \
	-e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_REGISTRY_PATH \
	-e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null

URL_OLD="$(tenant_replica_url "$ID_OLD")"
check "old alice reaches S3 under her own derived prefix" "yes" \
	"$(wait_for_replica "$URL_OLD" 120 && echo yes || echo no)"
if ! replica_has_objects "$URL_OLD"; then
	echo "  --- last 20 lines from ${LS_CTR} ---"
	podman logs "$LS_CTR" 2>&1 | tail -20 | sed 's/^/  /'
fi
# Poll for the CARD, not just for the prefix: the assertion in §6 is about this
# row, and a proof that the row was never replicated proves nothing.
wait_for_card old-live.sqlite "$URL_OLD" "$MARKER" || true
check "her card is in the replica, restorable" "1" \
	"$(restored_card_count old-live.sqlite "$MARKER")"

# The moment the old failure reached for: inside the retention window, with old
# alice's card live. Every point-in-time restore below asks for this instant.
sleep 2
T_OLD=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 2
# One more write, so T_OLD is strictly inside old alice's replicated range and
# not merely "the end of it" — see the note on LATE_CARD.
add_card "$(tenant_file "$ID_OLD")" "$LATE_CARD" 'old alice, later'
wait_for_card old-late.sqlite "$URL_OLD" "$LATE_CARD" || true
check "a later write reaches her replica too, so T_OLD is inside the window" "1" \
	"$(restored_card_count old-late.sqlite "$LATE_CARD")"
echo "  marker timestamp inside the window, old alice live: ${T_OLD}"

# ── 4. remove alice — which is now detach, then purge ───────────────────────
log "4. remove alice (detach), then purge: the local bytes go, the replica stays"
pk tenant detach alice --yes >/dev/null
check "the handle is free again" "" "$(registry_id alice)"
# Both at once, which is the partial index doing the work: no live user answers
# to "alice", and the surviving row still says these bytes were alice's. The
# handle is not rewritten to free it, so an orphaned database is attributable
# by reading a column rather than by parsing a composite name.
check "the row survives under her REAL handle, so the bytes stay attributable" "$ID_OLD" \
	"$(detached_ids alice)"
check "detach kept her database" "yes" \
	"$([ -f "$(tenant_file "$ID_OLD")" ] && echo yes || echo no)"

# Purge is the harder case and the one pd-pm7b actually described: the only copy
# of old alice's collection is now the S3 replica, kept until retention expires.
# `pkdump tenant purge` says so itself. That is the window the old design let a
# recreated handle reach into.
pk tenant purge "$ID_OLD" --yes >/dev/null
check "purge destroyed the local database" "no" \
	"$([ -f "$(tenant_file "$ID_OLD")" ] && echo yes || echo no)"
check "and the replica outlives it — the retention window is open" "yes" \
	"$(replica_has_objects "$URL_OLD" && echo yes || echo no)"

# ── 5. create alice again ───────────────────────────────────────────────────
log "5. create alice again — a new id, therefore a new prefix"
ID_NEW="$(pk tenant create alice | created_id)"
URL_NEW="$(tenant_replica_url "$ID_NEW")"
differ "the two database_ids differ" "$ID_OLD" "$ID_NEW"
differ "the two replica prefixes differ" \
	"$(printf '%s' "$URL_OLD" | sed 's/?.*//')" "$(printf '%s' "$URL_NEW" | sed 's/?.*//')"
check "the registry now maps the handle to the NEW id" "$ID_NEW" "$(registry_id alice)"
# The prefix is a function of the id, and the id is a function of nothing the
# caller controls. This is what makes the collision unreachable rather than
# merely unlikely: there is no value of the handle that reproduces it.
check "old alice's prefix is not reachable from the handle any more" "unreachable" \
	"$([ "$(tenant_replica_url "$(registry_id alice)")" = "$URL_OLD" ] && echo reachable || echo unreachable)"

add_card "$(tenant_file "$ID_NEW")" "$NEW_CARD" 'new alice'
check "new alice reaches S3 under her own prefix" "yes" \
	"$(wait_for_replica "$URL_NEW" 120 && echo yes || echo no)"
wait_for_card new-live.sqlite "$URL_NEW" "$NEW_CARD" || true

# A point-in-time marker inside NEW alice's own stream, with a write after it.
# §6 restores at this instant as well as at T_OLD: a PITR that fails for want of
# a moment is a weaker statement than a PITR that succeeds, hands back her
# collection as it stood, and still contains nothing of her predecessor's.
sleep 2
T_NEW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 2
add_card "$(tenant_file "$ID_NEW")" "$LATE_CARD" 'new alice, later'
wait_for_card new-late.sqlite "$URL_NEW" "$LATE_CARD" || true
check "and a later write of hers reaches it, so T_NEW is inside her window" "1" \
	"$(restored_card_count new-late.sqlite "$LATE_CARD")"

# ── 6. THE PROOF ────────────────────────────────────────────────────────────
log "6. THE PROOF: no restore of new alice can produce old alice's card"
restore_to new-latest.sqlite "$URL_NEW" -integrity-check full
check "restoring new alice returns HER card" "1" \
	"$(restored_card_count new-latest.sqlite "$NEW_CARD")"
check "restoring new alice does NOT return old alice's card" "0" \
	"$(restored_card_count new-latest.sqlite "$MARKER")"

# A point-in-time restore of new alice that SUCCEEDS. It rolls her back to T_NEW
# — her later card is gone, her first card is there — so the window demonstrably
# works on this stream. It still contains nothing of her predecessor's.
restore_to new-pit-own.sqlite "$URL_NEW" -timestamp "$T_NEW" -integrity-check full
check "a working point-in-time restore of new alice rolls HER back" "1 0" \
	"$(restored_card_count new-pit-own.sqlite "$NEW_CARD") $(restored_card_count new-pit-own.sqlite "$LATE_CARD")"
check "and that restore still holds no card of old alice's" "0" \
	"$(restored_card_count new-pit-own.sqlite "$MARKER")"

# The exact move the old design rewarded: ask new alice's replica for the moment
# old alice's card was live. There is nothing to find, because that moment is not
# in this stream at all — Litestream refuses the timestamp outright rather than
# handing over a predecessor's rows.
restore_to new-pit-old.sqlite "$URL_NEW" -timestamp "$T_OLD"
check "asking her replica for the instant old alice was live returns no card" "0" \
	"$(restored_card_count new-pit-old.sqlite "$MARKER")"
echo "  (litestream's answer at ${T_OLD}: $(restored_state new-pit-old.sqlite))"
[ -s "${WORK}/restore/new-pit-old.sqlite" ] || \
	tail -3 "${WORK}/restore/new-pit-old.sqlite.log" 2>/dev/null | sed 's/^/    /'

# NON-VACUITY. Old alice's card is still there — restorable, complete, healthy,
# and reachable at the very instant §6 asked for — addressed by the id that names
# it. So the zeroes above are the absence of a path from the handle to those
# bytes, not the absence of the bytes and not a point-in-time window that never
# worked in the first place.
restore_to old-by-id.sqlite "$URL_OLD" -integrity-check full
check "old alice's card IS still in the bucket, restorable by database id" "1" \
	"$(restored_card_count old-by-id.sqlite "$MARKER")"
check "and it comes back healthy — the retention window really is open" "ok" \
	"$(restored_state old-by-id.sqlite)"
restore_to old-by-id-pit.sqlite "$URL_OLD" -timestamp "$T_OLD" -integrity-check full
check "T_OLD is a real moment: her own stream rolls back to it and holds the card" "1 0" \
	"$(restored_card_count old-by-id-pit.sqlite "$MARKER") $(restored_card_count old-by-id-pit.sqlite "$LATE_CARD")"

# ── 7. the control arm: the old design, executed ────────────────────────────
log "7. CONTROL: the same procedure under handle-derived naming DOES leak"
# Same bucket, same shipped config, one difference: the database is named by the
# handle, the way every database was before this epic (and the way
# tenants/collection.sqlite still is on the prod box — which is why
# validate_db_stem accepts the shape).
#
# The sidecar is stopped for the swap deliberately. What this arm measures is the
# ADDRESS: under handle-derived naming, the URL a recreated user's restore
# resolves to is the same string as the URL her predecessor replicated to, so the
# predecessor's data is what comes back. Whatever Litestream then does when two
# different databases write to one prefix is a second failure, not a mitigation,
# and holding it out keeps this arm measuring one thing.
export LITESTREAM_S3_PATH="${RC_PREFIX}/legacy"
export LITESTREAM_REGISTRY_DB=/data/registry.sqlite
export LITESTREAM_S3_REGISTRY_PATH="${RC_PREFIX}/legacy-registry.sqlite"
LEGACY_DB="$WORK/legacy/tenants/legacy.sqlite"
new_legacy_db() {
	sq "$LEGACY_DB" 'PRAGMA journal_mode=WAL;' >/dev/null
	sq "$LEGACY_DB" <"${REPO_DIR}/crates/pkdump-db/src/schema_user.sql" >/dev/null
}
new_legacy_db
add_card "$LEGACY_DB" "$LEG_MARKER" 'old legacy'

podman run -d --name "$LEG_CTR" "${PODMAN_NET[@]}" --user 0 \
	-v "$WORK/legacy:/data:Z" -v "$SHIPPED_YML:/etc/litestream.yml:ro,Z" \
	"${CRED_ARGS[@]}" \
	-e LITESTREAM_TENANTS_DIR -e LITESTREAM_S3_BUCKET -e LITESTREAM_S3_PATH \
	-e LITESTREAM_REGISTRY_DB -e LITESTREAM_S3_REGISTRY_PATH \
	-e LITESTREAM_S3_REGION -e LITESTREAM_S3_ENDPOINT \
	"$LITESTREAM_IMAGE" replicate -config /etc/litestream.yml >/dev/null

URL_LEG_BEFORE="$(tenant_replica_url legacy)"
wait_for_replica "$URL_LEG_BEFORE" 120 || true
wait_for_card legacy-live.sqlite "$URL_LEG_BEFORE" "$LEG_MARKER" || true
check "the first legacy user's card reached her handle-derived prefix" "1" \
	"$(restored_card_count legacy-live.sqlite "$LEG_MARKER")"
sleep 2
T_LEG=$(date -u +%Y-%m-%dT%H:%M:%SZ)
sleep 2
add_card "$LEGACY_DB" "$LATE_CARD" 'old legacy, later'
wait_for_card legacy-late.sqlite "$URL_LEG_BEFORE" "$LATE_CARD" || true
check "and a later write, so T_LEG is inside her window too" "1" \
	"$(restored_card_count legacy-late.sqlite "$LATE_CARD")"

# The old `tenant remove`: unlink the file and its Litestream bookkeeping. Then
# create the handle again — which, under handle-derived naming, is the same path.
podman rm -f --ignore "$LEG_CTR" >/dev/null 2>&1 || true
rm -f "$LEGACY_DB" "$LEGACY_DB-wal" "$LEGACY_DB-shm"
rm -rf "$WORK/legacy/tenants/.legacy.sqlite-litestream"
new_legacy_db
URL_LEG_AFTER="$(tenant_replica_url legacy)"
check "the recreated legacy user's database is empty on disk" "0" \
	"$(sq "$LEGACY_DB" 'SELECT count(*) FROM collection;')"
check "recreating the handle did not change the address" "same" \
	"$([ "$URL_LEG_BEFORE" = "$URL_LEG_AFTER" ] && echo same || echo different)"

# So this is what "restore the collection of the user currently called legacy"
# resolves to — and it is somebody else's collection.
restore_to legacy-latest.sqlite "$URL_LEG_AFTER" -integrity-check full
check "restoring the RECREATED user hands back the DELETED user's card" "1" \
	"$(restored_card_count legacy-latest.sqlite "$LEG_MARKER")"
restore_to legacy-pit.sqlite "$URL_LEG_AFTER" -timestamp "$T_LEG" -integrity-check full
check "and so does a point-in-time restore inside the retention window" "1" \
	"$(restored_card_count legacy-pit.sqlite "$LEG_MARKER")"
check "with integrity_check ok — the sting in pd-pm7b" "ok" \
	"$(restored_state legacy-pit.sqlite)"

# ── RESULT ──────────────────────────────────────────────────────────────────
log "RESULT"
echo "  ${pass} passed, ${fail} failed"
echo "  S3 target: ${S3_TARGET}"
[ -n "${RECREATE_REAL_S3:-}" ] || echo "  (real-S3 credentials were not requested; rerun with RECREATE_REAL_S3=1)"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — pd-pm7b is CLOSED."
echo "         alice was created (${ID_OLD}), removed, and created again (${ID_NEW})."
echo "         The ids differ, the replica prefixes differ, and no restore of the"
echo "         second alice — latest or point-in-time inside the retention window —"
echo "         returns the first alice's card, which is still in the bucket and"
echo "         still healthy under the id that names it. The same procedure under"
echo "         handle-derived naming hands that card straight back (§7)."
