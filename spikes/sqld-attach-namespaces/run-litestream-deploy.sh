#!/usr/bin/env bash
# 8ch.7 validation: prove the Litestream backup sidecar works with the ACTUAL
# deploy config (deploy/litestream.yml) and the project credential standard —
# assume-role via ~/.aws/config (SDK auto-refresh), NOT direct/static creds.
#
# Replicates the FIXTURE collection DB (26 rows) to real S3 exactly as the
# Quadlet unit would, simulates total loss, restores, and verifies the rows.
# No 1h soak (single-user phase). Creds (bootstrap key + role) from the env file.
set -euo pipefail

ENV_FILE="${S3_ENV_FILE:-$HOME/.pkdump-s3-spike.env}"
[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }
: "${S3_BUCKET:?}"; : "${S3_REGION:?}"; : "${S3_ROLE_ARN:?}"
: "${S3_ACCESS_KEY_ID:?}"; : "${S3_SECRET_ACCESS_KEY:?}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${HERE}/../.." && pwd)"
LS_YML="${REPO_DIR}/deploy/litestream.yml"
FIXTURE="${REPO_DIR}/tests/ui/fixtures/collection.sqlite"
LS="docker.io/litestream/litestream:latest"
AWSCLI="docker.io/amazon/aws-cli:latest"
CTR="pkdump-lsdeploy"
PREFIX="litestream-deploy-verify"
WORK="$(mktemp -d)"; chmod 777 "$WORK"

ok(){ printf '\033[1;32m%s\033[0m\n' "$*"; }; no(){ printf '\033[1;31m%s\033[0m\n' "$*"; }
c(){ printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }

# out-of-band verify/cleanup creds (assume-role, like the other spikes)
EAK=""; ESK=""; ETOKEN=""
assume(){ local o; o=$(podman run --rm -e AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" -e AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" -e AWS_DEFAULT_REGION="$S3_REGION" \
  "$AWSCLI" sts assume-role --role-arn "$S3_ROLE_ARN" --role-session-name lsdeploy-vfy --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' --output text 2>&1)
  read -r EAK ESK ETOKEN <<< "$o"; case "$EAK" in ASIA*) ;; *) no "assume failed: $o"; exit 1;; esac; }
AWS(){ podman run --rm -e AWS_ACCESS_KEY_ID="$EAK" -e AWS_SECRET_ACCESS_KEY="$ESK" -e AWS_SESSION_TOKEN="$ETOKEN" -e AWS_DEFAULT_REGION="$S3_REGION" "$AWSCLI" "$@"; }

cleanup(){ podman rm -f "$CTR" >/dev/null 2>&1 || true
  if [ -n "$EAK" ]; then AWS s3 rm "s3://${S3_BUCKET}/" --recursive --exclude "*" --include "${PREFIX}/*" >/dev/null 2>&1 || true; fi
  rm -rf "$WORK"; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
assume

c "Stage: fixture collection DB (26 rows) + assume-role ~/.aws profile (the project standard)"
cp "$FIXTURE" "${WORK}/collection.sqlite"
mkdir -p "${WORK}/aws"
cat > "${WORK}/aws/config" <<EOF
[profile pkdump]
role_arn = ${S3_ROLE_ARN}
source_profile = bootstrap
region = ${S3_REGION}
EOF
cat > "${WORK}/aws/credentials" <<EOF
[bootstrap]
aws_access_key_id = ${S3_ACCESS_KEY_ID}
aws_secret_access_key = ${S3_SECRET_ACCESS_KEY}
EOF
BEFORE=$(sqlite3 "${WORK}/collection.sqlite" "SELECT count(*) FROM collection;")
ok "staged; collection rows = ${BEFORE}"

# podman flags shared by start + restore (invoked exactly like the Quadlet unit:
# same image, mounts, env) — creds come ONLY from the assume-role profile in /aws.
LS_ARGS=( --user 0:0
  -v "${WORK}:/data"
  -v "${LS_YML}:/etc/litestream.yml:ro"
  -v "${WORK}/aws:/aws:ro"
  -e AWS_CONFIG_FILE=/aws/config -e AWS_SHARED_CREDENTIALS_FILE=/aws/credentials -e AWS_PROFILE=pkdump
  -e LITESTREAM_S3_BUCKET="$S3_BUCKET" -e LITESTREAM_S3_REGION="$S3_REGION"
  -e LITESTREAM_S3_PATH="${PREFIX}/collection" -e LITESTREAM_DB_PATH=/data/collection.sqlite )

c "Start the sidecar (continuous replicate) — SDK assumes role/pokedump-data itself"
podman run -d --name "$CTR" "${LS_ARGS[@]}" "$LS" replicate -config /etc/litestream.yml >/dev/null
sleep 7
echo "--- sidecar log (expect a snapshot/sync, no credential errors) ---"
podman logs "$CTR" 2>&1 | tail -8 | sed 's/^/  /'
if podman logs "$CTR" 2>&1 | grep -qiE 'no such|denied|invalid|error.*credential|failed to assume'; then
  no "credential/replication error in sidecar log"; RC=1
fi

c "Objects in S3 (verified out-of-band):"
AWS s3 ls --recursive "s3://${S3_BUCKET}/${PREFIX}/" 2>&1 | sed 's/^/  /' | head

c "SIMULATE TOTAL LOSS: stop sidecar, delete the collection DB"
podman rm -f "$CTR" >/dev/null; rm -f "${WORK}/collection.sqlite" "${WORK}/collection.sqlite-wal" "${WORK}/collection.sqlite-shm"
ok "collection DB gone"

c "Restore from S3 (same config + assume-role creds)"
podman run --rm "${LS_ARGS[@]}" "$LS" restore -config /etc/litestream.yml -o /data/restored.sqlite /data/collection.sqlite 2>&1 | tail -5 | sed 's/^/  /'
AFTER=$(sqlite3 "${WORK}/restored.sqlite" "SELECT count(*) FROM collection;" 2>&1 || echo "RESTORE-READ-FAILED")

c "VERDICT"
if [ "$AFTER" = "$BEFORE" ] && [ "$BEFORE" -gt 0 ] 2>/dev/null; then
  ok "PASS — Litestream replicated + restored the fixture collection (${AFTER}/${BEFORE} rows) via the deploy config + ASSUME-ROLE auto-refresh creds. No static keys."
  RC=0
else
  no "FAIL — restored ${AFTER} rows, expected ${BEFORE}."; RC=1
fi
exit ${RC:-0}
