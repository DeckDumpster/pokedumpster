#!/usr/bin/env bash
#
# Govern the TENANT ZONE (pd-uz8q, item 2 of pd-8lw7).
#
# The tenant zone is `tenant/` inside the lake's own bucket: holdings and
# valuations, always keyed by database_id, retained 90 days, reachable only by
# credentials that reach nothing else. It is a DIFFERENT OBJECT from the
# catalog zone (`raw/`, `lake/`) that happens to share a bucket with it, and
# this script is what makes that separation real rather than described.
#
#   bash deploy/setup-tenant-zone.sh --apply            # apply retention, then verify it
#   bash deploy/setup-tenant-zone.sh --check            # verify only; see the exits below
#   bash deploy/setup-tenant-zone.sh --render --out DIR # write the two policy documents
#
# ── THREE OUTCOMES, THREE EXITS (pd-2hnp) ───────────────────────────────────
#
# "There is no retention rule" and "I am not allowed to look" are OPPOSITE
# facts, and this script printed the same sentence for both. On a check whose
# subject is a rule that DELETES TENANT DATA AFTER 90 DAYS that is worse than
# having no check, because it is trusted: the direction that hurts is not the
# one seen first (an identity that can read nothing, reported as "never
# applied") but its inverse — once the rule IS applied, an operator whose
# credentials cannot read it is told the same sentence, concludes retention was
# never applied, and re-applies or widens one.
#
#   0  present and correct
#   1  PRESENT BUT WRONG — a rule exists and is not the rule: the wrong window,
#      or a prefix that reaches the catalog zone. Every configuration refusal
#      below exits 1 too; each names itself
#   2  usage
#   3  ABSENT — the bucket carries no lifecycle configuration at all, or none of
#      its enabled rules covers the tenant zone. Retention was never applied,
#      and --apply is the repair
#   4  CANNOT VERIFY — the read itself failed; AccessDenied first among them.
#      NEVER rendered as absent, and never rendered as correct. The message
#      names the missing permission and the identity that was refused
#
# ── WHAT IT APPLIES, AND WHAT IT ONLY PRINTS ────────────────────────────────
#
# Retention is bucket configuration, so it is APPLIED: a 90-day expiration
# scoped to `tenant/`, read back and checked afterwards, because an S3 lifecycle
# PUT that lands somewhere unexpected reports success exactly like one that does
# not.
#
# The two credential policies are IAM, which is account configuration — the role
# ARNs, the trust policy and the assume-role chain are not facts this repo holds.
# So they are RENDERED (the bucket substituted in) for the operator to attach,
# and the acceptance gate applies the rendered documents verbatim to a MinIO
# standing in for the bucket. Same bytes both places: an IAM policy document is
# the same dialect for AWS and for MinIO, which is the only reason this
# separation can be tested at all rather than trusted.
#
# ── THE REFUSALS ────────────────────────────────────────────────────────────
#
# This script writes a rule whose whole job is DELETING OBJECTS AFTER 90 DAYS,
# so every way it could be pointed at the wrong bucket is a refusal:
#
#   * no bucket configured          -> names ~/.config/pkdump/lake.env and stops
#   * the Litestream backup bucket  -> stops. That bucket holds the only
#                                      irreplaceable data in the system; an
#                                      expiry rule must never be able to reach it
#   * a rule that reaches `raw/`    -> stops. The catalog's retention is
#     or `lake/`, or one with no       INDEFINITE and deliberately unmanaged; a
#     prefix at all                    lifecycle rule that spans the bucket would
#                                      quietly start deleting it
#   * 90 days changed to anything   -> stops. It is a hard product limit, not a
#     else                             tunable — it IS the backfill window and it
#                                      is what bounds a missed deletion's blast
#                                      radius. Changing it is a decision to file
#
# Prod-safe: it touches no systemd unit, no podman volume, no container, and no
# tenant database. It talks to one bucket and nothing else.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
POLICY_DIR="${SCRIPT_DIR}/policies/tenant-zone"

# The zone's prefix and its retention live in crates/pkdump-lake/src/tenant.rs
# as well, because the shipper needs them at runtime. These two lines are the
# other copy, and tests/lake/tenant_zone.sh §8 is what stops them drifting.
TENANT_PREFIX="tenant/"
CATALOG_PREFIXES="raw/ lake/"
RETENTION_DAYS=90

# The aws CLI, overridable as a whole command so the gate can run a
# containerised one instead of depending on the host having it installed. Every
# JSON document is passed INLINE rather than as file://, which is what makes a
# container wrapper work — a path this shell can see is not a path that shell
# can.
read -r -a AWS <<<"${PKDUMP_AWS:-aws}"

MODE=""
BUCKET=""
ENDPOINT=""
PROFILE=""
OUT_DIR=""

usage() {
	echo "usage: bash deploy/setup-tenant-zone.sh (--apply|--check|--render)"
	echo "         [--bucket B] [--endpoint URL] [--profile P] [--out DIR]"
}

while [ $# -gt 0 ]; do
	case "$1" in
	--apply | --check | --render) MODE="${1#--}" ;;
	--bucket)
		shift
		BUCKET="${1:-}"
		;;
	--endpoint)
		shift
		ENDPOINT="${1:-}"
		;;
	--profile)
		shift
		PROFILE="${1:-}"
		;;
	--out)
		shift
		OUT_DIR="${1:-}"
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "unknown argument: $1" >&2
		usage >&2
		exit 2
		;;
	esac
	shift
done

[ -n "$MODE" ] || {
	usage >&2
	exit 2
}

die() {
	echo "!! $*" >&2
	exit 1
}

command -v python3 >/dev/null 2>&1 ||
	die "python3 is required: this script reads the applied lifecycle back and checks it, and a check that cannot parse is a check that cannot fail"

# ── The bucket ──────────────────────────────────────────────────────────────
# Host config, exactly like every other bucket name in this repo. There is no
# default and there will not be one.

LAKE_ENV="${PKDUMP_LAKE_ENV:-${HOME}/.config/pkdump/lake.env}"

if [ -z "$BUCKET" ] && [ -f "$LAKE_ENV" ]; then
	# The same dotenv subset crates/pkdump-lake/src/config.rs parses.
	# shellcheck disable=SC1090
	BUCKET="$(set -a && . "$LAKE_ENV" && printf '%s' "${PKDUMP_LAKE_S3_BUCKET:-}")"
fi

[ -n "$BUCKET" ] || die "no bucket: pass --bucket, or set PKDUMP_LAKE_S3_BUCKET in ${LAKE_ENV}.
The tenant zone lives in the LAKE's bucket under the ${TENANT_PREFIX} prefix (decided 2026-08-13:
one bucket, separate prefix, revisited once proven out). Its name is host configuration and has
no default — guessing one here would mean applying a 90-day delete rule to a bucket nobody named."

# The one bucket this must never be pointed at. Everything in the lake is
# reproducible; the Litestream bucket holds the only irreplaceable data in the
# system, and this script's whole output is a rule that deletes things.
for inst_env in "${HOME}"/.config/pkdump/*/litestream.env; do
	[ -f "$inst_env" ] || continue
	# shellcheck disable=SC1090
	backup="$(set -a && . "$inst_env" && printf '%s' "${LITESTREAM_S3_BUCKET:-}")"
	[ -n "$backup" ] || continue
	[ "$backup" != "$BUCKET" ] ||
		die "refusing: ${BUCKET} is the Litestream BACKUP bucket (named in ${inst_env}).
That bucket holds the only irreplaceable data in the system. This script applies an expiration
rule; it must never be able to reach the backups. The lake bucket is a separate bucket."
done

AWS_ARGS=()
[ -z "$ENDPOINT" ] || AWS_ARGS+=(--endpoint-url "$ENDPOINT")
[ -z "$PROFILE" ] || AWS_ARGS+=(--profile "$PROFILE")

# ── Rendering ───────────────────────────────────────────────────────────────

render() {
	sed "s|{{BUCKET}}|${BUCKET}|g" "$1"
}

if [ -n "$OUT_DIR" ]; then
	mkdir -p "$OUT_DIR"
fi

emit_policy() {
	local name="$1" src="${POLICY_DIR}/$1.json"
	[ -f "$src" ] || die "missing policy document: ${src}"
	if [ -n "$OUT_DIR" ]; then
		render "$src" >"${OUT_DIR}/${name}.json"
		echo "    ${OUT_DIR}/${name}.json"
	else
		echo "--- ${name}.json ---"
		render "$src"
	fi
}

if [ "$MODE" = "render" ]; then
	echo "==> Credential policies for ${BUCKET}"
	emit_policy catalog-credentials
	emit_policy tenant-credentials
	echo ""
	echo "Attach each to its own role. They are mirror images: the catalog role"
	echo "reaches ${CATALOG_PREFIXES}and is explicitly denied ${TENANT_PREFIX}, the tenant"
	echo "role reaches ${TENANT_PREFIX} and is explicitly denied the rest. Neither may"
	echo "rewrite the bucket's lifecycle — retention is not theirs to widen."
	exit 0
fi

# ── Whose credentials are these? ────────────────────────────────────────────
# This script can apply a rule that DELETES OBJECTS, and with --profile omitted
# it acts as whatever the environment happens to hold. On 2026-08-26 that was
# another project's BACKUP user, pointed at this project's lake bucket; it
# failed safe only because that user had no lifecycle permission anywhere. So
# the identity is resolved and PRINTED before anything acts, and named again in
# every refusal below — a denial is only diagnosable if you know who was denied.
#
# Resolution is best-effort on purpose. sts:GetCallerIdentity is not something
# every endpoint implements (MinIO does not), and a governance check that
# refuses to run against a stand-in bucket is a check that cannot be tested.
# Unresolved is printed AS unresolved, with the reason — never as nothing.

IDENTITY=""
IDENTITY_ERR=""

resolve_identity() {
	local errfile out rc=0
	errfile="$(mktemp)"
	out="$("${AWS[@]}" "${AWS_ARGS[@]}" sts get-caller-identity \
		--query Arn --output text 2>"$errfile")" || rc=$?
	IDENTITY_ERR="$(tr '\n' ' ' <"$errfile")"
	rm -f "$errfile"
	[ "$rc" -eq 0 ] || return 0
	[ -n "$out" ] && [ "$out" != "None" ] || return 0
	IDENTITY="$out"
}

identity_line() {
	if [ -n "$IDENTITY" ]; then
		printf '%s' "$IDENTITY"
	else
		printf 'UNRESOLVED (sts get-caller-identity: %s)' "${IDENTITY_ERR:-no answer}"
	fi
}

resolve_identity
echo "==> Identity: $(identity_line)"
if [ -n "$PROFILE" ]; then
	echo "    named by --profile ${PROFILE}"
else
	echo "    NO --profile GIVEN — these are ambient credentials, whatever the"
	echo "    environment holds. Pass --profile to name them instead of inheriting"
	echo "    them; this script's output is a rule that deletes objects."
fi

# ── The check ───────────────────────────────────────────────────────────────
# Shared by --apply (which runs it afterwards) and --check (which runs only it),
# because "applied" and "applied where I meant" are different claims and only
# the second one is worth anything.

# Its three non-zero answers are 1 (present but wrong), 3 (absent) and 4 (cannot
# verify), and keeping them apart is the whole of pd-2hnp. The read's own
# failure is classified rather than collapsed: only NoSuchLifecycleConfiguration
# is absence. Everything else — AccessDenied first, but anything unrecognised
# too — is "cannot verify", because the one place a governance check must not
# guess is about a deletion rule it could not read.
check_lifecycle() {
	local applied errfile err rc=0
	errfile="$(mktemp)"
	applied="$("${AWS[@]}" "${AWS_ARGS[@]}" s3api get-bucket-lifecycle-configuration \
		--bucket "$BUCKET" --output json 2>"$errfile")" || rc=$?
	err="$(tr '\n' ' ' <"$errfile")"
	rm -f "$errfile"

	if [ "$rc" -ne 0 ]; then
		case "$err" in
		*NoSuchLifecycleConfiguration*)
			echo "   !! ABSENT: ${BUCKET} carries no lifecycle configuration at all, so" >&2
			echo "      ${TENANT_PREFIX} retains INDEFINITELY — the catalog's policy applied to the" >&2
			echo "      one kind of data it was never meant for." >&2
			echo "      identity: $(identity_line)" >&2
			echo "      repair  : bash deploy/setup-tenant-zone.sh --apply" >&2
			return 3
			;;
		*AccessDenied* | *AccessDeniedException* | *"not authorized"* | *Forbidden*)
			echo "   !! CANNOT VERIFY: reading the lifecycle configuration of ${BUCKET} was DENIED." >&2
			echo "      This is NOT \"there is no retention rule\". It is \"I am not allowed to" >&2
			echo "      look\", and the two are opposite facts. Conclude NOTHING about retention" >&2
			echo "      from this run, and do not apply or widen a rule on the strength of it." >&2
			echo "      identity: $(identity_line)" >&2
			echo "      needs   : s3:GetLifecycleConfiguration on arn:aws:s3:::${BUCKET}" >&2
			echo "      aws said: ${err}" >&2
			echo "      Both tenant-zone roles deny the lifecycle actions BY DESIGN — retention" >&2
			echo "      is not theirs to widen. deploy/TENANT_ZONE.md §4a names the identity that" >&2
			echo "      is meant to hold them." >&2
			return 4
			;;
		*)
			echo "   !! CANNOT VERIFY: reading the lifecycle configuration of ${BUCKET} failed for" >&2
			echo "      a reason this script does not recognise. It is deliberately NOT reported" >&2
			echo "      as absent: an unrecognised failure is exactly where guessing would be" >&2
			echo "      guessing about a rule that deletes tenant data." >&2
			echo "      identity: $(identity_line)" >&2
			echo "      aws said: ${err:-<no message>} (exit ${rc})" >&2
			return 4
			;;
		esac
	fi

	TENANT_PREFIX="$TENANT_PREFIX" CATALOG_PREFIXES="$CATALOG_PREFIXES" \
		RETENTION_DAYS="$RETENTION_DAYS" python3 -c '
import json, os, sys

doc = json.load(sys.stdin)
tenant = os.environ["TENANT_PREFIX"]
catalog = os.environ["CATALOG_PREFIXES"].split()
want_days = int(os.environ["RETENTION_DAYS"])

problems = []
covering = []

for rule in doc.get("Rules", []):
    rid = rule.get("ID", "<unnamed>")
    if rule.get("Status") != "Enabled":
        continue
    # A rule may carry its prefix at the top level (the pre-2018 spelling) or
    # under Filter, directly or inside an And. All three are read, because a
    # rule this check fails to understand is one it would silently pass.
    f = rule.get("Filter") or {}
    prefix = rule.get("Prefix")
    if prefix is None:
        prefix = f.get("Prefix")
    if prefix is None and "And" in f:
        prefix = (f["And"] or {}).get("Prefix")
    prefix = prefix or ""

    # The refusal that protects the catalog. An enabled rule with no prefix
    # spans the whole bucket, which means raw/ — retained INDEFINITELY and
    # deliberately unmanaged — starts expiring, silently, on whatever schedule
    # the rule names.
    if prefix == "":
        problems.append(
            f"rule {rid!r} has no prefix, so it spans the whole bucket: it would expire "
            f"the catalog zone, whose retention is indefinite by decision"
        )
        continue
    for c in catalog:
        if prefix.startswith(c) or c.startswith(prefix):
            problems.append(
                f"rule {rid!r} has prefix {prefix!r}, which reaches the catalog zone ({c})"
            )
    if not prefix.startswith(tenant):
        continue

    covering.append(rid)
    days = (rule.get("Expiration") or {}).get("Days")
    if days != want_days:
        problems.append(
            f"rule {rid!r} expires {tenant} after {days} days, not {want_days}. "
            f"{want_days} days is a hard product limit, not a tunable: it IS the backfill "
            f"window, and it is what bounds a missed deletion s blast radius"
        )

# Absence and wrongness are different answers with different repairs, so they
# are different exits. A document that exists but says nothing about the tenant
# zone is ABSENT retention, not wrong retention — and if it ALSO carries a rule
# that reaches the catalog, that is the more urgent fact and it wins.
if not covering:
    print(
        f"   !! ABSENT: no enabled rule expires {tenant!r}, so the tenant zone retains "
        f"indefinitely — the catalog s policy applied to the one kind of data it was "
        f"never meant for",
        file=sys.stderr,
    )

for p in problems:
    print("   !! " + p, file=sys.stderr)
if problems:
    sys.exit(1)
if not covering:
    sys.exit(3)
print(f"   ok   {tenant} expires after {want_days} days ({", ".join(covering)}); "
      f"no rule reaches the catalog zone")
' <<<"$applied"
}

# Three outcomes, three sentences, three exits. The summary line repeats which
# one it was, because the detail above it scrolls and the last line is what an
# operator reads.
if [ "$MODE" = "check" ]; then
	echo "==> Checking tenant-zone retention on ${BUCKET}"
	CHECK_RC=0
	check_lifecycle || CHECK_RC=$?
	case "$CHECK_RC" in
	0) exit 0 ;;
	3)
		echo "!! ABSENT: the tenant zone has NO retention rule (exit 3)" >&2
		exit 3
		;;
	4)
		echo "!! CANNOT VERIFY: this identity could not read ${BUCKET}'s retention (exit 4)." >&2
		echo "   That is not the same answer as \"absent\", and must not be acted on as one." >&2
		exit 4
		;;
	*)
		echo "!! the tenant zone's retention is not what it must be (exit 1)" >&2
		exit 1
		;;
	esac
fi

# ── Apply ───────────────────────────────────────────────────────────────────

echo "==> Applying tenant-zone retention to ${BUCKET}"
LIFECYCLE="$(cat "${POLICY_DIR}/lifecycle.json")"
PUT_ERR="$(mktemp)"
PUT_RC=0
"${AWS[@]}" "${AWS_ARGS[@]}" s3api put-bucket-lifecycle-configuration \
	--bucket "$BUCKET" --lifecycle-configuration "$LIFECYCLE" >/dev/null 2>"$PUT_ERR" || PUT_RC=$?
if [ "$PUT_RC" -ne 0 ]; then
	# The wall every operator hits here once: NEITHER zone role may hold this
	# permission, so the identity that applies retention is a third one. Say so
	# rather than leaving a bare aws traceback.
	PUT_MSG="$(tr '\n' ' ' <"$PUT_ERR")"
	rm -f "$PUT_ERR"
	echo "!! the lifecycle rule was NOT applied" >&2
	echo "   identity: $(identity_line)" >&2
	echo "   needs   : s3:PutLifecycleConfiguration on arn:aws:s3:::${BUCKET}" >&2
	echo "   aws said: ${PUT_MSG:-<no message>} (exit ${PUT_RC})" >&2
	echo "   Neither tenant-zone credential may carry that permission — both documents" >&2
	echo "   deny the lifecycle actions by design. Applying retention is a separate" >&2
	echo "   identity's job; deploy/TENANT_ZONE.md §4a names it." >&2
	exit 1
fi
rm -f "$PUT_ERR"

# Applied is not the claim; applied WHERE I MEANT is. A PUT that scoped itself
# to the wrong prefix succeeds identically to one that did not. The read-back
# keeps its own status: a PUT that landed and then could not be READ is exit 4,
# not a silent success and not "absent".
APPLY_RC=0
check_lifecycle || APPLY_RC=$?
if [ "$APPLY_RC" -ne 0 ]; then
	echo "!! the rule was accepted but the read-back did not confirm it — see above (exit ${APPLY_RC})" >&2
	exit "$APPLY_RC"
fi

echo ""
echo "==> Credential policies (attach these; IAM is account config, not repo config)"
if [ -z "$OUT_DIR" ]; then
	echo "    re-run with --render --out DIR to write them out"
else
	emit_policy catalog-credentials
	emit_policy tenant-credentials
fi
echo ""
echo "==> The zone is governed. It is also EMPTY, and meant to stay that way"
echo "    until the shipper exists — this script defines where tenant bytes may"
echo "    live and who may reach them, not how they get there."
