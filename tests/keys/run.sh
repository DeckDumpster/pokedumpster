#!/usr/bin/env bash
# Container-tier gate (pd-ulds): tenant-zone key custody, in the SHIPPED image,
# against a real data volume, through the wrapper a timer would run.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/keys/run.sh          # ~1min after the image is warm
#   KEEP=1 bash tests/keys/run.sh   # leave WORK in place
#
# ── WHY THIS EXISTS AND IS NOT JUST A RUST TEST ─────────────────────────────
# `crates/pkdump-keys` already proves the maths, the ordering and the
# separation, hermetically and in milliseconds. Two claims it CANNOT make:
#
#   1. **The file is mode 600 where it is deployed.** "The code sets 600" and
#      "the file on the box is 600" are different claims, and only the second
#      one protects anything — a umask, a volume mount, a container's user
#      mapping or a later `chmod` all sit between them. So this gate stats the
#      key file on the HOST, after the shipped binary in the shipped image
#      wrote it.
#   2. **The wrapper keeps the two paths apart.** deploy/keys.sh is the one
#      layer that could quietly undo the separation the Rust side is held to,
#      by mounting everything for everything. §7 asserts it does not.
#
# ── WHAT IT ASSERTS ─────────────────────────────────────────────────────────
#   §2 `keys init` writes a key, at mode 600, on the host, and refuses to
#      overwrite it — the one irreversible act is never implicit.
#   §3 Derivation is DETERMINISTIC (same id, same fingerprint, every time) and
#      DISTINCT (different ids, different fingerprints — over a real set).
#   §4 Absence is not permission: an unregistered id REFUSES.
#   §5 A tombstone REFUSES, clearly, and by a different route than a missing
#      key does — and it refuses even with the master key perfectly healthy.
#   §6 THE CRUX: with the master key moved away, a tombstoned tenant still
#      reads as REVOKED while a live one reads as an OPERATIONAL failure. A
#      lost key never reports as a deleted tenant, on a real box.
#   §7 The wrapper's mounts: `backup` never sees the data volume (where the
#      tombstones are), `tombstone` never sees the key file.
#   §8 The backup path round-trips: a restored key derives what it did before,
#      and lifts no tombstone.
#
# Prod-safe: its own image tag, temp directory and host-config directory, and a
# data volume that is a HOST PATH under $WORK rather than a podman volume. It
# touches no pkdump-* unit, no pkdump-*-data volume, no real ~/.config/pkdump.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES="${REPO_DIR}/tests/ui/fixtures"

# shellcheck source=deploy/image-lib.sh
. "${REPO_DIR}/deploy/image-lib.sh"

# PER-CHECKOUT, for the reason every other gate derives its names the same way:
# concurrent polecats run whole suites from their own worktrees.
SUFFIX="${PDK_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-8)}"
IMAGE="localhost/pkdump:keys-${SUFFIX}"
INSTANCE="keys-${SUFFIX}"

WORK=${WORK:-$(mktemp -d /tmp/pd-keys.XXXXXX)}
DATA="$WORK/data"
# The wrapper reads ~/.config/pkdump/<instance>/, so HOME is redirected at it
# rather than the real one being written to.
FAKE_HOME="$WORK/home"
CONF_DIR="${FAKE_HOME}/.config/pkdump/${INSTANCE}"
KEY_FILE="${CONF_DIR}/tenant-master.key"

pass=0
fail=0
check() { # check <label> <expected> <actual>
	if [[ "$2" == "$3" ]]; then
		echo "  PASS  $1 (= $3)"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 (expected $2, got $3)"
		fail=$((fail + 1))
	fi
}
check_contains() { # check_contains <label> <needle> <haystack>
	if [[ "$3" == *"$2"* ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 — expected to find '$2' in:"
		printf '%s\n' "$3" | sed 's/^/          /'
		fail=$((fail + 1))
	fi
}
check_absent() { # check_absent <label> <needle> <haystack>
	if [[ "$3" != *"$2"* ]]; then
		echo "  PASS  $1"
		pass=$((pass + 1))
	else
		echo "  FAIL  $1 — '$2' should NOT appear in:"
		printf '%s\n' "$3" | sed 's/^/          /'
		fail=$((fail + 1))
	fi
}
log() { printf '\n=== %s ===\n' "$*"; }

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
	if [[ -n "${KEEP:-}" ]]; then
		echo
		echo "KEEP=1 — leaving WORK=$WORK in place."
		return
	fi
	rm -rf "$WORK"
}
trap cleanup EXIT

# The wrapper an operator (and, later, a unit) actually runs — with HOME, the
# image and the data directory redirected at this gate's throwaway copies. The
# data "volume" is a host path, which `-v` takes just as happily as a name.
keys() { # keys <subcommand> [args...]
	HOME="$FAKE_HOME" \
		PKDUMP_KEYS_IMAGE="$IMAGE" PKDUMP_KEYS_DATA="$DATA" \
		bash "${REPO_DIR}/deploy/keys.sh" "$INSTANCE" "$@"
}
# …and the same, capturing failure instead of exiting. `set -e` plus a command
# substitution in an assignment is how a gate turns an expected refusal into a
# silent early exit.
keys_out() { keys "$@" 2>&1 || true; }
# `|| rc=$?` rather than a bare call: under `set -e` the bare form aborts the
# command substitution before the echo, so an EXPECTED refusal would reach
# `check` as an empty string instead of its status.
keys_rc() {
	local rc=0
	keys "$@" >/dev/null 2>&1 || rc=$?
	echo "$rc"
}

# The fingerprint out of `keys derive`, which prints
# "<database_id> key fingerprint <hex>" and never the key.
fingerprint() { # fingerprint <database-id>
	keys derive "$1" 2>/dev/null | awk '/key fingerprint/ { print $NF }'
}

A_ID=""
B_ID=""

log "1. the shipped image, and a data directory with two provisioned tenants"
pkdump_image_ensure "$IMAGE" "$REPO_DIR" >/dev/null
echo "  $IMAGE"
mkdir -p "$DATA" "$CONF_DIR"
cp "${FIXTURES}/shared.sqlite" "${DATA}/shared.sqlite"
pkdump() { podman run --rm -v "${DATA}:/data:Z" --entrypoint pkdump "$IMAGE" "$@"; }
pkdump tenant create alice >/dev/null
pkdump tenant create bob >/dev/null
A_ID="$(pkdump tenant list | awk '$1 == "alice" { print $2 }')"
B_ID="$(pkdump tenant list | awk '$1 == "bob" { print $2 }')"
if [[ -z "$A_ID" || -z "$B_ID" || "$A_ID" == "$B_ID" ]]; then
	echo "  ABORT: could not read two distinct database ids from \`pkdump tenant list\`:"
	pkdump tenant list | sed 's/^/    /'
	exit 1
fi
echo "  alice -> $A_ID"
echo "  bob   -> $B_ID"

# ---------------------------------------------------------------------------
log "2. keys init — the key lands, at mode 600, ON THE HOST"

check "there is no key before init" "absent" "$([[ -e "$KEY_FILE" ]] && echo present || echo absent)"
INIT_OUT="$(keys_out init)"
check "init succeeded" "yes" "$([[ -e "$KEY_FILE" ]] && echo yes || echo no)"
# THE assertion this gate exists for: stat the real file, on the host, after
# the shipped binary in the shipped image wrote it.
check "the deployed key file is mode 600" "600" "$(stat -c '%a' "$KEY_FILE")"
check "its directory is mode 700" "700" "$(stat -c '%a' "$CONF_DIR")"
check_contains "init says to back it up, naming the path" "keys backup --yes" "$INIT_OUT"
check_contains "…and says a lost key is not a deletion" "must never be treated" "$INIT_OUT"
check_contains "the wrapper verified the mode on the host too" "is mode 600 on the host" "$INIT_OUT"

FIRST_FP="$(keys status 2>/dev/null | awk '/fingerprint/ { print $NF }')"
check "status reports a fingerprint" "16" "${#FIRST_FP}"

# The one irreversible act is never implicit — and a refused init leaves the
# existing key exactly as it was.
REINIT="$(keys_out init)"
check_contains "a second init REFUSES" "refusing to overwrite" "$REINIT"
check_contains "…and says what overwriting would cost" "destroys EVERY tenant" "$REINIT"
check "the key is unchanged after the refusal" "$FIRST_FP" \
	"$(keys status 2>/dev/null | awk '/fingerprint/ { print $NF }')"

# ---------------------------------------------------------------------------
log "3. derivation is deterministic, and distinct per database_id"

keys register "$A_ID" >/dev/null
keys register "$B_ID" >/dev/null

A_FP="$(fingerprint "$A_ID")"
check "alice's key has a fingerprint" "16" "${#A_FP}"
check "the same id derives the same key (2nd)" "$A_FP" "$(fingerprint "$A_ID")"
check "the same id derives the same key (3rd)" "$A_FP" "$(fingerprint "$A_ID")"

B_FP="$(fingerprint "$B_ID")"
check "a different id derives a DIFFERENT key" "different" \
	"$([[ "$A_FP" != "$B_FP" ]] && echo different || echo same)"

# A real set rather than two: register a handful more and assert no collisions.
COLLIDE_IDS=()
for n in 1 2 3 4 5 6; do
	id="01J00000000000000000000${n}0"
	keys register "$id" >/dev/null
	COLLIDE_IDS+=("$id")
done
FPS="$(
	for id in "${COLLIDE_IDS[@]}"; do fingerprint "$id"; done
)"
check "no two database_ids in the set collide" "${#COLLIDE_IDS[@]}" \
	"$(printf '%s\n' "$FPS" | sort -u | grep -c .)"

check "derive never prints key material" "" \
	"$(keys derive "$A_ID" 2>&1 | grep -Eo '[0-9a-f]{64}' || true)"

# ---------------------------------------------------------------------------
log "4. absence is not permission — an unregistered id refuses"

UNREG="01J0000000000000000000ZZZZ"
OUT="$(keys_out derive "$UNREG")"
check "an unregistered id is refused" "1" "$(keys_rc derive "$UNREG")"
check_contains "…as NOT REGISTERED, not as a revocation" "no key state is registered" "$OUT"
check_contains "…and it warns about a restored-empty registry" "missing its tombstones" "$OUT"
check_absent "…and it does NOT call it revoked" "REVOKED" "$OUT"

# ---------------------------------------------------------------------------
log "5. THE DESTRUCTION PATH — a tombstone refuses, with the key healthy"

check "tombstone refuses without --yes" "1" "$(keys_rc tombstone "$A_ID")"
check "…and alice still derives" "$A_FP" "$(fingerprint "$A_ID")"

TOMB="$(keys_out tombstone "$A_ID" --yes --reason "account deleted")"
check_contains "tombstone reports the revocation" "key REVOKED at" "$TOMB"
check_contains "…and says the master key was not touched" "master key itself was not touched" "$TOMB"
check "the master key is still there" "present" \
	"$([[ -e "$KEY_FILE" ]] && echo present || echo absent)"

OUT="$(keys_out derive "$A_ID")"
check "a tombstoned id is refused" "1" "$(keys_rc derive "$A_ID")"
check_contains "…as a deliberate REVOCATION" "was REVOKED at" "$OUT"
check_contains "…carrying the reason recorded at the time" "account deleted" "$OUT"
check_contains "…and saying explicitly it is not a missing key" "not a missing key" "$OUT"
check "bob is untouched by alice's revocation" "$B_FP" "$(fingerprint "$B_ID")"
check "a tombstone cannot be lifted by re-registering" "1" "$(keys_rc register "$A_ID")"

# ---------------------------------------------------------------------------
log "6. THE CRUX — with the master key gone, revoked still reads as revoked"

# Moved aside rather than deleted: this is a gate, and §8 needs the key back.
mv "$KEY_FILE" "$WORK/key-moved-aside"

REVOKED="$(keys_out derive "$A_ID")"
check_contains "the revoked tenant STILL reads as revoked" "was REVOKED at" "$REVOKED"
check_absent "…and not as a missing key" "master key is unavailable" "$REVOKED"

LIVE="$(keys_out derive "$B_ID")"
check_contains "the live tenant reads as an OPERATIONAL failure" "OPERATIONAL FAILURE" "$LIVE"
check_contains "…naming the key file" "master key is unavailable" "$LIVE"
check_absent "…and NEVER as a deletion" "REVOKED" "$LIVE"
check_contains "…and it says so in as many words" "not a deletion" "$LIVE"

# The destruction path still works with no key on the box at all — it is a row,
# not a file.
check "tombstoning works with no master key present" "0" \
	"$(keys_rc tombstone "${COLLIDE_IDS[0]}" --yes --reason "no key on this box")"
check "…and the backup path is the one that cannot run" "1" "$(keys_rc backup --yes)"

mv "$WORK/key-moved-aside" "$KEY_FILE"

# ---------------------------------------------------------------------------
log "7. the wrapper keeps the two paths apart"

# The rule the Rust side is held to (crates/pkdump-keys/tests/separation.rs),
# carried through the one layer that could quietly undo it.
WRAPPER="$(sed 's|#.*||' "${REPO_DIR}/deploy/keys.sh")"
check "backup is excluded from the data volume mount" "1" \
	"$(printf '%s\n' "$WRAPPER" | grep -c '^backup) ;;')"
check "tombstone is in the branch that mounts no key file" "1" \
	"$(printf '%s\n' "$WRAPPER" | grep -c 'tombstone | register | list)')"

# …and behaviourally, which is what actually matters: run each with the OTHER
# one's world removed and confirm it is unaffected.
mv "$DATA" "$WORK/data-moved-aside"
mkdir -p "$WORK/out"
check "backup runs with the registry volume gone" "0" "$(keys_rc backup -o "$WORK/out/b1.key")"
mv "$WORK/data-moved-aside" "$DATA"

mv "$KEY_FILE" "$WORK/key-moved-aside"
check "tombstone runs with the key gone" "0" \
	"$(keys_rc tombstone "${COLLIDE_IDS[1]}" --yes --reason "still works")"
mv "$WORK/key-moved-aside" "$KEY_FILE"

# ---------------------------------------------------------------------------
log "8. THE BACKUP PATH round-trips, and lifts nothing"

keys backup -o "$WORK/out/backup.key" >/dev/null
check "the staged backup is mode 600" "600" "$(stat -c '%a' "$WORK/out/backup.key")"
check "a second backup to the same file REFUSES" "1" "$(keys_rc backup -o "$WORK/out/backup.key")"
check "printing refuses without --yes" "1" "$(keys_rc backup)"

# A rebuilt box: the same registry, the key pasted back from the "password
# manager". Restore refuses over a live key, so the live one goes first.
rm "$KEY_FILE"
# The staged copy is on the host, not in the container, so it goes over stdin —
# which is how deploy/KEYS.md documents a paste anyway.
RESTORE="$(HOME="$FAKE_HOME" PKDUMP_KEYS_IMAGE="$IMAGE" PKDUMP_KEYS_DATA="$DATA" \
	bash "${REPO_DIR}/deploy/keys.sh" "$INSTANCE" restore <"$WORK/out/backup.key" 2>&1)"
check_contains "the restore reports a fingerprint" "fingerprint" "$RESTORE"
check "the restored key is mode 600 on the host" "600" "$(stat -c '%a' "$KEY_FILE")"
check "…and it is THE key: bob derives what he did before" "$B_FP" "$(fingerprint "$B_ID")"

OUT="$(keys_out derive "$A_ID")"
check_contains "…and the restore lifted NO tombstone" "was REVOKED at" "$OUT"

LIST="$(keys_out list)"
check_contains "list shows the revocation" "tombstoned" "$LIST"
check_contains "…with its reason" "account deleted" "$LIST"

# ---------------------------------------------------------------------------
log "RESULT"
echo "  ${pass} passed, ${fail} failed"
[[ $fail -eq 0 ]] || exit 1
echo "  PASS — the deployed key file is 600, derivation is deterministic and"
echo "         distinct, a tombstone refuses permanently, and a LOST key never"
echo "         reads as a DELETED tenant."
