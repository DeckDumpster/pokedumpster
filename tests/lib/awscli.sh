#!/usr/bin/env bash
# awscli.sh — the ONE definition of the AWS CLI image every gate runs.
#
# WHY THIS FILE EXISTS
#
# Three gates named `docker.io/amazon/aws-cli:latest` as their own literal.
# This is exactly the shape of pd-pfxf, where `litestream:latest` moving
# 0.5.16 -> 0.5.17 overnight turned three gates red on a change that touched
# none of them. The lesson was written down for the Litestream image and for
# the MinIO images (tests/lib/minio.sh), and not generalised here — so it
# had to be paid again when `latest` moves.
#
# tenant_zone.sh §2 drives put/get/delete-bucket-lifecycle through this image
# and asserts the exact NoSuchLifecycleConfiguration error path that
# setup-tenant-zone.sh's exit 3 depends on; drill.sh and recreate.sh list a
# real bucket through it. An output or error-message change in a new aws-cli
# reads as an object-store failure in all three.
#
# Callers may override for a one-off experiment
# (`AWSCLI_IMAGE=… bash tests/lake/tenant_zone.sh`); nothing under tests/ may
# HARDCODE another value, which is what tests/lib/objects_test.sh asserts.

PKDUMP_AWSCLI_VERSION="2.37.3"
PKDUMP_AWSCLI_IMAGE="docker.io/amazon/aws-cli:${PKDUMP_AWSCLI_VERSION}"
