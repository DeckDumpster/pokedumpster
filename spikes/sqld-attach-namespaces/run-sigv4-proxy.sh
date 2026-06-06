#!/usr/bin/env bash
# Spike pokedumpster-8ch.9: bottomless behind an aws-sigv4-proxy egress sidecar.
# The proxy assumes role/pokedump-data (--role-arn, auto-refreshing temp creds)
# and SigV4-signs; sqld/bottomless sends path-style S3 to the proxy with DUMMY
# creds and holds no real credentials. Proves replicate -> lose -> restore still
# works through the sidecar. Fully local; creds from ~/.pkdump-s3-spike.env.
set -euo pipefail

ENV_FILE="${S3_ENV_FILE:-$HOME/.pkdump-s3-spike.env}"
[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }
: "${S3_BUCKET:?}"; : "${S3_REGION:?}"; : "${S3_ROLE_ARN:?set S3_ROLE_ARN}"
: "${S3_ACCESS_KEY_ID:?}"; : "${S3_SECRET_ACCESS_KEY:?}"
DBID="pkdump-sv-spike"

SQLD_IMG="ghcr.io/tursodatabase/libsql-server:latest"
PROXY_IMG="public.ecr.aws/aws-observability/aws-sigv4-proxy:latest"
AWSCLI_IMG="docker.io/amazon/aws-cli:latest"
NET="pkdump-sv-net"; PROXY="pkdump-sv-proxy"; SQLD="pkdump-sv-sqld"; VOL="pkdump-sv-data"
HTTP_PORT=18080; PROXY_PORT=18091
BASE="http://127.0.0.1:${HTTP_PORT}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="python3 ${HERE}/probe.py --base ${BASE} --host localhost"

ok(){ printf '\033[1;32m%s\033[0m\n' "$*"; }; no(){ printf '\033[1;31m%s\033[0m\n' "$*"; }
c(){ printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }

# Real assumed-role creds for out-of-band bucket verify/cleanup (separate from the proxy).
EAK=""; ESK=""; ETOKEN=""
assume(){ local o; o=$(podman run --rm -e AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" -e AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" -e AWS_DEFAULT_REGION="$S3_REGION" \
    "$AWSCLI_IMG" sts assume-role --role-arn "$S3_ROLE_ARN" --role-session-name pkdump-sv --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' --output text 2>&1)
  read -r EAK ESK ETOKEN <<< "$o"; case "$EAK" in ASIA*) ;; *) no "assume failed: $o"; exit 1;; esac; }
AWS(){ podman run --rm -e AWS_ACCESS_KEY_ID="$EAK" -e AWS_SECRET_ACCESS_KEY="$ESK" -e AWS_SESSION_TOKEN="$ETOKEN" -e AWS_DEFAULT_REGION="$S3_REGION" "$AWSCLI_IMG" "$@"; }

cleanup(){ podman rm -f "$SQLD" "$PROXY" >/dev/null 2>&1 || true; podman volume rm -f "$VOL" >/dev/null 2>&1 || true; podman network rm -f "$NET" >/dev/null 2>&1 || true; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup; podman network create "$NET" >/dev/null

c "Start aws-sigv4-proxy (assumes ${S3_ROLE_ARN##*/}, signs for s3)"
podman run -d --name "$PROXY" --network "$NET" -p 127.0.0.1:${PROXY_PORT}:8080 \
  -e AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" -e AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" -e AWS_REGION="$S3_REGION" \
  "$PROXY_IMG" --name s3 --region "$S3_REGION" --host "s3.${S3_REGION}.amazonaws.com" \
    --role-arn "$S3_ROLE_ARN" --unsigned-payload \
    -s Authorization -s X-Amz-Date -s X-Amz-Content-Sha256 -s X-Amz-User-Agent -v >/dev/null
    # ^ X-Amz-User-Agent MUST be stripped: the Rust SDK sends it, S3 requires all
    #   x-amz-* headers be signed, the proxy forwards it unsigned -> 403 SignatureDoesNotMatch.
sleep 4
podman ps --filter name="$PROXY" --format '{{.Names}} {{.Status}}' | sed 's/^/  /'

c "Sanity: UNSIGNED list-bucket through the proxy (proxy signs with assumed role)"
code=$(curl -s -o /tmp/sv_ls.xml -w '%{http_code}' "http://127.0.0.1:${PROXY_PORT}/${S3_BUCKET}/?list-type=2&max-keys=1" || true)
echo "  HTTP $code"
if [ "$code" != 200 ]; then no "proxy signing/list failed:"; sed 's/^/    /' /tmp/sv_ls.xml | head -8; podman logs "$PROXY" 2>&1 | tail -15; exit 1; fi
ok "proxy signs + reaches S3"

c "Start sqld -> bottomless via proxy (DUMMY creds; no real creds in sqld)"
podman run -d --name "$SQLD" --network "$NET" -p 127.0.0.1:${HTTP_PORT}:8080 \
  -v "${VOL}:/var/lib/sqld" -e SQLD_DB_PATH=/var/lib/sqld/iku.db -e SQLD_ENABLE_BOTTOMLESS_REPLICATION=true \
  -e LIBSQL_BOTTOMLESS_ENDPOINT="http://${PROXY}:8080" -e LIBSQL_BOTTOMLESS_BUCKET="$S3_BUCKET" \
  -e LIBSQL_BOTTOMLESS_DATABASE_ID="$DBID" \
  -e LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID=dummy -e LIBSQL_BOTTOMLESS_AWS_SECRET_ACCESS_KEY=dummy \
  -e LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION="$S3_REGION" \
  "$SQLD_IMG" /bin/sqld --enable-bottomless-replication >/dev/null
for i in $(seq 1 40); do curl -fsS "${BASE}/health" >/dev/null 2>&1 && { ok "sqld up after ${i}s"; break; }
  [ "$i" = 40 ] && { no "sqld never came up"; podman logs "$SQLD" 2>&1 | tail -25; exit 1; }; sleep 1; done

$PROBE --sql "CREATE TABLE IF NOT EXISTS widgets(id INTEGER PRIMARY KEY, label TEXT)" \
       --sql "DELETE FROM widgets" \
       --sql "INSERT INTO widgets(id,label) VALUES (1,'alpha'),(2,'beta'),(3,'gamma'),(4,'delta'),(5,'epsilon')" >/dev/null
ok "seeded 5 widgets"

c "Replicate via proxy, then DESTROY local volume"
sleep 8
podman stop -t 30 "$SQLD" >/dev/null && ok "graceful stop (flush through proxy)"; sleep 3
assume
echo "objects in bucket (verified out-of-band with real creds):"; AWS s3 ls --recursive "s3://${S3_BUCKET}/ns-${DBID}:default" 2>&1 | sed 's/^/  /' | head
podman rm -f "$SQLD" >/dev/null; podman volume rm -f "$VOL" >/dev/null && ok "destroyed local volume"

c "RESTORE: fresh sqld through the same proxy"
podman run -d --name "$SQLD" --network "$NET" -p 127.0.0.1:${HTTP_PORT}:8080 \
  -v "${VOL}:/var/lib/sqld" -e SQLD_DB_PATH=/var/lib/sqld/iku.db -e SQLD_ENABLE_BOTTOMLESS_REPLICATION=true \
  -e LIBSQL_BOTTOMLESS_ENDPOINT="http://${PROXY}:8080" -e LIBSQL_BOTTOMLESS_BUCKET="$S3_BUCKET" \
  -e LIBSQL_BOTTOMLESS_DATABASE_ID="$DBID" \
  -e LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID=dummy -e LIBSQL_BOTTOMLESS_AWS_SECRET_ACCESS_KEY=dummy \
  -e LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION="$S3_REGION" \
  "$SQLD_IMG" /bin/sqld --enable-bottomless-replication >/dev/null
for i in $(seq 1 40); do curl -fsS "${BASE}/health" >/dev/null 2>&1 && break; sleep 1; done
sleep 3
AFTER=$($PROBE --sql "SELECT id,label FROM widgets ORDER BY id" 2>&1); echo "$AFTER"
N=$(echo "$AFTER" | grep -c -E '^\s+[0-9]+ \| ' || true)

c "VERDICT"
if echo "$AFTER" | grep -q epsilon && [ "$N" = 5 ]; then
  ok "PASS — bottomless replicated + restored through the aws-sigv4-proxy sidecar; sqld held only dummy creds."
  RC=0
else
  no "FAIL — restore through proxy did not return the data (rows=$N)."
  echo "--- sqld ---"; podman logs "$SQLD" 2>&1 | grep -iE 'bottomless|restore|error|denied|sign|credential' | tail -15
  echo "--- proxy ---"; podman logs "$PROXY" 2>&1 | tail -15
  RC=1
fi

# stop sqld first (halt replication), then glob-delete the test prefix with real creds
podman rm -f "$SQLD" >/dev/null 2>&1 || true
AWS s3 rm "s3://${S3_BUCKET}/" --recursive --exclude "*" --include "ns-${DBID}:*" 2>&1 | tail -3 || true
[ "${KEEP:-0}" = 1 ] && echo && echo "KEEP=1: proxy ${PROXY_PORT} / sqld ${BASE} left up."
exit ${RC:-0}
