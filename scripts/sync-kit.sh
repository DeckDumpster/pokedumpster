#!/usr/bin/env bash
#
# Vendor dumpster-kit's ui assets (tokens, primitives) into frontend/src/lib/kit/ at ONE
# revision, and record that revision and a sha256 per file in frontend/kit.lock.
#
#   scripts/sync-kit.sh <kit-checkout> [<rev>]    vendor <rev> (default: the checkout's HEAD)
#   scripts/sync-kit.sh --check <kit-checkout>    verify the vendored tree against kit.lock's rev
#
# Bytes come from `git show <rev>:…`, never from the working tree, so a dirty kit checkout
# cannot leak into the pin. frontend/tests/kit.pin.test.js holds the lock to the tree offline.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
dest="$here/frontend/src/lib/kit"
lock="$here/frontend/kit.lock"
assets="crates/dumpster-kit-ui/assets"
# What a consumer takes. The kit's own tests, package.json and tests/support stay in the kit.
paths=(tokens.css tokens.ts contrast-pairs.json)

check=0
if [[ "${1:-}" == "--check" ]]; then check=1; shift; fi
kit="$(cd "${1:?usage: sync-kit.sh [--check] <kit-checkout> [<rev>]}" && pwd)"
if ((check)); then
  rev="$(python3 -I -c 'import json,sys;print(json.load(open(sys.argv[1]))["rev"])' "$lock")"
else
  rev="$(git -C "$kit" rev-parse "${2:-HEAD}^{commit}")"
fi

files=("${paths[@]}")
while IFS= read -r f; do files+=("${f#"$assets/"}"); done \
  < <(git -C "$kit" ls-tree -r --name-only "$rev" -- "$assets/components")

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
for f in "${files[@]}"; do
  mkdir -p "$out/$(dirname "$f")"
  git -C "$kit" show "$rev:$assets/$f" >"$out/$f"
done

if ((check)); then
  diff -r "$out" "$dest" && echo "kit: frontend/src/lib/kit matches ${rev}"
  exit
fi

rm -rf "$dest"
mkdir -p "$dest"
cp -a "$out/." "$dest/"
python3 -I - "$dest" "$rev" >"$lock" <<'PY'
import hashlib, json, os, sys
dest, rev = sys.argv[1], sys.argv[2]
files = {}
for root, _, names in os.walk(dest):
    for n in names:
        p = os.path.join(root, n)
        files[os.path.relpath(p, dest)] = hashlib.sha256(open(p, "rb").read()).hexdigest()
print(json.dumps({"kit": "https://github.com/DeckDumpster/dumpster-kit", "rev": rev,
                  "files": dict(sorted(files.items()))}, indent="\t"))
PY
echo "kit: vendored ${rev} (${#files[@]} files)"
