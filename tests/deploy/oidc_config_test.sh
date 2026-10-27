#!/usr/bin/env bash
# tests/deploy/oidc_config_test.sh — deploy/oidc-config.sh runs where the Terraform state is and
# writes the prod host's files over ssh (sp-v5f004).
#
# The prod host has no terraform and no access to the pkdump secrets, so the script runs on the
# operator box and pushes. A fake terraform/aws/ssh on PATH stand in; the fake ssh runs the
# remote command locally against a fake $HOME, with `mv` shadowed to log the rename.
#
#   §1  the cookie key is read from the REMOTE oidc.env and kept
#   §2  both remote files exist, mode 600, written by temp file + rename, no temp left behind
#   §3  no secret reaches stdout or stderr
#   §4  a first run (no remote oidc.env) generates a key
#   §5  no host argument is a refusal
# Hermetic: bash only.
set -uo pipefail
REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="${OIDC_CONFIG_SCRIPT:-$REPO_DIR/deploy/oidc-config.sh}"
FAILED=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1"; else echo "  FAIL $1 (expected '$2', got '$3')"; FAILED=1; fi; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
BIN="$TMP/bin"; RHOME="$TMP/remote-home"; SHADOW="$TMP/shadow"
mkdir -p "$BIN" "$RHOME/.config/pkdump/prod" "$SHADOW"
REAL_MV="$(command -v mv)"

cat >"$BIN/terraform" <<'T'
#!/usr/bin/env bash
case "$*" in *probe_secret_name*) echo pkdump/prod/probe;; *secret_name*) echo pkdump/prod/oidc;; *redirect_url*) echo https://poke.example/auth/callback;; esac
T
cat >"$BIN/aws" <<'T'
#!/usr/bin/env bash
case "$*" in
  *pkdump/prod/probe*) echo '{"PKDUMP_PROBE_CLIENT_ID":"probeid","PKDUMP_PROBE_CLIENT_SECRET":"PROBESECRET999"}';;
  *) echo '{"PKDUMP_OIDC_CLIENT_ID":"cid","PKDUMP_OIDC_CLIENT_SECRET":"CLIENTSECRET888"}';;
esac
T
cat >"$BIN/ssh" <<T
#!/usr/bin/env bash
while [ "\${1:-}" = "-o" ]; do shift 2; done
[ "\$1" = prodhost ] || { echo "unexpected host \$1" >&2; exit 255; }
shift
HOME="$RHOME" PATH="$SHADOW:\$PATH" exec bash -c "\$1"
T
cat >"$SHADOW/mv" <<T
#!/usr/bin/env bash
echo "\$*" >>"$TMP/mv.log"
exec "$REAL_MV" "\$@"
T
chmod +x "$BIN"/* "$SHADOW/mv"

run() { PATH="$BIN:$PATH" PKDUMP_OIDC_INFRA_DIR="$TMP/infra" bash "$SCRIPT" "$@" 2>&1; }
D="$RHOME/.config/pkdump/prod"
echo "PKDUMP_OIDC_COOKIE_KEY=keepmekeepme" >"$D/oidc.env"

OUT="$(run prod prodhost)"; RC=$?
check "run succeeds" 0 "$RC"
check "§1 cookie key kept from the remote file" 1 "$(grep -c '^PKDUMP_OIDC_COOKIE_KEY=keepmekeepme$' "$D/oidc.env")"
check "§2 oidc.env carries the client" 1 "$(grep -c '^PKDUMP_OIDC_CLIENT_SECRET=CLIENTSECRET888$' "$D/oidc.env")"
check "§2 probe.env carries the probe" 1 "$(grep -c '^PKDUMP_PROBE_CLIENT_SECRET=PROBESECRET999$' "$D/probe.env")"
check "§2 oidc.env mode" 600 "$(stat -c %a "$D/oidc.env" 2>/dev/null)"
check "§2 probe.env mode" 600 "$(stat -c %a "$D/probe.env" 2>/dev/null)"
check "§2 both written by rename from a temp file" 2 "$(grep -c "\.XXXXXX\|oidc.env\.[A-Za-z0-9]\{6\} .*oidc.env$\|probe.env\.[A-Za-z0-9]\{6\} .*probe.env$" "$TMP/mv.log" 2>/dev/null)"
check "§2 no temp file left" 2 "$(ls "$D" | wc -l)"
check "§3 no secret printed" 0 "$(grep -c 'CLIENTSECRET888\|PROBESECRET999\|keepmekeepme' <<<"$OUT")"

rm -f "$D"/*
run prod prodhost >/dev/null
KEY="$(sed -n 's/^PKDUMP_OIDC_COOKIE_KEY=//p' "$D/oidc.env")"
check "§4 first run generates a 64-hex key" 64 "${#KEY}"

run prod >/dev/null; check "§5 no host is refused" 1 "$?"
exit "$FAILED"
