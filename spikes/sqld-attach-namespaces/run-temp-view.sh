#!/usr/bin/env bash
# TEMP-VIEW spike driver (pokedumpster-5jv follow-up): starts sqld, runs the
# baton-threaded temp_view_spike.py scenarios, tears down. KEEP=1 to leave up.
set -euo pipefail

IMG="ghcr.io/tursodatabase/libsql-server:latest"
NAME="sqld-spike"
HTTP_PORT=18080
ADMIN_PORT=19090
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cleanup() { podman rm -f "$NAME" >/dev/null 2>&1 || true; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT
cleanup

printf '\n\033[1;36m== Start sqld (namespaces + admin API) ==\033[0m\n'
# entrypoint auto-appends --db-path/--http-listen-addr/--grpc-listen-addr; pass only the rest.
podman run -d --name "$NAME" \
  -p ${HTTP_PORT}:8080 -p ${ADMIN_PORT}:9090 "$IMG" \
  /bin/sqld --admin-listen-addr 0.0.0.0:9090 --enable-namespaces >/dev/null
for i in $(seq 1 40); do
  curl -fsS "http://127.0.0.1:${HTTP_PORT}/health" >/dev/null 2>&1 && { echo "up after ${i}s"; break; }
  [ "$i" = 40 ] && { echo "sqld never came up"; podman logs "$NAME" | tail -30; exit 1; }
  sleep 1
done

python3 "${HERE}/temp_view_spike.py" \
  --base "http://127.0.0.1:${HTTP_PORT}" --admin "http://127.0.0.1:${ADMIN_PORT}"
rc=$?

[ "${KEEP:-0}" = "1" ] && echo && echo "KEEP=1: container '${NAME}' left running."
exit $rc
