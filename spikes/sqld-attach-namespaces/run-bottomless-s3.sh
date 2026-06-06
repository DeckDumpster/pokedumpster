#!/usr/bin/env bash
# bottomless backup/restore against a REAL S3-compatible bucket (single-DB mode —
# matches today's single-user app). Proves replicate -> destroy local volume ->
# restart -> auto-restore, against actual cloud storage.
#
# SECRETS: never pass creds on the CLI or in this repo. Create an env file
# (default ~/.pkdump-s3-spike.env, chmod 600) in your own editor:
#
#   S3_BUCKET=my-pkdump-backup-test
#   S3_ENDPOINT=https://s3.us-east-1.amazonaws.com   # AWS; or R2/Tigris/B2 endpoint
#   S3_REGION=us-east-1
#   S3_ACCESS_KEY_ID=AKIA...
#   S3_SECRET_ACCESS_KEY=...
#
# Then:  spikes/sqld-attach-namespaces/run-bottomless-s3.sh
# Use a THROWAWAY/test bucket and a SCOPED key (access to only that bucket).
# 5 test rows are uploaded; set CLEANUP=1 to delete the test prefix afterward.
set -euo pipefail

ENV_FILE="${S3_ENV_FILE:-$HOME/.pkdump-s3-spike.env}"
[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }

: "${S3_BUCKET:?set S3_BUCKET (see header / env file)}"
: "${S3_ENDPOINT:?set S3_ENDPOINT}"
: "${S3_ACCESS_KEY_ID:?set S3_ACCESS_KEY_ID}"
: "${S3_SECRET_ACCESS_KEY:?set S3_SECRET_ACCESS_KEY}"
S3_REGION="${S3_REGION:-us-east-1}"
DBID="pkdump-s3-spike"          # stable bucket prefix: ns-${DBID}:default-<uuid>/

SQLD_IMG="ghcr.io/tursodatabase/libsql-server:latest"
MC_IMG="docker.io/minio/mc:latest"
SQLD="pkdump-s3-sqld"; VOL="pkdump-s3-data"; HTTP_PORT=18080
BASE="http://127.0.0.1:${HTTP_PORT}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="python3 ${HERE}/probe.py --base ${BASE} --host localhost"

ok(){ printf '\033[1;32m%s\033[0m\n' "$*"; }; no(){ printf '\033[1;31m%s\033[0m\n' "$*"; }
c(){ printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
# mc against the real endpoint; creds passed as args (handles special chars), best-effort.
mc(){ podman run --rm -e AK="$S3_ACCESS_KEY_ID" -e SK="$S3_SECRET_ACCESS_KEY" -e EP="$S3_ENDPOINT" \
        "$MC_IMG" sh -c 'mc alias set s3 "$EP" "$AK" "$SK" >/dev/null 2>&1 && mc "$@"' _ "$@"; }

start_sqld(){
  podman run -d --name "$SQLD" -p 127.0.0.1:${HTTP_PORT}:8080 \
    -v "${VOL}:/var/lib/sqld" -e SQLD_DB_PATH=/var/lib/sqld/iku.db \
    -e SQLD_ENABLE_BOTTOMLESS_REPLICATION=true \
    -e LIBSQL_BOTTOMLESS_ENDPOINT="$S3_ENDPOINT" -e LIBSQL_BOTTOMLESS_BUCKET="$S3_BUCKET" \
    -e LIBSQL_BOTTOMLESS_DATABASE_ID="$DBID" \
    -e LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" \
    -e LIBSQL_BOTTOMLESS_AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" \
    -e LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION="$S3_REGION" \
    "$SQLD_IMG" /bin/sqld --enable-bottomless-replication >/dev/null
  for i in $(seq 1 40); do curl -fsS "${BASE}/health" >/dev/null 2>&1 && { ok "sqld up ($1) after ${i}s"; return; }
    [ "$i" = 40 ] && { no "sqld never came up ($1)"; podman logs "$SQLD" | tail -40; exit 1; }; sleep 1; done
}
cleanup(){ podman rm -f "$SQLD" >/dev/null 2>&1 || true; podman volume rm -f "$VOL" >/dev/null 2>&1 || true; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup

c "Target: ${S3_ENDPOINT}  bucket=${S3_BUCKET}  prefix=ns-${DBID}:default-*"
echo "bucket reachable? listing current contents (best-effort):"
mc ls --recursive "s3/${S3_BUCKET}" 2>&1 | sed 's/^/  /' | head -10 || true

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
echo "objects in bucket after shutdown:"; mc ls --recursive "s3/${S3_BUCKET}/ns-${DBID}:default" 2>&1 | sed 's/^/  /' | head -20 || true
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
  podman logs "$SQLD" 2>&1 | grep -iE 'bottomless|restore|generation|error|denied|credential' | tail -25
  RC=1
fi

if [ "${CLEANUP:-0}" = 1 ]; then
  c "CLEANUP: removing test prefix from bucket"
  mc rm --recursive --force "s3/${S3_BUCKET}/ns-${DBID}:default" 2>&1 | tail -3 || true
fi
[ "${KEEP:-0}" = 1 ] && echo && echo "KEEP=1: sqld left up on ${BASE}."
exit ${RC:-0}
