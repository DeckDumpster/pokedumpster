#!/usr/bin/env bash
# bottomless backup/restore against a REAL S3-compatible bucket (single-DB mode —
# matches today's single-user app). Proves replicate -> destroy local volume ->
# restart -> auto-restore, against actual cloud storage.
#
# Creds via a user-created env file (default ~/.pkdump-s3-spike.env, chmod 600):
#   S3_BUCKET=...           S3_ENDPOINT=https://s3.us-west-2.amazonaws.com
#   S3_REGION=us-west-2     S3_ACCESS_KEY_ID=...   S3_SECRET_ACCESS_KEY=...
#   S3_ROLE_ARN=arn:aws:iam::ACCT:role/...   # OPTIONAL: assume this role via STS;
#                                            # the key above only needs sts:AssumeRole.
# Use a THROWAWAY bucket. 5 test rows are uploaded; CLEANUP=1 deletes them after.
#
# NOTE on assumed-role creds: STS temp creds expire (default 1h). Fine for this
# <1min spike; NOT a production credential strategy for a long-running server
# (bottomless takes static env creds and does not refresh them). See RESULT.md.
set -euo pipefail

ENV_FILE="${S3_ENV_FILE:-$HOME/.pkdump-s3-spike.env}"
[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }
: "${S3_BUCKET:?set S3_BUCKET}"; : "${S3_ENDPOINT:?set S3_ENDPOINT}"
: "${S3_ACCESS_KEY_ID:?set S3_ACCESS_KEY_ID}"; : "${S3_SECRET_ACCESS_KEY:?set S3_SECRET_ACCESS_KEY}"
S3_REGION="${S3_REGION:-us-east-1}"; S3_ROLE_ARN="${S3_ROLE_ARN:-}"
DBID="pkdump-s3-spike"

SQLD_IMG="ghcr.io/tursodatabase/libsql-server:latest"
AWSCLI_IMG="docker.io/amazon/aws-cli:latest"
SQLD="pkdump-s3-sqld"; VOL="pkdump-s3-data"; HTTP_PORT=18080
BASE="http://127.0.0.1:${HTTP_PORT}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="python3 ${HERE}/probe.py --base ${BASE} --host localhost"

ok(){ printf '\033[1;32m%s\033[0m\n' "$*"; }; no(){ printf '\033[1;31m%s\033[0m\n' "$*"; }
c(){ printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }

# Effective creds: static, or temporary from assume-role.
EAK="$S3_ACCESS_KEY_ID"; ESK="$S3_SECRET_ACCESS_KEY"; ETOKEN=""
assume_role(){
  local out
  out=$(podman run --rm -e AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" -e AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" \
        -e AWS_DEFAULT_REGION="$S3_REGION" "$AWSCLI_IMG" \
        sts assume-role --role-arn "$S3_ROLE_ARN" --role-session-name pkdump-spike \
        --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' --output text 2>&1)
  read -r EAK ESK ETOKEN <<< "$out"
  case "$EAK" in ASIA*) ok "assumed ${S3_ROLE_ARN##*/} — temp key ...${EAK: -4}, token len=${#ETOKEN}";;
    *) no "assume-role FAILED:"; echo "$out"; exit 1;; esac
}

# aws-cli with the effective creds (+ session token when present).
AWS(){ local a=(-e AWS_ACCESS_KEY_ID="$EAK" -e AWS_SECRET_ACCESS_KEY="$ESK" -e AWS_DEFAULT_REGION="$S3_REGION");
       [ -n "$ETOKEN" ] && a+=(-e AWS_SESSION_TOKEN="$ETOKEN"); podman run --rm "${a[@]}" "$AWSCLI_IMG" "$@"; }

start_sqld(){
  local creds=(-e LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID="$EAK" -e LIBSQL_BOTTOMLESS_AWS_SECRET_ACCESS_KEY="$ESK"
               -e LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION="$S3_REGION")
  [ -n "$ETOKEN" ] && creds+=(-e AWS_SESSION_TOKEN="$ETOKEN" -e LIBSQL_BOTTOMLESS_AWS_SESSION_TOKEN="$ETOKEN")
  podman run -d --name "$SQLD" -p 127.0.0.1:${HTTP_PORT}:8080 -v "${VOL}:/var/lib/sqld" \
    -e SQLD_DB_PATH=/var/lib/sqld/iku.db -e SQLD_ENABLE_BOTTOMLESS_REPLICATION=true \
    -e LIBSQL_BOTTOMLESS_ENDPOINT="$S3_ENDPOINT" -e LIBSQL_BOTTOMLESS_BUCKET="$S3_BUCKET" \
    -e LIBSQL_BOTTOMLESS_DATABASE_ID="$DBID" "${creds[@]}" \
    "$SQLD_IMG" /bin/sqld --enable-bottomless-replication >/dev/null
  for i in $(seq 1 40); do curl -fsS "${BASE}/health" >/dev/null 2>&1 && { ok "sqld up ($1) after ${i}s"; return; }
    [ "$i" = 40 ] && { no "sqld never came up ($1)"; podman logs "$SQLD" 2>&1 | tail -25; exit 1; }; sleep 1; done
}
cleanup(){ podman rm -f "$SQLD" >/dev/null 2>&1 || true; podman volume rm -f "$VOL" >/dev/null 2>&1 || true; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup

c "Target: ${S3_ENDPOINT}  bucket=${S3_BUCKET}  prefix=ns-${DBID}:default-*"
[ -n "$S3_ROLE_ARN" ] && { c "Assume role for temporary credentials"; assume_role; }

echo "validate bucket access with effective creds (aws s3 ls):"
AWS s3 ls "s3://${S3_BUCKET}" 2>&1 | sed 's/^/  /' | head -5 || { no "cannot access bucket with effective creds"; exit 1; }
ok "bucket reachable"

c "Start sqld with bottomless -> real S3, seed data"
start_sqld "initial"
$PROBE \
  --sql "CREATE TABLE IF NOT EXISTS widgets(id INTEGER PRIMARY KEY, label TEXT)" \
  --sql "DELETE FROM widgets" \
  --sql "INSERT INTO widgets(id,label) VALUES (1,'alpha'),(2,'beta'),(3,'gamma'),(4,'delta'),(5,'epsilon')" >/dev/null
ok "seeded 5 widgets"; echo "before loss:"; $PROBE --sql "SELECT id,label FROM widgets ORDER BY id" || true

c "Replicate, then SIMULATE LOSS (graceful stop + destroy local volume)"
sleep 8
podman stop -t 30 "$SQLD" >/dev/null && ok "graceful stop (final flush to S3)"
sleep 3
echo "objects in bucket after shutdown:"; AWS s3 ls --recursive "s3://${S3_BUCKET}/ns-${DBID}:default" 2>&1 | sed 's/^/  /' | head -20 || true
podman rm -f "$SQLD" >/dev/null; podman volume rm -f "$VOL" >/dev/null && ok "destroyed local volume — local data GONE"

c "RESTORE: fresh sqld, empty volume, SAME bucket + DATABASE_ID"
start_sqld "restored"; sleep 3
AFTER=$($PROBE --sql "SELECT id,label FROM widgets ORDER BY id" 2>&1); echo "$AFTER"
N=$(echo "$AFTER" | grep -c -E '^\s+[0-9]+ \| ' || true)

c "VERDICT"
if echo "$AFTER" | grep -q epsilon && [ "$N" = 5 ]; then
  ok "PASS — bottomless replicated to REAL S3 (${S3_ENDPOINT}) and auto-restored all 5 rows after total local loss."
  RC=0
else
  no "FAIL — data did not restore from S3 (rows=${N})."
  podman logs "$SQLD" 2>&1 | grep -iE 'bottomless|restore|generation|error|denied|credential|token' | tail -25
  RC=1
fi

if [ "${CLEANUP:-0}" = 1 ]; then
  c "CLEANUP: stop sqld (halt replication), then remove test prefix from bucket"
  podman rm -f "$SQLD" >/dev/null 2>&1 || true   # else the running server re-uploads mid-delete
  # glob form: bare-prefix `s3 rm` misses keys that continue with '-' (not '/').
  AWS s3 rm "s3://${S3_BUCKET}/" --recursive --exclude "*" --include "ns-${DBID}:*" 2>&1 | tail -10 || true
fi
[ "${KEEP:-0}" = 1 ] && echo && echo "KEEP=1: sqld left up on ${BASE}."
exit ${RC:-0}
