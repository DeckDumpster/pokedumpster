#!/usr/bin/env bash
# Container-tier gate (pd-uz8q): the TENANT ZONE is governed, not just described.
#
# Run by deploy/ci.sh. Standalone:
#   bash tests/lake/tenant_zone.sh          # ~1min, full run + teardown
#   KEEP=1 bash tests/lake/tenant_zone.sh   # leave MinIO + WORK up for poking
#
# ── WHAT THIS EXISTS TO CATCH ───────────────────────────────────────────────
#
# The tenant zone (`tenant/`) and the catalog zone (`raw/`, `lake/`) share one
# bucket. The ONLY thing separating them is a pair of credential policies and a
# lifecycle rule — and a policy is one misconfiguration away from being nothing
# at all, while looking exactly the same from the outside. Everything below is
# arranged around that:
#
#   * §4-§5 assert the boundary in BOTH directions: catalog credentials cannot
#     reach `tenant/`, tenant credentials cannot reach `raw/` or `lake/`. Read,
#     write and list, each separately, because a policy that forgets one of the
#     three is the normal way this goes wrong.
#   * §6 IS THE POINT OF THE GATE. It deliberately breaks the boundary —
#     replaces each credential in turn with a whole-bucket grant — and re-runs
#     the SAME assertion functions §5 ran, asserting they go RED. A boundary
#     check that has only ever been seen passing is not known to check
#     anything; three times in one day this repo shipped a gate that passed
#     because its subject never ran. The assertions are functions precisely so
#     the red run cannot be a differently-worded version of the green one.
#   * §6b is what writing §6 wrongly turned up, and it is worth more than the
#     mistake cost: a whole-bucket grant attached BESIDE either policy does not
#     open the boundary, because an explicit Deny beats any Allow. So a later
#     broad grant made somewhere else cannot silently widen either zone — which
#     is the property that makes these two documents safe to live with, and it
#     is asserted rather than assumed.
#   * §2 does the same for retention, and it asserts the EXIT CODE rather than
#     "non-zero", because the check has three different answers and they are
#     not interchangeable (pd-2hnp). Three ways of getting the rule wrong are
#     applied in turn (whole-bucket, wrong number of days, a prefix reaching
#     the catalog) and each must come back 1; the rule is then DELETED and must
#     come back 3; and finally the correct rule is read by a real credential
#     that MinIO really denies, which must come back 4. "There is no retention
#     rule" and "I am not allowed to look" are opposite facts, and this script
#     printed one sentence for both — on a check whose subject is a rule that
#     deletes tenant data after 90 days, that is worse than no check, because
#     it is trusted. A whole-bucket expiry would silently start deleting
#     `raw/`, whose retention is INDEFINITE by decision.
#   * §7 asserts the zone is still EMPTY of tenant data. This item builds the
#     governance, not the data flow; a fixture holding that got left behind
#     would be real tenant-shaped data sitting in a zone whose shipper does not
#     exist yet.
#   * §8 is the drift guard. The prefixes and the retention live in three
#     places that cannot share code — Rust (the shipper reads them at runtime),
#     the policy documents (AWS reads them), and the deploy script (bash). A
#     prefix changed in one and not the others silently widens a policy.
#
# The policy documents are applied VERBATIM, bucket substituted, by the real
# deploy/setup-tenant-zone.sh. An IAM policy document is the same dialect for
# AWS and for MinIO, which is the only reason this separation can be tested at
# all rather than trusted.
#
# Prod-safe: its own podman network, its own MinIO, its own temp dir, its own
# bucket name. Touches no pkdump-* unit, no pkdump-*-data volume, no real S3
# bucket and no tenant database. **No real tenant data anywhere** — every byte
# it writes under `tenant/` is a literal governance probe, and §7 removes it.
set -euo pipefail

MINIO_IMAGE=${MINIO_IMAGE:-docker.io/minio/minio:latest}
MC_IMAGE=${MC_IMAGE:-docker.io/minio/mc:latest}
AWSCLI_IMAGE=${AWSCLI_IMAGE:-docker.io/amazon/aws-cli:latest}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=tests/lib/diagnostics.sh
. "${REPO_DIR}/tests/lib/diagnostics.sh"
diag_init

die() {
	diag "!! $*"
	exit 1
}

# shellcheck source=tests/lib/ports.sh
. "${REPO_DIR}/tests/lib/ports.sh"
# shellcheck source=tests/lib/wait.sh
. "${REPO_DIR}/tests/lib/wait.sh"
# shellcheck source=deploy/store-lib.sh
. "${REPO_DIR}/deploy/store-lib.sh"
pkdump_store_load_config
pkdump_store_activate

# Unique per checkout — deploy/ci.sh runs gates in parallel and several polecats
# run whole suites beside each other from their own worktrees.
SUFFIX="${PDTZ_SUFFIX:-$(printf '%s' "$REPO_DIR" | sha1sum | cut -c1-6)}"
NET=pdtz-test-net-${SUFFIX}
MINIO_CTR=pdtz-test-minio-${SUFFIX}
BUCKET=pdtz-test-${SUFFIX}
MINIO_PORT=${MINIO_PORT:-$(free_port)}

ROOT_AK=pdtztestroot
ROOT_SK=pdtztestsecret123

# The two identities the whole gate is about. Distinct keys, so a mix-up shows
# up as a wrong answer rather than as an accidentally-correct one.
CAT_AK=pdtzcatalog
CAT_SK=pdtzcatalogsecret123
TEN_AK=pdtztenant
TEN_SK=pdtztenantsecret123

WORK=${WORK:-$(mktemp -d /tmp/pdtz-test.XXXXXX)}
mkdir -p "$WORK/minio" "$WORK/policies"
chmod 777 "$WORK/minio" "$WORK/policies"

cleanup() {
	if [ -n "${KEEP:-}" ]; then
		echo ""
		echo "KEEP=1 — leaving everything up:"
		echo "  MinIO   http://localhost:${MINIO_PORT}  (${ROOT_AK} / ${ROOT_SK})"
		echo "  bucket  ${BUCKET}"
		echo "  work    ${WORK}"
		echo "  tear down: podman rm -f ${MINIO_CTR}; podman network rm ${NET}"
		return
	fi
	podman rm -f "$MINIO_CTR" >/dev/null 2>&1 || true
	podman network rm "$NET" >/dev/null 2>&1 || true
	podman unshare rm -rf "$WORK" 2>/dev/null || rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

# mc as an arbitrary identity. The alias is always `x`, so every call site
# reads the same whichever credential it is holding — the point of §6 is that
# the SAME command answers differently, and a differently-spelled command would
# undermine that.
mc_as() {
	local ak="$1" sk="$2"
	shift 2
	podman run --rm --network "$NET" \
		-v "$WORK/policies:/policies:ro,Z" \
		-e "MC_HOST_x=http://${ak}:${sk}@${MINIO_CTR}:9000" \
		"$MC_IMAGE" "$@"
}
mc_root() { mc_as "$ROOT_AK" "$ROOT_SK" "$@"; }

# The aws CLI, containerised, so this gate does not depend on the host having
# one installed. deploy/setup-tenant-zone.sh takes the whole command as an
# override for exactly this reason, and passes every JSON document inline
# rather than as file:// so a container wrapper can work at all.
AWS_WRAPPER="podman run --rm --network ${NET} -e AWS_ACCESS_KEY_ID=${ROOT_AK} -e AWS_SECRET_ACCESS_KEY=${ROOT_SK} -e AWS_DEFAULT_REGION=us-west-2 ${AWSCLI_IMAGE}"
MINIO_URL="http://${MINIO_CTR}:9000"

zone_script() {
	PKDUMP_AWS="$AWS_WRAPPER" PKDUMP_LAKE_ENV="${WORK}/lake.env" \
		bash "${REPO_DIR}/deploy/setup-tenant-zone.sh" \
		--bucket "$BUCKET" --endpoint "$MINIO_URL" "$@"
}

# The same script as some OTHER identity. §2's forbidden case needs a real
# credential that MinIO really refuses — a denial this gate invented as a
# string would prove nothing about how the script classifies a real one.
zone_script_as() {
	local ak="$1" sk="$2"
	shift 2
	PKDUMP_AWS="podman run --rm --network ${NET} -e AWS_ACCESS_KEY_ID=${ak} -e AWS_SECRET_ACCESS_KEY=${sk} -e AWS_DEFAULT_REGION=us-west-2 ${AWSCLI_IMAGE}" \
		PKDUMP_LAKE_ENV="${WORK}/lake.env" \
		bash "${REPO_DIR}/deploy/setup-tenant-zone.sh" \
		--bucket "$BUCKET" --endpoint "$MINIO_URL" "$@"
}

# ---------------------------------------------------------------------------
echo "==> §0  Object store, and a catalog zone with real bytes in it"
# The deny assertions below have to fail because they are DENIED, never because
# the object is not there — a gate that passes on a missing object is the
# "never ran" failure wearing a green tick. So every prefix gets a real object,
# placed by root, and §4 proves each is readable by the identity that should
# see it before §5 proves it is not readable by the one that should not.
podman network create "$NET" >/dev/null
podman run -d --name "$MINIO_CTR" --network "$NET" \
	-p "127.0.0.1:${MINIO_PORT}:9000" \
	-e MINIO_ROOT_USER="$ROOT_AK" -e MINIO_ROOT_PASSWORD="$ROOT_SK" \
	-v "$WORK/minio:/data:Z" \
	"$MINIO_IMAGE" server /data >/dev/null

minio_live() { curl -sf -o /dev/null "http://localhost:${MINIO_PORT}/minio/health/live"; }
wait_until 30 0.25 minio_live || true
minio_live || die "MinIO never became healthy on ${MINIO_PORT}"

mc_root mb "x/${BUCKET}" >/dev/null

seed() {
	printf '%s' "$2" | mc_root pipe "x/${BUCKET}/$1" >/dev/null
}
# Catalog zone: shaped like the real thing, holding nothing that is anybody's.
seed "raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-13/run=01TEST/part-0000.json" \
	'{"catalog":"a landed upstream response"}'
seed "lake/catalog/prices/metadata/v1.metadata.json" \
	'{"catalog":"an iceberg table metadata file"}'
echo "    bucket ${BUCKET}: catalog zone seeded under raw/ and lake/"

# ---------------------------------------------------------------------------
echo "==> §1  Retention applied by the real deploy/setup-tenant-zone.sh"
zone_script --apply >"${WORK}/apply.log" 2>&1 ||
	die "setup-tenant-zone.sh --apply failed: $(cat "${WORK}/apply.log")"
grep -q "expires after 90 days" "${WORK}/apply.log" ||
	die "--apply did not report the 90-day rule: $(cat "${WORK}/apply.log")"
zone_script --check >/dev/null 2>&1 ||
	die "--check disagreed with the rule --apply had just written"
echo "    ok   tenant/ expires after 90 days, and the script verified its own write"

echo "==> §1b The script refuses rather than guessing a bucket"
# Cheap, hermetic, and the refusal that matters most: this script's output is a
# rule that DELETES OBJECTS. Every way of pointing it somewhere unintended is a
# stop, not a default.
mkdir -p "${WORK}/emptycfg"
: >"${WORK}/emptycfg/lake.env"
NO_BUCKET_RC=0
NO_BUCKET_OUT=$(PKDUMP_LAKE_ENV="${WORK}/emptycfg/lake.env" HOME="${WORK}/nohome" \
	bash "${REPO_DIR}/deploy/setup-tenant-zone.sh" --check 2>&1) || NO_BUCKET_RC=$?
[ "$NO_BUCKET_RC" -ne 0 ] || die "the script accepted an unconfigured bucket"
case "$NO_BUCKET_OUT" in
*lake.env*) echo "    ok   no bucket -> refuses, and names lake.env" ;;
*) die "refused without naming lake.env: ${NO_BUCKET_OUT}" ;;
esac

# The Litestream backup bucket holds the only irreplaceable data in the system.
mkdir -p "${WORK}/nohome/.config/pkdump/someinstance"
printf 'LITESTREAM_S3_BUCKET=%s\n' "$BUCKET" \
	>"${WORK}/nohome/.config/pkdump/someinstance/litestream.env"
BACKUP_RC=0
BACKUP_OUT=$(HOME="${WORK}/nohome" PKDUMP_LAKE_ENV="${WORK}/emptycfg/lake.env" \
	bash "${REPO_DIR}/deploy/setup-tenant-zone.sh" --check --bucket "$BUCKET" 2>&1) || BACKUP_RC=$?
[ "$BACKUP_RC" -ne 0 ] || die "the script accepted the Litestream backup bucket"
case "$BACKUP_OUT" in
*"backup"*) echo "    ok   the backup bucket -> refuses" ;;
*) die "refused for the wrong reason: ${BACKUP_OUT}" ;;
esac

# ---------------------------------------------------------------------------
echo "==> §2  Retention seen RED: ABSENT, FORBIDDEN and WRONG are three answers"
# --check is the instrument, so it is the thing that has to be shown failing —
# and since pd-2hnp it has to be shown failing three DIFFERENT ways, with the
# exit code asserted rather than merely "non-zero". The failure this section
# exists for is not the loud one: it is an operator whose credentials cannot
# read an APPLIED rule being told there is no rule, and re-applying or widening
# one on the strength of that.

put_lifecycle() {
	podman run --rm --network "$NET" \
		-e AWS_ACCESS_KEY_ID="$ROOT_AK" -e AWS_SECRET_ACCESS_KEY="$ROOT_SK" \
		-e AWS_DEFAULT_REGION=us-west-2 "$AWSCLI_IMAGE" \
		--endpoint-url "$MINIO_URL" s3api put-bucket-lifecycle-configuration \
		--bucket "$BUCKET" --lifecycle-configuration "$1" >/dev/null
}

delete_lifecycle() {
	podman run --rm --network "$NET" \
		-e AWS_ACCESS_KEY_ID="$ROOT_AK" -e AWS_SECRET_ACCESS_KEY="$ROOT_SK" \
		-e AWS_DEFAULT_REGION=us-west-2 "$AWSCLI_IMAGE" \
		--endpoint-url "$MINIO_URL" s3api delete-bucket-lifecycle \
		--bucket "$BUCKET" >/dev/null
}

expect_check_rc() { # expect_check_rc <want-rc> <phrase> <what>
	local want="$1" phrase="$2" what="$3" rc=0
	zone_script --check >"${WORK}/check.log" 2>&1 || rc=$?
	[ "$rc" -ne 0 ] || die "the retention check PASSED with ${what} — it is not checking anything"
	[ "$rc" -eq "$want" ] ||
		die "the retention check answered ${rc} for ${what}, expected ${want}: $(cat "${WORK}/check.log")"
	grep -q "$phrase" "${WORK}/check.log" ||
		die "the check exited ${rc} for ${what} without saying '${phrase}': $(cat "${WORK}/check.log")"
	echo "    red  ${what} -> exit ${rc}"
}

# (a) A rule with no prefix spans the bucket: raw/ starts expiring, silently.
put_lifecycle '{"Rules":[{"ID":"whole-bucket","Status":"Enabled","Filter":{"Prefix":""},"Expiration":{"Days":90}}]}'
expect_check_rc 1 "spans the whole bucket" "a whole-bucket rule (it would expire the catalog)"

# (b) The right prefix, the wrong window. 90 days IS the backfill window.
put_lifecycle '{"Rules":[{"ID":"too-long","Status":"Enabled","Filter":{"Prefix":"tenant/"},"Expiration":{"Days":365}}]}'
expect_check_rc 1 "not 90" "365-day retention on tenant/"

# (c) A second rule that reaches the catalog, beside a correct tenant rule —
#     the shape a well-meaning addition actually takes.
put_lifecycle '{"Rules":[{"ID":"tenant","Status":"Enabled","Filter":{"Prefix":"tenant/"},"Expiration":{"Days":90}},{"ID":"tidy-raw","Status":"Enabled","Filter":{"Prefix":"raw/"},"Expiration":{"Days":30}}]}'
expect_check_rc 1 "reaches the catalog zone" "a second rule expiring raw/ (whose retention is indefinite by decision)"

# (d) ABSENT — the one case where "there is no retention rule" is the truth,
#     and the only one that may say so. Its repair is to apply the rule, which
#     is exactly why none of the others may be reported this way.
delete_lifecycle
expect_check_rc 3 "ABSENT" "no lifecycle configuration at all"

# And green again from the same instrument, so the red runs above were the
# configuration failing and not the checker having broken.
zone_script --apply >/dev/null 2>&1 || die "re-applying the correct rule failed"
zone_script --check >/dev/null 2>&1 || die "the check did not go green again"
echo "    ok   and green again once the correct rule is restored"

# (e) FORBIDDEN, against the rule that was just restored — this is pd-2hnp
#     itself. The retention IS applied and IS correct; the identity running the
#     check cannot read it. A denial rendered as absence tells an operator to
#     apply a rule that already exists, or to widen one.
#
#     A real MinIO credential refused by a real policy, not a string this gate
#     invented: what is being tested is how the script classifies what the
#     server actually says.
echo "==> §2b The same correct rule, read by a credential that may not look"
BLIND_AK=pdtzblind
BLIND_SK=pdtzblindsecret123
cat >"${WORK}/policies/blind.json" <<EOF
{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Action":["s3:GetObject","s3:ListBucket"],
"Resource":["arn:aws:s3:::${BUCKET}","arn:aws:s3:::${BUCKET}/*"]}]}
EOF
mc_root admin user add x "$BLIND_AK" "$BLIND_SK" >/dev/null
mc_root admin policy create x pdtz-blind /policies/blind.json >/dev/null 2>&1 ||
	mc_root admin policy add x pdtz-blind /policies/blind.json >/dev/null 2>&1 ||
	die "could not install the lifecycle-blind policy"
mc_root admin policy attach x pdtz-blind --user "$BLIND_AK" >/dev/null 2>&1 ||
	die "could not attach the lifecycle-blind policy to ${BLIND_AK}"

# It has to reach the bucket at all, or exit 4 could be the credential simply
# being broken — the "passed because its subject never ran" failure again.
mc_as "$BLIND_AK" "$BLIND_SK" ls "x/${BUCKET}/raw/" >/dev/null 2>&1 ||
	die "the lifecycle-blind credential cannot reach the bucket at all — it proves nothing about lifecycle permission"

BLIND_RC=0
zone_script_as "$BLIND_AK" "$BLIND_SK" --check >"${WORK}/blind.log" 2>&1 || BLIND_RC=$?
[ "$BLIND_RC" -eq 4 ] ||
	die "a credential that may not READ the lifecycle answered ${BLIND_RC}; expected 4 (cannot verify): $(cat "${WORK}/blind.log")"
grep -q "CANNOT VERIFY" "${WORK}/blind.log" ||
	die "a denied read did not say CANNOT VERIFY: $(cat "${WORK}/blind.log")"
grep -q "s3:GetLifecycleConfiguration" "${WORK}/blind.log" ||
	die "a denied read did not name the missing permission: $(cat "${WORK}/blind.log")"
# The regression itself. ABSENT is the verdict label; a denied run may say the
# word "absent" only in the sentence explaining that this is NOT that answer.
grep -q "ABSENT" "${WORK}/blind.log" &&
	die "a DENIED read was reported as ABSENT — the two opposite facts are one sentence again: $(cat "${WORK}/blind.log")"
echo "    red  a credential without s3:GetLifecycleConfiguration -> exit 4, not 3"

# The rule the blind credential could not see is still there, and the identity
# that CAN see it still says so. Without this, exit 4 above could be a bucket
# that had quietly lost its rule.
zone_script --check >/dev/null 2>&1 ||
	die "the correct rule did not survive the forbidden probe"
echo "    ok   and the rule it could not see is still applied, and still correct"

# ---------------------------------------------------------------------------
echo "==> §3  Two identities, from the rendered policy documents"
zone_script --render --out "${WORK}/policies" >/dev/null ||
	die "--render failed"
[ -s "${WORK}/policies/catalog-credentials.json" ] || die "no rendered catalog policy"
[ -s "${WORK}/policies/tenant-credentials.json" ] || die "no rendered tenant policy"
grep -q "{{BUCKET}}" "${WORK}/policies/"*.json &&
	die "a rendered policy still carries the {{BUCKET}} placeholder"
grep -q "arn:aws:s3:::${BUCKET}/tenant/\*" "${WORK}/policies/catalog-credentials.json" ||
	die "the rendered catalog policy does not name this bucket's tenant prefix"

policy_install() {
	local name="$1" file="$2"
	mc_root admin policy create x "$name" "/policies/${file}" >/dev/null 2>&1 ||
		mc_root admin policy add x "$name" "/policies/${file}" >/dev/null 2>&1 ||
		die "could not install the ${name} policy"
}
policy_attach() {
	mc_root admin policy attach x "$1" --user "$2" >/dev/null 2>&1 ||
		die "could not attach ${1} to ${2}"
}
# Detaching has to be able to FAIL LOUDLY. §6 replaces a credential's policy to
# break the boundary, and a detach that quietly did nothing would leave the
# correct policy in place — which would make the red run come out green and be
# reported as "the check is not checking anything". The failure would be real,
# the diagnosis would be wrong, so this does not swallow its status.
policy_detach() {
	mc_root admin policy detach x "$1" --user "$2" >/dev/null 2>&1 ||
		die "could not detach ${1} from ${2}"
}

mc_root admin user add x "$CAT_AK" "$CAT_SK" >/dev/null
mc_root admin user add x "$TEN_AK" "$TEN_SK" >/dev/null
policy_install pdtz-catalog catalog-credentials.json
policy_install pdtz-tenant tenant-credentials.json
policy_attach pdtz-catalog "$CAT_AK"
policy_attach pdtz-tenant "$TEN_AK"
echo "    ok   ${CAT_AK} and ${TEN_AK} carry the two documents, verbatim"

# ---------------------------------------------------------------------------
# The probes. Each is one S3 operation by one identity against one key, and
# each returns a status rather than dying — §6 re-runs these very functions and
# needs to invert them.

can_get() { mc_as "$1" "$2" cat "x/${BUCKET}/$3" >/dev/null 2>&1; }
can_put() { printf 'governance probe' | mc_as "$1" "$2" pipe "x/${BUCKET}/$3" >/dev/null 2>&1; }
can_list() { mc_as "$1" "$2" ls "x/${BUCKET}/$3" >/dev/null 2>&1; }

RAW_KEY="raw/source=tcgcsv/dataset=groups/ingest_date=2026-08-13/run=01TEST/part-0000.json"
LAKE_KEY="lake/catalog/prices/metadata/v1.metadata.json"
# A probe key shaped like the real layout, holding no tenant data — the bytes
# are the literal string "governance probe". §7 removes it.
PROBE_KEY="tenant/database_id=01TESTPROBE0000000000TZTZ/dataset=holdings/as_of=2026-08-13/part-0000.parquet"

# The two isolation claims, each a function so §6 can run exactly these again
# against a deliberately-broken configuration. They print what failed, and
# return non-zero — never exit — for the same reason.
catalog_cannot_reach_tenant() {
	local bad=0
	can_get "$CAT_AK" "$CAT_SK" "$PROBE_KEY" && {
		diag "   !! catalog credentials READ ${PROBE_KEY}"
		bad=1
	}
	can_list "$CAT_AK" "$CAT_SK" "tenant/" && {
		diag "   !! catalog credentials LISTED tenant/"
		bad=1
	}
	can_put "$CAT_AK" "$CAT_SK" "tenant/database_id=01TESTPROBE0000000000TZTZ/intrusion" && {
		diag "   !! catalog credentials WROTE into tenant/"
		bad=1
	}
	return "$bad"
}

tenant_cannot_reach_catalog() {
	local bad=0
	can_get "$TEN_AK" "$TEN_SK" "$RAW_KEY" && {
		diag "   !! tenant credentials READ ${RAW_KEY}"
		bad=1
	}
	can_get "$TEN_AK" "$TEN_SK" "$LAKE_KEY" && {
		diag "   !! tenant credentials READ ${LAKE_KEY}"
		bad=1
	}
	can_list "$TEN_AK" "$TEN_SK" "raw/" && {
		diag "   !! tenant credentials LISTED raw/"
		bad=1
	}
	can_put "$TEN_AK" "$TEN_SK" "raw/intrusion" && {
		diag "   !! tenant credentials WROTE into raw/"
		bad=1
	}
	return "$bad"
}

# ---------------------------------------------------------------------------
echo "==> §4  Each identity CAN reach its own zone"
# This half is what makes §5 mean anything: it establishes that the objects
# exist and are reachable, so a denial below is a denial and not an absence.
can_get "$CAT_AK" "$CAT_SK" "$RAW_KEY" ||
	die "catalog credentials cannot read their own raw/ object — the policy is too narrow"
can_get "$CAT_AK" "$CAT_SK" "$LAKE_KEY" ||
	die "catalog credentials cannot read their own lake/ object"
can_list "$CAT_AK" "$CAT_SK" "raw/" ||
	die "catalog credentials cannot list raw/"
can_put "$TEN_AK" "$TEN_SK" "$PROBE_KEY" ||
	die "tenant credentials cannot write their own zone — the policy is too narrow"
can_get "$TEN_AK" "$TEN_SK" "$PROBE_KEY" ||
	die "tenant credentials cannot read back what they just wrote"
can_list "$TEN_AK" "$TEN_SK" "tenant/" ||
	die "tenant credentials cannot list tenant/"
echo "    ok   catalog reads raw/ + lake/; tenant reads, writes and lists tenant/"

echo "==> §5  THE GATE, both directions, against the correct configuration"
catalog_cannot_reach_tenant ||
	die "catalog credentials reached the tenant zone"
echo "    ok   catalog credentials cannot read, list or write tenant/"
tenant_cannot_reach_catalog ||
	die "tenant credentials reached the catalog zone"
echo "    ok   tenant credentials cannot read, list or write raw/ or lake/"

# ---------------------------------------------------------------------------
echo "==> §6  THE GATE SEEN RED: break the boundary, the SAME assertions must fail"
# Not a differently-worded check — the identical functions §5 just ran. If
# either of these passes, §5's green means nothing whatsoever.

# The realistic misconfiguration, not an exotic one: somebody writes
# `Resource: bucket/*` because it is shorter, and drops the Deny statements
# because with a narrow Allow they look redundant. That is a credential which
# reaches both zones, and it is what the boundary has to be shown failing
# against.
#
# It has to REPLACE the correct policy rather than sit beside it — see §6b for
# why, which is a property worth having found.
WIDE='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":["arn:aws:s3:::'"${BUCKET}"'","arn:aws:s3:::'"${BUCKET}"'/*"]}]}'
printf '%s' "$WIDE" >"${WORK}/policies/wide.json"
policy_install pdtz-wide wide.json

expect_red() {
	local what="$1" fn="$2"
	if "$fn"; then
		die "${what}: the boundary check still PASSED against a credential that can reach the whole bucket — it is not checking anything"
	fi
	echo "    red  ${what} replaced by a whole-bucket grant -> the check fails, as it must"
}

policy_detach pdtz-catalog "$CAT_AK"
policy_attach pdtz-wide "$CAT_AK"
expect_red "catalog credentials" catalog_cannot_reach_tenant
policy_detach pdtz-wide "$CAT_AK"
policy_attach pdtz-catalog "$CAT_AK"
catalog_cannot_reach_tenant ||
	die "restoring the correct catalog policy did not restore the boundary"
echo "    ok   and green again once the correct policy is restored"

policy_detach pdtz-tenant "$TEN_AK"
policy_attach pdtz-wide "$TEN_AK"
expect_red "tenant credentials" tenant_cannot_reach_catalog
policy_detach pdtz-wide "$TEN_AK"
policy_attach pdtz-tenant "$TEN_AK"
tenant_cannot_reach_catalog ||
	die "restoring the correct tenant policy did not restore the boundary"
echo "    ok   and green again once the correct policy is restored"

# ---------------------------------------------------------------------------
echo "==> §6b The explicit Deny statements are load-bearing, not decoration"
# Found by writing §6 wrong: attaching the whole-bucket grant BESIDE the
# correct policy does not open the boundary, because an explicit Deny beats any
# Allow. That is the single most valuable property these documents have — it
# means a later, broader grant added somewhere else cannot silently widen
# either zone — and it is worth an assertion of its own rather than a comment.
#
# It is also the reason §6 has to detach before it attaches. A red run that
# quietly stayed green here would have been read as "the check is broken".
policy_attach pdtz-wide "$CAT_AK"
catalog_cannot_reach_tenant ||
	die "a whole-bucket grant ALONGSIDE the catalog policy opened the tenant zone — the Deny statements are not doing their job"
policy_detach pdtz-wide "$CAT_AK"

policy_attach pdtz-wide "$TEN_AK"
tenant_cannot_reach_catalog ||
	die "a whole-bucket grant ALONGSIDE the tenant policy opened the catalog zone — the Deny statements are not doing their job"
policy_detach pdtz-wide "$TEN_AK"
echo "    ok   a whole-bucket grant added BESIDE either policy still cannot cross"

# The widened run may have written its intrusion objects; they are not tenant
# data, but §7 counts what is in the zone, so they go.
mc_root rm --recursive --force "x/${BUCKET}/tenant/" >/dev/null 2>&1 || true
mc_root rm --force "x/${BUCKET}/raw/intrusion" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
echo "==> §7  The zone is EMPTY of tenant data"
# This item builds the governance, not the data flow. Anything left under
# tenant/ at this point is tenant-shaped data in a zone whose shipper does not
# exist yet — and every fixture here is treated as if it were real.
REMAINING=$(mc_root ls --recursive "x/${BUCKET}/tenant/" 2>/dev/null | wc -l)
[ "$REMAINING" -eq 0 ] ||
	die "${REMAINING} object(s) remain under tenant/ — the zone is meant to be empty until the shipper exists"
# And the catalog zone is untouched by everything above.
CATALOG_OBJECTS=$(mc_root ls --recursive "x/${BUCKET}/" 2>/dev/null | grep -c ' raw/\| lake/' || true)
[ "$CATALOG_OBJECTS" -eq 2 ] ||
	die "the catalog zone holds ${CATALOG_OBJECTS} objects, expected the 2 seeded — something wrote or deleted there"
echo "    ok   tenant/ holds nothing; the catalog's 2 seeded objects are untouched"

# ---------------------------------------------------------------------------
echo "==> §8  The prefixes and the window do not drift across their three homes"
# Rust (the shipper reads them at runtime), the policy documents (AWS reads
# them) and the deploy script (bash) cannot share an implementation. What they
# can share is this check. A prefix changed in one and not the others does not
# fail loudly — it silently widens a policy.
RS="${REPO_DIR}/crates/pkdump-lake/src/tenant.rs"
RUST_TENANT=$(grep -oP 'TENANT_ROOT: &str = "\K[^"]+' "$RS")
RUST_DAYS=$(grep -oP 'RETENTION_DAYS: u32 = \K[0-9]+' "$RS")
# Split on the comma FIRST: without it `[^"]+` happily matches the `, ` between
# two quoted strings and the list comes out with a phantom entry in it.
RUST_CATALOG=$(grep -oP 'CATALOG_ROOTS: &\[&str\] = &\[\K[^]]+' "$RS" |
	tr ',' '\n' | grep -oP '"\K[^"]+')

[ "$RUST_TENANT" = "tenant/" ] || die "tenant.rs says the zone is ${RUST_TENANT}; this gate is written against tenant/"
[ "$RUST_DAYS" = "90" ] || die "tenant.rs says ${RUST_DAYS}-day retention; 90 is the product limit"

SRC_POLICIES="${REPO_DIR}/deploy/policies/tenant-zone"
grep -q "\"Prefix\": \"${RUST_TENANT}\"" "${SRC_POLICIES}/lifecycle.json" ||
	die "lifecycle.json does not scope itself to ${RUST_TENANT}"
grep -q "\"Days\": ${RUST_DAYS}" "${SRC_POLICIES}/lifecycle.json" ||
	die "lifecycle.json does not expire after ${RUST_DAYS} days"
grep -q "{{BUCKET}}/${RUST_TENANT}\*" "${SRC_POLICIES}/catalog-credentials.json" ||
	die "the catalog policy does not deny ${RUST_TENANT} by that name"
grep -q "{{BUCKET}}/${RUST_TENANT}\*" "${SRC_POLICIES}/tenant-credentials.json" ||
	die "the tenant policy does not allow ${RUST_TENANT} by that name"
for c in $RUST_CATALOG; do
	grep -q "{{BUCKET}}/${c}\*" "${SRC_POLICIES}/catalog-credentials.json" ||
		die "the catalog policy does not allow ${c}, which tenant.rs calls a catalog root"
	grep -q "{{BUCKET}}/${c}\*" "${SRC_POLICIES}/tenant-credentials.json" ||
		die "the tenant policy does not deny ${c}, which tenant.rs calls a catalog root"
done

ZONE_SH="${REPO_DIR}/deploy/setup-tenant-zone.sh"
grep -q "^TENANT_PREFIX=\"${RUST_TENANT}\"$" "$ZONE_SH" ||
	die "setup-tenant-zone.sh disagrees with tenant.rs about the zone prefix"
grep -q "^RETENTION_DAYS=${RUST_DAYS}$" "$ZONE_SH" ||
	die "setup-tenant-zone.sh disagrees with tenant.rs about the retention window"
grep -q "^CATALOG_PREFIXES=\"$(echo "$RUST_CATALOG" | tr '\n' ' ' | sed 's/ $//')\"$" "$ZONE_SH" ||
	die "setup-tenant-zone.sh disagrees with tenant.rs about the catalog prefixes"
echo "    ok   ${RUST_TENANT} / ${RUST_CATALOG//$'\n'/ } / ${RUST_DAYS}d agree across Rust, the policies and the script"

echo ""
echo "==> tests/lake/tenant_zone.sh PASSED"
