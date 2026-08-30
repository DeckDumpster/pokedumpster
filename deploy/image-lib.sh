#!/usr/bin/env bash
#
# Materialising the shipped image — build it, or name one that already exists.
#
# Five gates in deploy/ci.sh need the image built from this checkout's
# Containerfile, each under its own tag so its teardown cannot untag another
# tier's: the container gate (`pkdump:<ci-instance>`, via deploy/setup.sh), the
# upgrade gate (`pkdump:upgrade-*`), the tenant-header gate (`pkdump:handles-*`),
# the schema-version gate (`pkdump:<sv-instance>`, via setup.sh again) and the
# refresh gate (`pkdump:refresh-*`). Every one of them ran its own `podman
# build`, so one CI run invoked the builder five times over identical content.
#
# Podman's layer cache made four of those cheap rather than free — measured on
# the CI box: a build whose sources changed costs 5m23s, the repeats over
# identical content ~4s each. What the cache does NOT do is guarantee it: it is
# one `podman image prune`, one store teardown or one cold box away from handing
# the run five compiles instead of one (7m27s each with the layer cache dropped
# and the cargo mount cache still warm). Building once and tagging makes that
# structural instead of lucky, and it is why this is a single function rather
# than five copies of a `podman build` line.
#
# The contract:
#
#   PKDUMP_PREBUILT_IMAGE unset  — build from <repo_dir>/Containerfile, exactly
#                                  what every caller did before. This is prod's
#                                  path and the path a tier run by hand takes,
#                                  so a gate remains standalone-runnable.
#   PKDUMP_PREBUILT_IMAGE set    — <tag> becomes another name for that image. No
#                                  builder runs.
#
# Set-but-missing is a FAILURE, not a quiet rebuild. A silent fallback would
# turn "the build-once wiring broke" into "CI got slower again", which is the
# kind of regression nobody files.
#
# Tagging is safe against the teardowns: `podman rmi <tag>` on an image carrying
# several names untags rather than deletes, so the ci instance's teardown cannot
# pull the image out from under the tier after it.

# pkdump_image_ensure <tag> <repo_dir>
#
# Leaves <tag> naming an image built from <repo_dir>/Containerfile. Stdout is
# the builder's, so a caller that wants it quiet redirects.
pkdump_image_ensure() {
	local tag="$1" repo_dir="$2"
	local prebuilt="${PKDUMP_PREBUILT_IMAGE:-}"

	if [ -z "$prebuilt" ]; then
		# Scope the cargo target cache to THIS checkout (pd-sjn7). One box runs
		# the rig root, a CI runner and any number of polecat worktrees; with a
		# constant cache id they share one target directory, and cargo
		# fingerprints do not record which checkout produced the objects, so a
		# neighbour's merely-newer rlib gets reused and the build fails on
		# whatever it does not export — or, worse, links cleanly and ships old
		# behaviour. The same sha1-of-the-path suffix every container gate
		# already derives for its network, volume and image names.
		#
		# Every gate in one ci.sh run shares a checkout, so they still share the
		# cache: this removes the cross-checkout hazard without giving up the
		# build-once-and-tag benefit the id exists for.
		local scope
		scope="$(printf '%s' "$repo_dir" | sha1sum | cut -c1-8)"
		pkdump_image_build_collecting -t "$tag" \
			--build-arg "CARGO_TARGET_CACHE_SCOPE=${scope}" \
			-f "${repo_dir}/Containerfile" "$repo_dir"
		return
	fi

	if ! podman image exists "$prebuilt"; then
		echo "ERROR: PKDUMP_PREBUILT_IMAGE=${prebuilt} names no image in this store." >&2
		echo "       It is set by deploy/ci.sh after it builds the image once; if you" >&2
		echo "       are running this gate by hand, unset it and the Containerfile at" >&2
		echo "       ${repo_dir}/Containerfile will be built instead." >&2
		return 1
	fi

	podman tag "$prebuilt" "$tag"
}

# --- Collecting what a build orphaned ---------------------------------------
#
# A build leaves litter behind, and until pd-h3wy nothing on the box collected
# it. Two sources, both structural:
#
#   * a multi-stage build's STAGE images are never tagged, so every one of them
#     is untagged the moment the build ends — 1.62 GB for the Rust builder here;
#   * re-pointing a tag leaves the image it USED to name unreachable.
#
# Neither belongs to anyone afterwards. Measured on the deployment box: 5.1 GB
# of them in prod's default store from three builds, 3.6 GB in the non-prod
# store from two CI runs — roughly 2 GB per build, accumulating forever on the
# filesystem prod runs from. That is what took / to 91% and stopped the whole
# container tier (pd-h3wy). pd-5aba's rule is the complement of this one: it is
# about the tag a gate NAMES, this is about the layers a tag stopped pointing at.
#
# THE RULE: a build collects what the build BEFORE it orphaned.
#
# Not its own orphans — those are the layer cache. Collecting them drops the
# last reference to the intermediates and podman cascades them away (measured:
# a store went 45M -> 5M and the next build recompiled), which is exactly the
# "five compiles instead of one" regression this file exists to prevent. By the
# time the NEXT build runs, its own predecessor holds those layers, so removing
# the older generation frees only what has genuinely diverged. Measured over
# four consecutive builds: store size flat, cache hits unchanged. One
# generation of litter is the steady state; unbounded growth was the bug.
#
# AND IT IS CONFINED BY LABEL, which is what makes it safe to run in prod's
# store. That store is shared — another project's images and another project's
# dangling layers live in it — so `podman image prune` is not ours to run there.
# Every image this repo builds carries `pkdump.build=1` (Containerfile,
# lake/Containerfile, stage images included); a dangling image without it is
# somebody else's and is never touched.

# The label every image built from this repo carries. One spelling, spent by the
# filter below and asserted against both Containerfiles by tests/deploy/run.sh.
PKDUMP_IMAGE_LABEL="pkdump.build=1"

# pkdump_image_orphans
#
# The dangling images this repo left in the active store, ids only. Note that
# `-f dangling=true` does NOT list build-cache intermediates — only the final
# image of each stage — which is why removing what it returns is not the same
# as dropping the cache.
pkdump_image_orphans() {
	podman images -f dangling=true -f "label=${PKDUMP_IMAGE_LABEL}" -q 2>/dev/null || true
}

# pkdump_image_orphans_reap <id>...
#
# Never `-f`: an image something still holds refuses to go and that is the right
# answer, not a thing to force past. A removal that fails does not fail the
# caller either — the build succeeded, and this is housekeeping after it.
pkdump_image_orphans_reap() {
	[ "$#" -gt 0 ] || return 0
	echo "==> Collecting $# orphaned image layer(s) from the previous build."
	local id
	for id in "$@"; do
		podman rmi "$id" >/dev/null 2>&1 || true
	done
}

# pkdump_image_build_collecting <podman build arg>...
#
# `podman build`, with the previous build's orphans collected once this one has
# succeeded. Every builder invocation in deploy/ goes through here.
pkdump_image_build_collecting() {
	local -a stale=()
	mapfile -t stale < <(pkdump_image_orphans)
	podman build "$@" || return
	pkdump_image_orphans_reap "${stale[@]}"
}
