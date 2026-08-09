#!/usr/bin/env bash
#
# Remove a PokeDumpster non-prod container store (pd-yfev).
#
# deploy/teardown.sh removes an INSTANCE — its unit, its image, its volume — and
# deliberately leaves the store that instance lived in alone, because the store
# is shared by every instance on the box. Nothing removed the store itself, so a
# box that opted into an alternate store accumulated one forever: 3.9G of images
# and layers on the machine this was written on, plus a runroot per store under
# /run/user/$UID that no script ever collected.
#
# This is that missing command. It removes the store's containers and images,
# its graph root, its Buildah TMPDIR, its runroot and its rootless-netns name.
#
# Usage:
#   bash deploy/store-teardown.sh              # the store ~/.config/pkdump/store.env names
#   PKDUMP_STORE_ROOT=/some/dir bash deploy/store-teardown.sh
#
# It will NOT touch Podman's default store — that is the one prod runs in, and
# with no alternate store configured this exits non-zero rather than defaulting
# to it. It also does not use `podman system reset`.
#
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=deploy/store-lib.sh
. "$SCRIPT_DIR/store-lib.sh"

# Same resolution order as deploy/ci.sh: an explicit PKDUMP_STORE_ROOT wins, the
# host's store.env answers when the caller said nothing, and answered nowhere
# means the default store — which pkdump_store_teardown refuses.
pkdump_store_load_config

if [ -z "${PKDUMP_STORE_ROOT:-}" ]; then
    echo "No alternate container store is configured for this box."
    echo "  (nothing in \$PKDUMP_STORE_ROOT or ~/.config/pkdump/store.env)"
    echo ""
    echo "There is nothing to remove: unconfigured means every instance uses"
    echo "Podman's default store, which is prod's and is not this script's to delete."
    exit 1
fi

echo "==> Store to remove: ${PKDUMP_STORE_ROOT}"
if [ -d "${PKDUMP_STORE_ROOT}/storage" ]; then
    echo "    (currently $(du -sh "${PKDUMP_STORE_ROOT}" 2>/dev/null | cut -f1) on disk)"
fi

pkdump_store_teardown

echo "==> Store removed. The next deploy/ci.sh run rebuilds it from scratch."
