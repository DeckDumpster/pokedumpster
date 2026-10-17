#!/usr/bin/env bash
# Every Rust binary the deploy scripts and units name must be built by the
# Containerfile's builder stage, copied to /out, and COPYed into the runtime
# image (sp-vxa4l8). pkdump-lakejobs' three bins landed on master but the
# Containerfile never built them, so prod could not run them.
#
# A name is checked only if it is a bin this workspace defines, so the Python
# modules of the lake image (pkdump-lake-value-snapshots) are not demanded here.
set -euo pipefail
export LC_ALL=C
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_DIR"
fail=0

# Bins the workspace defines: src/bin/<name>.rs files and [[bin]] names.
defined=$( { ls crates/*/src/bin/*.rs 2>/dev/null | sed 's|.*/||; s|\.rs$||'
             grep -h -A1 '^\[\[bin\]\]' crates/*/Cargo.toml | sed -n 's/^name *= *"\(.*\)"/\1/p'; } | sort -u)
[ -n "$defined" ] || { echo "FAIL: found no workspace bins" >&2; exit 1; }

# Names referenced by deploy scripts and units.
named=$(grep -rhoE 'pkdump-(lake-)?[a-z][a-z-]*' deploy/*.sh deploy/*.service deploy/*.container deploy/*.timer 2>/dev/null | sort -u)

# Every lakejobs bin is required regardless of being named: it is the crate's purpose.
required=$( { comm -12 <(echo "$defined") <(echo "$named"); ls crates/pkdump-lakejobs/src/bin/*.rs | sed 's|.*/||; s|\.rs$||'; } | sort -u)

for b in $required; do
    grep -qE -- "--bin $b( |\\\\|$)" Containerfile || { echo "FAIL: $b not built (--bin) in Containerfile" >&2; fail=1; }
    grep -qF -- "cp target/release/$b /out/$b" Containerfile || { echo "FAIL: $b not copied to /out" >&2; fail=1; }
    grep -qF -- "COPY --from=builder /out/$b /usr/local/bin/$b" Containerfile || { echo "FAIL: $b not installed in the runtime image" >&2; fail=1; }
done
[ "$fail" = 0 ] && echo "ok: image ships $(echo "$required" | wc -w) named binaries"
exit "$fail"
