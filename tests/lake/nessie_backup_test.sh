#!/usr/bin/env bash
# sp-87nj2v: hermetic gate for the Nessie backup — the Python logic (stale /
# corrupt / drill-failure positive controls) and the shell wiring. No podman.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
fail=0
python3 -I "$ROOT/tests/lake/nessie_backup_test.py" || fail=1

bash -n "$ROOT/deploy/nessie-backup.sh" || fail=1
# Every unit installed, declared host-wide, and torn down.
for u in nessie-backup nessie-backup-check; do
    for ext in service timer; do
        [ -f "$ROOT/deploy/pkdump-$u.$ext" ] || { echo "missing pkdump-$u.$ext" >&2; fail=1; }
        grep -q "pkdump-$u@.$ext" "$ROOT/deploy/units-lib.sh" || { echo "units-lib lacks $u@.$ext" >&2; fail=1; }
    done
    grep -q "pkdump-$u@\${INSTANCE}" "$ROOT/deploy/teardown.sh" || { echo "teardown lacks $u" >&2; fail=1; }
done
# No default backup location: the wrapper must refuse without one.
HOME_T="$(mktemp -d)"; trap 'rm -rf "$HOME_T"' EXIT
: >"$HOME_T/lake.env"
out="$(HOME="$HOME_T" PKDUMP_LAKE_ENV="$HOME_T/lake.env" bash "$ROOT/deploy/nessie-backup.sh" x check 2>&1 || true)"
case "$out" in *"PKDUMP_NESSIE_BACKUP_URL is not set"*) ;; *) echo "no refusal without a backup URL: $out" >&2; fail=1 ;; esac

# sp-77eatm: the backup signs with its OWN credential; the lake read keeps the lake's.
# A fake podman records every `run`; `check` is a store mode, `inventory` a lake read.
mkdir -p "$HOME_T/bin" "$HOME_T/home/.config/pkdump"
cat >"$HOME_T/bin/podman" <<'FAKE'
#!/usr/bin/env bash
[ "$1" = run ] && printf '%s\n' "$*" >>"$FAKE_LOG"
exit 0
FAKE
printf '#!/usr/bin/env bash\nexit 0\n' >"$HOME_T/bin/systemctl"
chmod +x "$HOME_T/bin/podman" "$HOME_T/bin/systemctl"
printf '[pkdump-lake-backup]\naws_access_key_id=BACKUPKEY\naws_secret_access_key=x\n' >"$HOME_T/backup.credentials"
printf '[Container]\nImage=ghcr.io/projectnessie/nessie:0.104.0\nVolume=%%h/n:/nessie:Z\n' >"$HOME_T/n.container"
cat >"$HOME_T/lake2.env" <<ENV
PKDUMP_NESSIE_BACKUP_URL=s3://backups-bucket/nessie
PKDUMP_LAKE_S3_BUCKET=lake-bucket
AWS_PROFILE=LAKEPROFILE
PKDUMP_LAKE_S3_ROLE_ARN=arn:LAKEROLE
PKDUMP_NESSIE_BACKUP_CREDENTIALS_FILE=$HOME_T/backup.credentials
PKDUMP_NESSIE_BACKUP_PROFILE=pkdump-lake-backup
ENV
export FAKE_LOG="$HOME_T/run.log"; : >"$FAKE_LOG"
run_mode() { PATH="$HOME_T/bin:$PATH" HOME="$HOME_T/home" TMPDIR="$HOME_T" PKDUMP_LAKE_ENV="$HOME_T/lake2.env" \
    PKDUMP_ALERTS_ENV=/nonexistent PKDUMP_NESSIE_QUADLET="$HOME_T/n.container" \
    bash "$ROOT/deploy/nessie-backup.sh" x "$1" >/dev/null 2>&1 || true; }
run_mode check
store_run="$(grep -E 'nessie-backup .* check( |$)' "$FAKE_LOG" || true)"
case "$store_run" in *"AWS_PROFILE=pkdump-lake-backup"*"/backup-aws/credentials:ro"*) ;; *) echo "backup job lacks its own credential: $store_run" >&2; fail=1 ;; esac
case "$store_run" in *LAKEPROFILE*|*LAKEROLE*|*lake-bucket*) echo "backup job carries the lake identity: $store_run" >&2; fail=1 ;; esac
# the lake read keeps the lake identity and never sees the backup credential
PATH="$HOME_T/bin:$PATH" HOME="$HOME_T/home" TMPDIR="$HOME_T" PKDUMP_LAKE_ENV="$HOME_T/lake2.env" PKDUMP_ALERTS_ENV=/nonexistent \
    PKDUMP_NESSIE_QUADLET="$HOME_T/n.container" bash "$ROOT/deploy/nessie-backup.sh" x backup >/dev/null 2>&1 || true
lake_run="$(grep -E 'nessie-backup .* inventory( |$)' "$FAKE_LOG" || true)"
case "$lake_run" in *"AWS_PROFILE=LAKEPROFILE"*) ;; *) echo "lake read lost the lake identity: $lake_run" >&2; fail=1 ;; esac
case "$lake_run" in *pkdump-lake-backup*|*backup.credentials*) echo "lake read carries the backup credential: $lake_run" >&2; fail=1 ;; esac
exit "$fail"
