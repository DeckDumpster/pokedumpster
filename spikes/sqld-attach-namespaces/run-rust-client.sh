#!/usr/bin/env bash
# Rust libsql client spike (pokedumpster-5jv follow-up #2): starts sqld
# (dual-stack so tenant1.localhost -> ::1 reaches it), seeds catalog+tenant1,
# then runs the real libsql Rust client. KEEP=1 leaves the container up.
set -euo pipefail

IMG="ghcr.io/tursodatabase/libsql-server:latest"
NAME="sqld-spike"
HTTP_PORT=18080
ADMIN_PORT=19090
BASE="http://127.0.0.1:${HTTP_PORT}"
ADMIN="http://127.0.0.1:${ADMIN_PORT}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="python3 ${HERE}/probe.py --base ${BASE}"

ok() { printf '\033[1;32m%s\033[0m\n' "$*"; }
c()  { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
cleanup() { podman rm -f "$NAME" >/dev/null 2>&1 || true; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup

c "Start sqld (dual-stack v4+v6 so tenant1.localhost is reachable)"
podman run -d --name "$NAME" \
  -p 127.0.0.1:${HTTP_PORT}:8080 -p "[::1]:${HTTP_PORT}:8080" \
  -p 127.0.0.1:${ADMIN_PORT}:9090 \
  "$IMG" /bin/sqld --admin-listen-addr 0.0.0.0:9090 --enable-namespaces >/dev/null
for i in $(seq 1 40); do
  curl -fsS "${BASE}/health" >/dev/null 2>&1 && { ok "up after ${i}s"; break; }
  [ "$i" = 40 ] && { echo "sqld never came up"; podman logs "$NAME" | tail -30; exit 1; }
  sleep 1
done

c "Create + seed namespaces"
curl -fsS -X POST "${ADMIN}/v1/namespaces/catalog/create" -H 'content-type: application/json' -d '{}' >/dev/null && ok "ns catalog"
curl -fsS -X POST "${ADMIN}/v1/namespaces/tenant1/create" -H 'content-type: application/json' -d '{}' >/dev/null && ok "ns tenant1"
# allow_attach=true on catalog (full-object GET-patch-POST)
curl -fsS "${ADMIN}/v1/namespaces/catalog/config" \
  | python3 -c 'import json,sys;c=json.load(sys.stdin);c["allow_attach"]=True;print(json.dumps(c))' \
  | curl -fsS -X POST "${ADMIN}/v1/namespaces/catalog/config" -H 'content-type: application/json' --data @- >/dev/null \
  && ok "allow_attach=true on catalog"
$PROBE --host catalog.localhost \
  --sql "CREATE TABLE IF NOT EXISTS cards(id INTEGER PRIMARY KEY, name TEXT)" \
  --sql "DELETE FROM cards" \
  --sql "INSERT INTO cards(id,name) VALUES (1,'Pikachu'),(2,'Charizard'),(3,'Mew')" >/dev/null && ok "seeded catalog"
$PROBE --host tenant1.localhost \
  --sql "CREATE TABLE IF NOT EXISTS collection(id INTEGER PRIMARY KEY, card_id INTEGER, condition TEXT)" \
  --sql "DELETE FROM collection" \
  --sql "INSERT INTO collection(id,card_id,condition) VALUES (10,2,'NM'),(11,3,'LP'),(12,2,'MP')" >/dev/null && ok "seeded tenant1"

c "Run the Rust libsql client (cargo build may take a minute)"
( cd "${HERE}/rust-client" && cargo run --quiet )
rc=$?

[ "${KEEP:-0}" = "1" ] && echo && echo "KEEP=1: container '${NAME}' left running."
exit $rc
