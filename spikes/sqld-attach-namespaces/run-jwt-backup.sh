#!/usr/bin/env bash
# Combined spike: per-namespace JWT auth + per-namespace bottomless backup/restore
# (pokedumpster-181 final spike). Fully local (MinIO). Proves:
#   PART 1: a namespace's jwt_key scopes a client to its own namespace.
#   PART 2: each namespace's data independently backs up to S3 and restores
#           after total local data loss (the risk the bottomless spike flagged).
set -euo pipefail

SQLD_IMG="ghcr.io/tursodatabase/libsql-server:latest"
MINIO_IMG="docker.io/minio/minio:latest"
MC_IMG="docker.io/minio/mc:latest"
NET="pkdump-jb-net"; MINIO="pkdump-jb-minio"; SQLD="pkdump-jb-sqld"; VOL="pkdump-jb-data"
BUCKET="pkdump-mt"; HTTP_PORT=18080; ADMIN_PORT=19090
BASE="http://127.0.0.1:${HTTP_PORT}"; ADMIN="http://127.0.0.1:${ADMIN_PORT}"
AKEY="minioadmin"; SKEY="minioadmin123"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYS="$(mktemp -d)"

ok(){ printf '\033[1;32m%s\033[0m\n' "$*"; }
no(){ printf '\033[1;31m%s\033[0m\n' "$*"; }
c(){  printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
mc(){ podman run --rm --network "$NET" -e MC_HOST_local="http://${AKEY}:${SKEY}@${MINIO}:9000" "$MC_IMG" "$@"; }
jh(){ python3 "${HERE}/jwt_helper.py" "$@"; }

# probe helper: q <host> <token-or-empty> <sql...>  -> exit 0 allowed / nonzero denied
q(){ local host="$1" tok="$2"; shift 2; local args=(); for s in "$@"; do args+=(--sql "$s"); done
     if [ -n "$tok" ]; then python3 "${HERE}/probe.py" --base "$BASE" --host "$host" --auth "$tok" "${args[@]}"
     else python3 "${HERE}/probe.py" --base "$BASE" --host "$host" "${args[@]}"; fi; }

create_ns(){ curl -fsS -X POST "${ADMIN}/v1/namespaces/$1/create" -H 'content-type: application/json' -d '{}' >/dev/null; }
set_ns_key(){ curl -fsS "${ADMIN}/v1/namespaces/$1/config" \
  | python3 -c "import json,sys;c=json.load(sys.stdin);c['jwt_key']='$2';print(json.dumps(c))" \
  | curl -fsS -X POST "${ADMIN}/v1/namespaces/$1/config" -H 'content-type: application/json' --data @- >/dev/null; }
ns_exists(){ curl -fsS -o /dev/null "${ADMIN}/v1/namespaces/$1/config" 2>/dev/null; }

start_sqld(){
  podman run -d --name "$SQLD" --network "$NET" \
    -p 127.0.0.1:${HTTP_PORT}:8080 -p 127.0.0.1:${ADMIN_PORT}:9090 \
    -v "${VOL}:/var/lib/sqld" -e SQLD_DB_PATH=/var/lib/sqld/iku.db \
    -e SQLD_ENABLE_BOTTOMLESS_REPLICATION=true \
    -e LIBSQL_BOTTOMLESS_ENDPOINT="http://${MINIO}:9000" -e LIBSQL_BOTTOMLESS_BUCKET="$BUCKET" \
    -e LIBSQL_BOTTOMLESS_DATABASE_ID="pkdump-mt-db" \
    -e LIBSQL_BOTTOMLESS_AWS_ACCESS_KEY_ID="$AKEY" -e LIBSQL_BOTTOMLESS_AWS_SECRET_ACCESS_KEY="$SKEY" \
    -e LIBSQL_BOTTOMLESS_AWS_DEFAULT_REGION="us-east-1" \
    "$SQLD_IMG" /bin/sqld --admin-listen-addr 0.0.0.0:9090 --enable-namespaces --enable-bottomless-replication >/dev/null
  for i in $(seq 1 40); do curl -fsS "${BASE}/health" >/dev/null 2>&1 && { ok "sqld up ($1) after ${i}s"; return; }
    [ "$i" = 40 ] && { no "sqld never came up ($1)"; podman logs "$SQLD" | tail -40; exit 1; }; sleep 1; done
}

cleanup(){ podman rm -f "$SQLD" "$MINIO" >/dev/null 2>&1 || true
  podman volume rm -f "$VOL" >/dev/null 2>&1 || true
  podman network rm -f "$NET" >/dev/null 2>&1 || true; rm -rf "$KEYS"; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup; mkdir -p "$KEYS"; podman network create "$NET" >/dev/null

c "MinIO + bucket"
podman run -d --name "$MINIO" --network "$NET" -e MINIO_ROOT_USER="$AKEY" -e MINIO_ROOT_PASSWORD="$SKEY" "$MINIO_IMG" server /data >/dev/null
for i in $(seq 1 40); do mc ready local >/dev/null 2>&1 && { ok "minio ready"; break; }; [ "$i" = 40 ] && { no "minio fail"; exit 1; }; sleep 1; done
mc mb "local/${BUCKET}" >/dev/null && ok "bucket ${BUCKET}"

c "Start sqld (namespaces + bottomless), mint per-tenant keys"
start_sqld "initial"
PUBA=$(jh genkey --priv "${KEYS}/a.pem" --pub "${KEYS}/a.pub"); TOKA=$(jh sign --priv "${KEYS}/a.pem")
PUBB=$(jh genkey --priv "${KEYS}/b.pem" --pub "${KEYS}/b.pub"); TOKB=$(jh sign --priv "${KEYS}/b.pem")
ok "tenantA pub=${PUBA:0:12}…  tenantB pub=${PUBB:0:12}…"

c "Create namespaces, set per-namespace jwt_key, seed (with tokens)"
create_ns tenantA; create_ns tenantB; ok "namespaces created"
echo "baseline (before key set): no-token write to tenantA ->"
if q tenantA.localhost "" "CREATE TABLE IF NOT EXISTS widgets(id INTEGER PRIMARY KEY, label TEXT)" >/dev/null 2>&1; then echo "  OPEN (no auth yet)"; else echo "  already closed"; fi
set_ns_key tenantA "$PUBA"; set_ns_key tenantB "$PUBB"; ok "jwt_key set on both namespaces"
q tenantA.localhost "$TOKA" \
  "CREATE TABLE IF NOT EXISTS widgets(id INTEGER PRIMARY KEY, label TEXT)" \
  "DELETE FROM widgets" "INSERT INTO widgets(id,label) VALUES (1,'A-one'),(2,'A-two')" >/dev/null && ok "seeded tenantA (2 rows)"
q tenantB.localhost "$TOKB" \
  "CREATE TABLE IF NOT EXISTS widgets(id INTEGER PRIMARY KEY, label TEXT)" \
  "DELETE FROM widgets" "INSERT INTO widgets(id,label) VALUES (1,'B-one'),(2,'B-two'),(3,'B-three')" >/dev/null && ok "seeded tenantB (3 rows)"

c "PART 1 — AUTH MATRIX"
SEL="SELECT count(*) FROM widgets"
declare -A R
if q tenantA.localhost ""     "$SEL" >/dev/null 2>&1; then R[no_token]=ALLOW; else R[no_token]=DENY; fi
if q tenantA.localhost "$TOKA" "$SEL" >/dev/null 2>&1; then R[A_on_A]=ALLOW;  else R[A_on_A]=DENY;  fi
if q tenantB.localhost "$TOKA" "$SEL" >/dev/null 2>&1; then R[A_on_B]=ALLOW;  else R[A_on_B]=DENY;  fi
if q tenantB.localhost "$TOKB" "$SEL" >/dev/null 2>&1; then R[B_on_B]=ALLOW;  else R[B_on_B]=DENY;  fi
row(){ printf "  %-26s got=%-5s want=%-5s  %s\n" "$1" "$2" "$3" "$([ "$2" = "$3" ] && echo OK || echo MISMATCH)"; }
row "no token -> tenantA"      "${R[no_token]}" "DENY"
row "tokenA  -> tenantA"       "${R[A_on_A]}"   "ALLOW"
row "tokenA  -> tenantB (scope)" "${R[A_on_B]}" "DENY"
row "tokenB  -> tenantB"       "${R[B_on_B]}"   "ALLOW"
AUTH_PASS=$([ "${R[no_token]}" = DENY ] && [ "${R[A_on_A]}" = ALLOW ] && [ "${R[A_on_B]}" = DENY ] && [ "${R[B_on_B]}" = ALLOW ] && echo 1 || echo 0)

c "PART 2 — per-namespace replication, then DESTROY local volume"
sleep 8
echo "bucket prefixes (expect per-namespace generations):"
mc ls "local/${BUCKET}" || true
podman stop -t 30 "$SQLD" >/dev/null && ok "graceful stop (flush)"
sleep 3
echo "objects after shutdown:"; mc ls --recursive "local/${BUCKET}" | sed 's/^/  /' | head -20
podman rm -f "$SQLD" >/dev/null; podman volume rm -f "$VOL" >/dev/null && ok "destroyed local volume — local data GONE"

c "RESTORE: fresh sqld, empty volume, same bucket"
start_sqld "restored"; sleep 3
if ns_exists tenantA && ns_exists tenantB; then ok "namespaces auto-present after restore (meta-store backed up)"; AUTONS=1
else
  no "namespaces NOT auto-present — recreating to trigger per-namespace restore-on-open"; AUTONS=0
  create_ns tenantA 2>/dev/null || true; create_ns tenantB 2>/dev/null || true; sleep 2
fi
A_AFTER=$(q tenantA.localhost "$TOKA" "SELECT id,label FROM widgets ORDER BY id" 2>&1 || true); echo "$A_AFTER"
B_AFTER=$(q tenantB.localhost "$TOKB" "SELECT id,label FROM widgets ORDER BY id" 2>&1 || true); echo "$B_AFTER"
A_OK=$([ "$(echo "$A_AFTER" | grep -c 'A-one\|A-two')" = "2" ] && echo 1 || echo 0)
B_OK=$([ "$(echo "$B_AFTER" | grep -c 'B-one\|B-two\|B-three')" = "3" ] && echo 1 || echo 0)
BACKUP_PASS=$([ "$A_OK" = 1 ] && [ "$B_OK" = 1 ] && echo 1 || echo 0)

c "VERDICT"
echo "  PART 1 auth scoping:        $([ "$AUTH_PASS" = 1 ] && ok PASS || no FAIL)"
echo "  PART 2 per-ns backup/restore: $([ "$BACKUP_PASS" = 1 ] && ok PASS || no FAIL)  (auto-namespaces=${AUTONS}, A=${A_OK}/1 B=${B_OK}/1)"
[ "${KEEP:-0}" = 1 ] && echo && echo "KEEP=1: stack up (sqld ${BASE}, admin ${ADMIN})."
[ "$AUTH_PASS" = 1 ] && [ "$BACKUP_PASS" = 1 ]