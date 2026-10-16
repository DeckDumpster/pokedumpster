#!/usr/bin/env bash
# coverage-json.sh — which suites cover which use cases, as the JSON `test-plan` reads.
#
#   coverage-json.sh              the working tree
#   coverage-json.sh --ref <ref>  the tree as it stood at <ref> (for `test-plan orphans`)
#
# Ported from Spira's spira/suite-coverage-json.sh; the output shape is the same
# (`{"path","tier","covers":[…]}` per suite) so test-plan reads it unchanged. Two sources:
#
#   * the intent runner. tests/ui/test_scenarios.ts discovers tests/ui/intents/*.yaml by
#     glob and runs every one, so it covers exactly the use cases whose intent file
#     exists. A deleted intent therefore orphans its use case, and a deleted runner
#     orphans all of them. The use case for an intent is the catalogue row whose
#     statement begins "<intent-name>: " (docs/test-plan/ui.toml).
#   * a `# covers: UC-…` / `// covers: UC-…` line in any file under tests/ or crates/ —
#     the declaration Spira's suites carry. tests/ui/implementations/*.ts declare theirs.
#
# tier: T0
# covers: tests/plan/coverage-json.sh docs/test-plan/*.toml
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd -P)"
ROOT="$REPO"
case "${1:-}" in
    --ref)
        ref="${2:?usage: coverage-json.sh --ref <ref>}"
        ROOT="$(mktemp -d)"; trap 'rm -rf "$ROOT"' EXIT
        paths=()
        for d in tests crates docs; do
            git -C "$REPO" cat-file -e "$ref:$d" 2>/dev/null && paths+=("$d")
        done
        [ "${#paths[@]}" -gt 0 ] || { echo "coverage-json: cannot read $ref" >&2; exit 1; }
        git -C "$REPO" archive "$ref" "${paths[@]}" | tar -x -C "$ROOT" || {
            echo "coverage-json: cannot read $ref" >&2; exit 1; }
        ;;
    "") ;;
    *) echo "coverage-json: unknown argument $1" >&2; exit 2 ;;
esac

CATALOGUE="$ROOT/docs/test-plan/ui.toml"
esc() { local s="$1"; s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; printf '%s' "$s"; }

# slug -> id, one "<id> <slug>" per line, from the catalogue ([[use_case]] id then statement).
slug_ids() {
    [ -f "$CATALOGUE" ] || return 0
    awk '/^id = "/ { gsub(/^id = "|"$/, ""); id = $0 }
         /^statement = "/ { s = $0; sub(/^statement = "/, "", s); sub(/: .*/, "", s); print id, s }' "$CATALOGUE"
}

first=1
sep() { [ "$first" = 1 ] && first=0 || printf ','; }

printf '['
if [ -d "$ROOT/tests/ui/intents" ] && [ -f "$ROOT/tests/ui/test_scenarios.ts" ]; then
    sep
    printf '{"path":"tests/ui/test_scenarios.ts","tier":"T3","covers":['
    f2=1
    while read -r id slug; do
        [ -f "$ROOT/tests/ui/intents/$slug.yaml" ] || continue
        [ "$f2" = 1 ] && f2=0 || printf ','
        printf '"%s"' "$id"
    done < <(slug_ids)
    printf ']}'
fi

declare -A seen=() tier=()
while IFS= read -r file; do
    ids="$(grep -hE '^[[:space:]]*(#|//) *covers:' "$ROOT/$file" | grep -oE 'UC-[a-z0-9-]+-[0-9]+' | sort -u | tr '\n' ' ')"
    [ -n "$ids" ] || continue
    t="$(sed -n 's/^[[:space:]]*\(#\|\/\/\) *tier: *\(T[0-4]\).*/\2/p' "$ROOT/$file" | head -1)"
    seen["$file"]="$ids"; tier["$file"]="${t:-T3}"
done < <(cd "$ROOT" && grep -rlE '^[[:space:]]*(#|//) *covers:.*UC-' tests crates 2>/dev/null | grep -v '^tests/plan/plan_test.sh$' | sort)
for file in "${!seen[@]}"; do
    sep
    printf '{"path":"%s","tier":"%s","covers":[' "$(esc "$file")" "${tier[$file]}"
    f2=1
    for id in ${seen[$file]}; do
        [ "$f2" = 1 ] && f2=0 || printf ','
        printf '"%s"' "$id"
    done
    printf ']}'
done
printf ']\n'
