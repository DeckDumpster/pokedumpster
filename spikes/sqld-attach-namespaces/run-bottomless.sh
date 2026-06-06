#!/usr/bin/env bash
# Spike: bottomless S3 backup + restore-after-loss, fully local (pokedumpster-13w).
# Flow: replicate sqld -> MinIO, destroy sqld's local volume, restart, prove the
# data auto-restores from the bucket. No external creds; nothing leaves the box.
set -euo pipefail

SQLD_IMG="ghcr.io/tursodatabase/libsql-server:latest"
MINIO_IMG="docker.io/minio/minio:latest"
MC_IMG="docker.io/minio/mc:latest"

NET="pkdump-bl-net"
MINIO="pkdump-bl-minio"
SQLD="pkdump-bl-sqld"
VOL="pkdump-bl-sqld-data"
BUCKET="pkdump-test"
HTTP_PORT=18080
BASE="http://127.0.0.1:${HTTP_PORT}"

AKEY="minioadmin"
SKEY="minioadmin123"          # MinIO requires >= 8 chars
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="python3 ${HERE}/probe.py --base ${BASE} --host localhost"

ok()  { printf '\033[1;32m%s\033[0m\n' "$*"; }
no()  { printf '\033[1;31m%s\033[0m\n' "$*"; }
c()   { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
mc()  { podman run --rm --network "$NET" -e MC_HOST_local="http://${AKEY}:${SKEY}@${MINIO}:9000" "$MC_IMG" "$@"; }

start_sqld() {  # $1 = friendly note
  podman run -d --name "$SQLD" --network "$NET" \
    -p 127.0.0.1:${HTTP_PORT}:8080 \
    -v "${VOL}:/var/lib/sqld" \
    -e SQLD_DB_PATH=/var/lib/sqld/iku.db \
    -e SQLD_ENABLE_BOTTOMLESS_REPLICATION=true \
    -e LIBSQL_BOTTOMLESS_ENDPOINT="http://${MINIO}:9000" \
    -e LIBSQL_BOTTOMLESS_BUCKET="$BUCKET" \
    -e LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID="$AKEY" \
    -e LIBSQL_BOTTOMLESS_AWS_SECRET_ACCESS_KEY="$SKEY" \
    -e LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION="us-east-1" \
    -e AWS_ACCESS_KEY_ID="$AKEY" \
    -e AWS_SECRET_ACCESS_KEY="$SKEY" \
    -e AWS_DEFAULT_REGION="us-east-1" \
    -e AWS_REGION="us-east-1" \
    "$SQLD_IMG" /bin/sqld --enable-bottomless-replication >/dev/null
  for i in $(seq 1 40); do
    curl -fsS "${BASE}/health" >/dev/null 2>&1 && { ok "sqld up ($1) after ${i}s"; return; }
    [ "$i" = 40 ] && { no "sqld never came up ($1)"; podman logs "$SQLD" | tail -40; exit 1; }
    sleep 1
  done
}

cleanup() {
  podman rm -f "$SQLD" "$MINIO" >/dev/null 2>&1 || true
  podman volume rm -f "$VOL" >/dev/null 2>&1 || true
  podman network rm -f "$NET" >/dev/null 2>&1 || true
}
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup

c "Network + MinIO"
podman network create "$NET" >/dev/null
podman run -d --name "$MINIO" --network "$NET" \
  -e MINIO_ROOT_USER="$AKEY" -e MINIO_ROOT_PASSWORD="$SKEY" \
  "$MINIO_IMG" server /data >/dev/null
for i in $(seq 1 40); do
  mc ready local >/dev/null 2>&1 && { ok "minio ready after ${i}s"; break; }
  [ "$i" = 40 ] && { no "minio never ready"; podman logs "$MINIO" | tail -20; exit 1; }
  sleep 1
done
mc mb "local/${BUCKET}" >/dev/null && ok "bucket created: ${BUCKET}"

c "Start sqld with bottomless, seed data"
start_sqld "initial"
$PROBE \
  --sql "CREATE TABLE IF NOT EXISTS widgets(id INTEGER PRIMARY KEY, label TEXT)" \
  --sql "DELETE FROM widgets" \
  --sql "INSERT INTO widgets(id,label) VALUES (1,'alpha'),(2,'beta'),(3,'gamma'),(4,'delta'),(5,'epsilon')" >/dev/null
ok "seeded 5 widgets"
echo "rows before loss:"; $PROBE --sql "SELECT id,label FROM widgets ORDER BY id" || true

c "Let bottomless replicate, then inspect the bucket"
sleep 8
echo "objects in s3://${BUCKET} (proof of replication):"
mc ls --recursive "local/${BUCKET}" | head -20
GEN_OBJECTS=$(mc ls --recursive "local/${BUCKET}" | wc -l | tr -d ' ')
echo "object count: ${GEN_OBJECTS}"

c "SIMULATE DISK LOSS: graceful stop + destroy sqld's local volume"
podman stop -t 30 "$SQLD" >/dev/null && ok "sqld stopped gracefully (final flush)"
sleep 3
echo "objects after graceful shutdown:"; mc ls --recursive "local/${BUCKET}" | tail -8
podman rm -f "$SQLD" >/dev/null
podman volume rm -f "$VOL" >/dev/null && ok "destroyed local volume ${VOL} — local data is GONE"

c "RESTORE TEST: fresh sqld, empty volume, SAME bucket -> auto-restore on startup"
start_sqld "restored"
sleep 3   # give restore a moment
AFTER=$($PROBE --sql "SELECT id,label FROM widgets ORDER BY id" 2>&1)
echo "$AFTER"
RESTORED=$(echo "$AFTER" | grep -c -E '^\s+[0-9]+ \| ' || true)

c "VERDICT"
if echo "$AFTER" | grep -q "epsilon" && [ "${RESTORED}" = "5" ]; then
  ok "PASS — bottomless replicated to MinIO and AUTO-RESTORED all 5 rows after total local loss."
  echo "=> backup/restore via bottomless works; can replace backup.sh toil for single-DB."
  RC=0
else
  no "FAIL — data did not come back after restore (restored rows: ${RESTORED})."
  echo "--- sqld logs (restore phase) ---"; podman logs "$SQLD" 2>&1 | grep -iE 'bottomless|restore|generation|error' | tail -25
  RC=1
fi

[ "${KEEP:-0}" = "1" ] && echo && echo "KEEP=1: stack left up (sqld ${BASE}, minio container ${MINIO})."
exit ${RC:-0}
