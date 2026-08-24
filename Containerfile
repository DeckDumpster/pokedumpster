# PokeDumpster container image — multi-stage build.
#
# Build:  podman build -t pkdump:latest -f Containerfile .
# The image runs `pkdump serve`; populate the catalog with deploy/seed.sh.

# --- Stage 1: Rust build ----------------------------------------------------
# The Debian release is pinned, not inherited. `rust:1.94-slim` is a MOVING
# tag: upstream retagged it from bookworm to trixie, the builder started
# linking against glibc 2.39, and every image built after that shipped a
# binary the bookworm runtime stage below cannot exec (pd-pejn). The invariant
# is builder glibc <= runtime glibc, and the only thing that holds it is these
# two FROM lines naming the same release. Change one, change the other — and
# the target cache id below with them.
FROM docker.io/library/rust:1.94-slim-bookworm AS builder

WORKDIR /app

# Workspace manifests, sources, and the embedded override data
# (data/overrides/*.json is pulled in at compile time via include_str!).
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY data/ data/

# BuildKit cache mounts: the cargo registry + the workspace's target/ both
# persist across rebuilds, so a small Rust change incrementally compiles in
# seconds instead of triggering a 3-4 minute release rebuild from scratch.
# `cp` the binary out at the end because the cache mount disappears after
# the RUN, taking target/release/pkdump with it.
#
# The target cache is scoped by `id=` on TWO axes, and both are load-bearing for
# the same reason: cargo fingerprints record crate versions, rustc version and
# source mtimes — they do NOT record which base image or which CHECKOUT produced
# the objects.
#
#   * the builder's DEBIAN RELEASE. When the base drifted (pd-pejn), a shared
#     cache handed the next build the objects compiled on trixie, cargo relinked
#     nothing, and `cp` copied the unrunnable binary straight through — so
#     re-pinning the FROM line above looked applied and changed nothing, on every
#     box that had already built once. A base-image change must change this id.
#
#   * the CHECKOUT (pd-sjn7). One box runs the rig root, a CI runner and any
#     number of polecat worktrees, every one of them building /app from its own
#     sources. With a constant id they share one target directory, and a worktree
#     whose rlib is merely NEWER wins: the next build reuses it, relinks nothing,
#     and fails on whatever that rlib does not export — while COPY demonstrably
#     delivered the right sources, which is a maddening thing to debug. Worse, a
#     stale rlib that still exports everything referenced links CLEANLY and ships
#     old behaviour, so a gate can go green against code nobody wrote.
#
# CARGO_TARGET_CACHE_SCOPE is that second axis. `deploy/image-lib.sh` passes a
# hash of the checkout path — the same per-checkout suffix every container gate
# already derives for its network, volume and image names — so each tree keeps
# its own warm cache and can poison nobody else's. Every gate in ONE ci.sh run
# shares a checkout and therefore still shares the cache, which is the whole
# benefit the id was introduced to have.
#
# The default is deliberately not a hash: a bare `podman build` cannot compute
# one, so it says so instead of silently claiming a scope it does not have.
#
# FOUR binaries, built in one invocation so they share the compile:
#
#   pkdump             the app. `serve` is the entrypoint; `data refresh` is
#                      what the nightly LANDING unit runs. Reads no raw/.
#   pkdump-lake-derive the offline catalog derive (pd-1uem). The only thing in
#                      the image that reads raw/, and the only thing that can:
#                      it is a bin-only crate, so `pkdump` cannot link it.
#   pkdump-ship        the shipper (pd-dxn3): the ownership outbox into the
#                      tenant zone. Offline, like the derive — nothing serving
#                      a request runs it, and the entrypoint above cannot
#                      become it by accident.
#   pkdump-erase       the deletion path (pd-qbrf): tombstone the key, drop the
#                      tenant's partition, and prove it unreadable. Offline for
#                      the same two reasons the shipper is — it needs the tenant
#                      credentials and the master key, and neither belongs on a
#                      process that answers requests.
#
# They ship in ONE image deliberately, even though the whole point of the crate
# split is that the halves can eventually run on different machines. The image
# is not the boundary — the process is, and so are the credentials each process
# is handed. Shipping one image keeps every offline job on exactly the build
# that wrote the schema it reads, which is what stops an offline job and an
# online server disagreeing about the shape of a database; splitting the images
# is a deployment change to make when the machines actually split, not before.
ARG CARGO_TARGET_CACHE_SCOPE=unscoped
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target,sharing=locked,id=pkdump-target-bookworm-${CARGO_TARGET_CACHE_SCOPE} \
    cargo build --release --locked --bin pkdump --bin pkdump-lake-derive \
        --bin pkdump-ship --bin pkdump-erase \
 && mkdir -p /out \
 && cp target/release/pkdump /out/pkdump \
 && cp target/release/pkdump-lake-derive /out/pkdump-lake-derive \
 && cp target/release/pkdump-ship /out/pkdump-ship \
 && cp target/release/pkdump-erase /out/pkdump-erase

# --- Stage 2: SvelteKit build -----------------------------------------------
# adapter-static emits the SPA to frontend/build. The committed ts-rs types in
# frontend/src/lib/types/ mean this stage needs nothing from the Rust build.
# Pinned to a release for the same reason as the builder, though this stage is
# far less exposed — it emits static files, not a linked binary.
FROM docker.io/library/node:22-bookworm-slim AS frontend

WORKDIR /app/frontend

COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci

COPY frontend/ ./
RUN npm run build

# --- Stage 3: runtime -------------------------------------------------------
FROM docker.io/library/debian:bookworm-slim

# ca-certificates for HTTPS during `pkdump setup` / `pkdump data refresh`. No
# OpenSSL needed — reqwest is built with rustls.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /out/pkdump /usr/local/bin/pkdump
COPY --from=builder /out/pkdump-lake-derive /usr/local/bin/pkdump-lake-derive
COPY --from=builder /out/pkdump-ship /usr/local/bin/pkdump-ship
COPY --from=builder /out/pkdump-erase /usr/local/bin/pkdump-erase
COPY --from=frontend /app/frontend/build /srv/pkdump/static

# Catalog + per-user databases live on a mounted volume.
ENV PKDUMP_HOME=/data
# Directory holding the built SvelteKit SPA, served by `pkdump serve`.
ENV PKDUMP_STATIC_DIR=/srv/pkdump/static
EXPOSE 8080

ENTRYPOINT ["pkdump", "serve", "--host", "0.0.0.0", "--port", "8080"]
