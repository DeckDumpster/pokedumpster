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
# The target cache is scoped by `id=` to the builder's Debian release, and that
# is load-bearing: cargo fingerprints record crate versions and rustc version,
# NOT which base image produced the objects. When the base drifted (pd-pejn),
# a shared cache handed the next build the objects compiled on trixie, cargo
# relinked nothing, and `cp` copied the unrunnable binary straight through — so
# re-pinning the FROM line above looked applied and changed nothing, on every
# box that had already built once. A base-image change must change this id.
#
# TWO binaries, built in one invocation so they share the compile:
#
#   pkdump             the app. `serve` is the entrypoint; `data refresh` is
#                      what the nightly LANDING unit runs. Reads no raw/.
#   pkdump-lake-derive the offline catalog derive (pd-1uem). The only thing in
#                      the image that reads raw/, and the only thing that can:
#                      it is a bin-only crate, so `pkdump` cannot link it.
#
# They ship in ONE image deliberately, even though the whole point of the crate
# split is that the two halves can eventually run on different machines. The
# image is not the boundary — the process is. Shipping one image keeps the
# derive on exactly the build that wrote the schema it derives into, which is
# what stops an offline job and an online server disagreeing about the catalog's
# shape; splitting the images is a deployment change to make when the machines
# actually split, not before.
# LTO is a BUILD ARG so CI can turn it off and production cannot accidentally.
#
# `[profile.release] lto = "thin"` runs a whole-program pass at link time over both
# binaries on EVERY build. It is not incremental, so no cache mount helps it, and it
# is a large part of why the image build costs 850-1119s on this box — a 4-core 6 W
# N150 — against 373 packages in Cargo.lock.
#
# Defaulting to `thin` is deliberate: an unset arg builds exactly what shipped
# before, so nothing changes for anyone who does not pass it.
#
# SAFE FOR CI SPECIFICALLY because CI's image is not the artifact. deploy/deploy.sh
# runs its OWN `podman build` on the prod box (line ~45), so what reaches production
# is unaffected by anything set here. The trade CI accepts is testing a
# differently-optimised binary: same code, same behaviour, slower hot paths — which
# does not change what any gate asserts.
ARG CARGO_PROFILE_RELEASE_LTO=thin
ENV CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO}

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target,sharing=locked,id=pkdump-target-bookworm \
    cargo build --release --locked --bin pkdump --bin pkdump-lake-derive \
 && mkdir -p /out \
 && cp target/release/pkdump /out/pkdump \
 && cp target/release/pkdump-lake-derive /out/pkdump-lake-derive

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
COPY --from=frontend /app/frontend/build /srv/pkdump/static

# Catalog + per-user databases live on a mounted volume.
ENV PKDUMP_HOME=/data
# Directory holding the built SvelteKit SPA, served by `pkdump serve`.
ENV PKDUMP_STATIC_DIR=/srv/pkdump/static
EXPOSE 8080

ENTRYPOINT ["pkdump", "serve", "--host", "0.0.0.0", "--port", "8080"]
