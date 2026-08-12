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
		podman build -t "$tag" -f "${repo_dir}/Containerfile" "$repo_dir"
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
