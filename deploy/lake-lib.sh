#!/usr/bin/env bash
#
# The lake network, from outside a job (pd-p39v). Sourced, never executed.
#
# deploy/prices.sh and deploy/value-snapshots.sh are two wrappers around one
# network: the same Nessie, reached by the same name, over the same user-defined
# bridge. What they have to agree about lives here rather than being spelled out
# twice — the second copy is where a URL drifts.
#
#   pkdump_lake_catalog_uri <instance>          the Iceberg REST endpoint
#   pkdump_lake_health_url <catalog-uri>        that service's config document
#   pkdump_lake_catalog_answering <network> <image> <url>
#                                               does it answer, asked from where
#                                               the job will ask
#
# WHY A READINESS PROBE IS PART OF THE DEPLOY SURFACE AT ALL.
#
# `pkdump_store_netns_ensure` repairs a wedged rootless netns by dropping the
# namespace and restarting what was on it — for the lake network, Nessie. But
# `systemctl --user restart` returns when the CONTAINER is running, and Nessie is
# a JVM that will not answer for another 30-40 seconds. The repair used to return
# there, the job started immediately, and it died on a connection error to a
# service that was coming up fine. By the next run everything was healthy, so the
# unit paged for a condition that had already fixed itself. These three functions
# are what let the repair wait for ANSWERING instead of RESTARTED.
#
# The probe runs where the job runs — a throwaway container ON the network,
# resolving the catalog by name. A host-side curl would take the published-port
# path instead, and can say yes about a network the job itself cannot use. It
# needs nothing the run does not already have: the job image is Python and
# urllib is in its standard library.

# The Iceberg REST endpoint the jobs talk to. PKDUMP_LAKE_NESSIE_URI is how
# lake.env (and every test substrate) redirects it; the default is the name
# deploy/setup-lake.sh gives the container.
pkdump_lake_catalog_uri() {
    printf '%s' "${PKDUMP_LAKE_NESSIE_URI:-http://pkdump-nessie-${1}:19120/iceberg/}"
}

# The same service's own config document — always present, needs no catalog, no
# warehouse and no credentials, and is the cheapest thing that proves the JVM is
# serving rather than merely running. DERIVED from the catalog URI, so a test
# that redirects the catalog redirects the health check with it and cannot end up
# waiting on a Nessie that is not the one the job will use.
pkdump_lake_health_url() {
    printf '%s' "${PKDUMP_LAKE_HEALTH_URL:-${1%%/iceberg/*}/api/v2/config}"
}

# Exits 0 when the catalog answers. Quiet: it is polled, and a caller that wanted
# the failure text would be reading 40 identical copies of it.
pkdump_lake_catalog_answering() {
    podman run --rm --network "$1" "$2" python -c \
        'import sys, urllib.request; urllib.request.urlopen(sys.argv[1], timeout=5).read()' \
        "$3" >/dev/null 2>&1
}
