#!/usr/bin/env bash
# Spike: does self-hosted sqld support ATTACH across namespaces? (pokedumpster-5jv)
set -euo pipefail

IMG="ghcr.io/tursodatabase/libsql-server:latest"
NAME="sqld-spike"
HTTP_PORT=18080
ADMIN_PORT=19090
BASE="http://127.0.0.1:${HTTP_PORT}"
ADMIN="http://127.0.0.1:${ADMIN_PORT}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="python3 ${HERE}/probe.py --base ${BASE}"

c()  { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
ok() { printf '\033[1;32m%s\033[0m\n' "$*"; }
no() { printf '\033[1;31m%s\033[0m\n' "$*"; }

cleanup() { podman rm -f "$NAME" >/dev/null 2>&1 || true; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT

cleanup

c "Start sqld with namespaces + admin API"
# NOTE: the image's docker-entrypoint.sh auto-appends --db-path, --http-listen-addr
# (from $SQLD_HTTP_LISTEN_ADDR, default 0.0.0.0:8080) and --grpc-listen-addr.
# We pass ONLY the flags it does not set, or sqld errors on duplicates.
podman run -d --name "$NAME" \
  -p ${HTTP_PORT}:8080 -p ${ADMIN_PORT}:9090 "$IMG" \
  /bin/sqld \
    --admin-listen-addr 0.0.0.0:9090 \
    --enable-namespaces >/dev/null
echo "container: $NAME"

c "Wait for HTTP API"
for i in $(seq 1 40); do
  if curl -fsS "${BASE}/health" >/dev/null 2>&1; then ok "up after ${i}s"; break; fi
  if [ "$i" = 40 ]; then no "sqld never came up"; podman logs "$NAME" | tail -30; exit 1; fi
  sleep 1
done

create_ns() {
  local ns="$1"
  curl -fsS -X POST "${ADMIN}/v1/namespaces/${ns}/create" \
    -H 'Content-Type: application/json' -d '{}' >/dev/null \
    && ok "created namespace: ${ns}" \
    || { no "failed to create namespace: ${ns}"; return 1; }
}

# A namespace only permits being ATTACHed if its config has allow_attach=true.
# The config endpoint requires the FULL config object, so we GET-patch-POST.
enable_attach() {
  local ns="$1"
  curl -fsS "${ADMIN}/v1/namespaces/${ns}/config" \
    | python3 -c 'import json,sys; c=json.load(sys.stdin); c["allow_attach"]=True; print(json.dumps(c))' \
    | curl -fsS -X POST "${ADMIN}/v1/namespaces/${ns}/config" \
        -H 'Content-Type: application/json' --data @- >/dev/null \
    && ok "enabled allow_attach on: ${ns}" \
    || { no "failed to enable allow_attach on: ${ns}"; return 1; }
}

c "Create namespaces"
create_ns catalog
create_ns tenant1

c "Allow the shared catalog to be ATTACHed (allow_attach=true)"
enable_attach catalog

c "Seed catalog namespace (shared, read-only role)"
$PROBE --host catalog.localhost \
  --sql "CREATE TABLE IF NOT EXISTS cards(id INTEGER PRIMARY KEY, name TEXT)" \
  --sql "DELETE FROM cards" \
  --sql "INSERT INTO cards(id,name) VALUES (1,'Pikachu'),(2,'Charizard'),(3,'Mew')"

c "Seed tenant1 namespace (per-user collection)"
$PROBE --host tenant1.localhost \
  --sql "CREATE TABLE IF NOT EXISTS collection(id INTEGER PRIMARY KEY, card_id INTEGER, condition TEXT)" \
  --sql "DELETE FROM collection" \
  --sql "INSERT INTO collection(id,card_id,condition) VALUES (10,2,'NM'),(11,3,'LP'),(12,2,'MP')"

c "Sanity: each namespace queryable in isolation"
$PROBE --host catalog.localhost --sql "SELECT id,name FROM cards ORDER BY id"
$PROBE --host tenant1.localhost --sql "SELECT id,card_id,condition FROM collection ORDER BY id"

c "THE TEST: from tenant1, ATTACH catalog read-only and JOIN across them"
# Expect 3 rows: each owned copy resolved to its catalog card name.
# Try several ATTACH syntaxes; the first that returns rows wins.
JOIN_SQL='SELECT col.id AS copy, c.name AS card, col.condition AS cond
          FROM collection col JOIN cat.cards c ON c.id = col.card_id
          ORDER BY col.id'

PASS=""
for attach in \
  'ATTACH "catalog" AS cat' \
  "ATTACH 'catalog' AS cat" \
  'ATTACH DATABASE "catalog" AS cat' \
  'ATTACH catalog AS cat' ; do
  echo
  echo ">>> attach form: ${attach}"
  if out=$($PROBE --host tenant1.localhost \
        --sql "BEGIN" \
        --sql "$attach" \
        --sql "$JOIN_SQL" \
        --sql "COMMIT" 2>&1); then
    echo "$out"
    if echo "$out" | grep -q "Charizard"; then
      PASS="$attach"
      break
    fi
  else
    echo "$out"
  fi
done

c "VERDICT"
if [ -n "$PASS" ]; then
  ok "PASS — ATTACH-across-namespaces WORKS in self-hosted sqld."
  ok "Working syntax: ${PASS}"
  echo "=> Shared-catalog architecture (read-only catalog + per-tenant DB) survives a libSQL/sqld move."
else
  no "FAIL — no ATTACH form produced a cross-namespace join."
  echo "=> Shared-catalog/ATTACH design would need rework before any libSQL/sqld move."
  echo "   (See container logs below for the rejection reason.)"
  podman logs "$NAME" 2>&1 | tail -20
fi

[ "${KEEP:-0}" = "1" ] && echo && echo "KEEP=1: container '${NAME}' left running on ${BASE} / admin ${ADMIN}"
