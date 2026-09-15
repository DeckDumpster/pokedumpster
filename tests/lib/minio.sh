#!/usr/bin/env bash
# minio.sh — the ONE definition of the MinIO images every object-store gate runs.
#
# WHY THIS FILE EXISTS
#
# Eleven gates named `docker.io/minio/minio:latest` and nine named
# `docker.io/minio/mc:latest`, each its own literal. On 2026-09-15 that stopped
# working outright: `minio/minio` and `minio/mc` on Docker Hub began answering
# an anonymous manifest request with 401 for EVERY tag, `latest` included. The
# first ephemeral CI run after that pulled nothing, and the gates did not fail
# cleanly — they carried on without an object store and one of them sat in a
# wait until the job hit its 90-minute cap.
#
# This is the same shape as the Litestream pin (pd-pfxf), which exists because
# `litestream:latest` moved 0.5.16 -> 0.5.17 overnight and turned three gates red
# on a change that touched none of them. The lesson was written down for one
# image and not generalised to the others, so it had to be paid twice. See
# deploy/litestream-lib.sh for that pin and tests/litestream/image_pin_test.sh
# for the gate that holds it.
#
# TWO DECISIONS, and they are separate:
#
#   Registry. quay.io, not Docker Hub. MinIO publishes the same builds to both
#   and quay.io still serves them anonymously. The alternative is a Docker Hub
#   credential, which would put a secret in the path of every gate and every
#   developer checkout to fetch a public image — a real cost for no capability,
#   and one more thing a fresh VM has to be given.
#
#   Version. Pinned, not `latest`, independent of the registry. The tags below
#   are the digests `latest` pointed at when this was written; a gate that rides
#   a moving tag tests whatever upstream pushed last night rather than anything
#   this repository chose. Bumping one is an edit here and nowhere else.
#
# Callers may override for a one-off experiment
# (`MINIO_IMAGE=… bash tests/lake/run.sh`); nothing under tests/ may HARDCODE
# another value, which is what tests/lib/objects_test.sh asserts over the tree.

PKDUMP_MINIO_VERSION="RELEASE.2025-09-07T16-13-09Z"
PKDUMP_MC_VERSION="RELEASE.2025-08-13T08-35-41Z"
PKDUMP_MINIO_IMAGE="quay.io/minio/minio:${PKDUMP_MINIO_VERSION}"
PKDUMP_MC_IMAGE="quay.io/minio/mc:${PKDUMP_MC_VERSION}"
