#!/usr/bin/env bash
#
# Deploy the image CI already built and tested, instead of compiling Rust on the
# prod host (sp-0ypc3x). Merge-to-live was 6-14 minutes, all of it rustc on the
# box that serves prod, and two repos deploying at once starved each other.
#
# The gate job exports the image it tested as the artifact
# `pkdump-image-<tree-hash>` (deploy/ci.sh, PKDUMP_CI_IMAGE_OUT). The key is the
# TREE, not the commit: the queue's merge commit is new but its tree is the one
# the batch PR passed. This script finds that artifact, loads it, checks the
# loaded image id against the one the gate recorded, and hands it to
# the deploy script as PKDUMP_PREBUILT_IMAGE — so unit files, restart, sidecar
# go through the one path they always did, minus the
# compile.
#
# No artifact for the tree is a FAILURE, not a quiet rebuild: a fallback to
# compiling on prod would put this slowness back without anyone filing it. The
# one exception is a push that changed nothing the app image contains (no
# container tier selected by deploy/ci-select.sh — docs, lake/), where CI builds
# no image by design; the app image already running is re-used as it is.
#
# Usage: bash deploy/ship-image.sh <instance> <sha> [<before-sha>]
# Env:   GITHUB_TOKEN (actions:read), GITHUB_REPOSITORY, GITHUB_API_URL
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

[ $# -ge 2 ] || { echo "Usage: bash deploy/ship-image.sh <instance> <sha> [<before-sha>]" >&2; exit 1; }
INSTANCE="$1"
SHA="$2"
BEFORE="${3:-}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
API="${GITHUB_API_URL:-https://api.github.com}"
DEPLOY="$SCRIPT_DIR/deploy.sh"

TREE="$(git -C "$REPO_DIR" rev-parse "${SHA}^{tree}")"
NAME="pkdump-image-${TREE}"
echo "==> Deploying ${SHA:0:12} (tree ${TREE:0:12}) to '${INSTANCE}' — looking for artifact ${NAME}"

api() {
    curl --fail-with-body -sS -H "Authorization: Bearer ${GITHUB_TOKEN}" \
        -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28" "$@"
}

url="$(api "${API}/repos/${GITHUB_REPOSITORY}/actions/artifacts?name=${NAME}&per_page=10" \
    | jq -r '[.artifacts[] | select(.expired == false)] | first | .archive_download_url // empty')"

if [ -z "$url" ]; then
    # Only a push that cannot have changed the image may go without one. An
    # unknown base (first push, force-push) is "we do not know" and fails.
    if [ -n "$BEFORE" ] && [ -n "${BEFORE//0/}" ] && git -C "$REPO_DIR" cat-file -e "${BEFORE}^{commit}" 2>/dev/null; then
        # shellcheck source=deploy/ci-select.sh
        . "$SCRIPT_DIR/ci-select.sh"
        tiers="$(git -C "$REPO_DIR" diff --no-renames --name-only "$BEFORE" "$SHA" | pkdump_ci_select_tiers)"
        if ! grep -qx container <<<"$tiers"; then
            echo "==> No artifact, and nothing this push changed is in the app image:"
            echo "    re-using the running image pkdump:${INSTANCE}."
            PKDUMP_PREBUILT_IMAGE="localhost/pkdump:${INSTANCE}"
            export PKDUMP_PREBUILT_IMAGE
            exec bash "$DEPLOY" "$INSTANCE"
        fi
    fi
    echo "ERROR: no CI artifact ${NAME}: this tree was never built and tested by the gate." >&2
    echo "       Refusing to compile on the prod host. Run CI on this tree (workflow_dispatch" >&2
    echo "       of ci.yml on the branch) and re-run this deploy." >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
api -L -o "$work/a.zip" "$url"
python3 -I -m zipfile -e "$work/a.zip" "$work"

want="$(cat "$work/image.id")"
zstd -dc "$work/image.tar.zst" | podman load -q >/dev/null
if ! podman image exists "$want"; then
    echo "ERROR: loaded artifact does not contain image ${want} recorded by the gate." >&2
    exit 1
fi
echo "==> Loaded image ${want:0:19} — the one the gate tested."

PKDUMP_PREBUILT_IMAGE="$want"
export PKDUMP_PREBUILT_IMAGE
exec bash "$DEPLOY" "$INSTANCE"
