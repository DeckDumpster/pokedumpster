#!/usr/bin/env bash
# Spike for the 8ch.2.1 GATE: can Litestream back up + restore a PLAIN SQLite DB
# to real S3 — i.e. achieve backup-first with ZERO DB-layer change (no libsql,
# no sqld, no cat.-qualify sweep, no async)? The standalone bottomless VFS is
# dead (crate stuck at 0.1.1), so Litestream is the viable lighter path.
#
# Creds from ~/.pkdump-s3-spike.env (assume-role, like the other S3 spikes).
set -euo pipefail

ENV_FILE="${S3_ENV_FILE:-$HOME/.pkdump-s3-spike.env}"
[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }
: "${S3_BUCKET:?}"; : "${S3_REGION:?}"; : "${S3_ROLE_ARN:?}"
: "${S3_ACCESS_KEY_ID:?}"; : "${S3_SECRET_ACCESS_KEY:?}"

LS="docker.io/litestream/litestream:latest"
AWSCLI="docker.io/amazon/aws-cli:latest"
PREFIX="litestream-spike"
REPLICA="s3://${S3_BUCKET}/${PREFIX}/collection"
WORK="$(mktemp -d)"; chmod 777 "$WORK"
DB="${WORK}/collection.db"

ok(){ printf '\033[1;32m%s\033[0m\n' "$*"; }; no(){ printf '\033[1;31m%s\033[0m\n' "$*"; }
c(){ printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }

EAK=""; ESK=""; ETOKEN=""
assume(){ local o; o=$(podman run --rm -e AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" -e AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" -e AWS_DEFAULT_REGION="$S3_REGION" \
    "$AWSCLI" sts assume-role --role-arn "$S3_ROLE_ARN" --role-session-name pkdump-ls --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' --output text 2>&1)
  read -r EAK ESK ETOKEN <<< "$o"; case "$EAK" in ASIA*) ok "assumed role (temp creds)";; *) no "assume failed: $o"; exit 1;; esac; }
AWS(){ podman run --rm -e AWS_ACCESS_KEY_ID="$EAK" -e AWS_SECRET_ACCESS_KEY="$ESK" -e AWS_SESSION_TOKEN="$ETOKEN" -e AWS_DEFAULT_REGION="$S3_REGION" "$AWSCLI" "$@"; }
# litestream runs as host-uid (--user 0:0 -> rootless maps to the invoking user) so files stay accessible.
LSRUN(){ podman run --rm --user 0:0 -v "${WORK}:/data" \
    -e AWS_ACCESS_KEY_ID="$EAK" -e AWS_SECRET_ACCESS_KEY="$ESK" -e AWS_SESSION_TOKEN="$ETOKEN" -e AWS_REGION="$S3_REGION" \
    "$LS" "$@"; }

cleanup(){ [ -n "${EAK:-}" ] && AWS s3 rm "s3://${S3_BUCKET}/" --recursive --exclude "*" --include "${PREFIX}/*" >/dev/null 2>&1 || true; rm -rf "$WORK"; }
trap '[ "${KEEP:-0}" = "1" ] || cleanup' EXIT

assume

c "Create a WAL-mode SQLite DB with 5 rows (host sqlite3 — the CURRENT stack, unchanged)"
sqlite3 "$DB" "PRAGMA journal_mode=WAL; \
  CREATE TABLE widgets(id INTEGER PRIMARY KEY, label TEXT); \
  INSERT INTO widgets VALUES (1,'alpha'),(2,'beta'),(3,'gamma'),(4,'delta'),(5,'epsilon');" >/dev/null
ok "5 rows in $(basename "$DB")"

cat > "${WORK}/litestream.yml" <<YML
dbs:
  - path: /data/collection.db
    replicas:
      - type: s3
        bucket: ${S3_BUCKET}
        path: ${PREFIX}/collection
        region: ${S3_REGION}
YML

c "Litestream replicate -once -> real S3 (region pinned -> no GetBucketLocation needed)"
LSRUN replicate -config /data/litestream.yml -once 2>&1 | sed 's/^/  /' | tail -8
ok "replicated"

c "Objects in S3 (proof):"
AWS s3 ls --recursive "s3://${S3_BUCKET}/${PREFIX}/" 2>&1 | sed 's/^/  /' | head -12

c "SIMULATE TOTAL LOSS: delete the DB (+ WAL/SHM)"
rm -f "$DB" "${DB}-wal" "${DB}-shm"
ok "local DB gone"

c "Litestream restore from S3"
LSRUN restore -config /data/litestream.yml -o "/data/restored.db" "/data/collection.db" 2>&1 | sed 's/^/  /' | tail -6

c "Verify restored DB (host sqlite3)"
OUT="$(sqlite3 "${WORK}/restored.db" "SELECT id || ' | ' || label FROM widgets ORDER BY id;" 2>&1 || echo "RESTORE-READ-FAILED")"
echo "$OUT" | sed 's/^/  /'
N="$(printf '%s\n' "$OUT" | grep -c ' | ' || true)"

c "VERDICT"
if printf '%s' "$OUT" | grep -q epsilon && [ "$N" = 5 ]; then
  ok "PASS — Litestream backed up a plain SQLite DB to real S3 and restored all 5 rows after total loss."
  echo "=> backup-first achievable with ZERO pkdump-db change (no libsql/sqld); 8ch.2 sweep+async NOT needed for phase 1."
  RC=0
else
  no "FAIL — restore did not return the rows (n=${N})."
  RC=1
fi
[ "${KEEP:-0}" = 1 ] && echo && echo "KEEP=1: workdir ${WORK} kept; S3 prefix left in place."
exit ${RC:-0}
