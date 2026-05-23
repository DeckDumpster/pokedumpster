# PokeDumpster container image — multi-stage build.
#
# Build:  podman build -t pkdump:latest -f Containerfile .
# The image runs `pkdump serve`; populate the catalog with deploy/seed.sh.

# --- Stage 1: Rust build ----------------------------------------------------
FROM docker.io/library/rust:1.94-slim AS builder

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
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --locked --bin pkdump \
 && mkdir -p /out \
 && cp target/release/pkdump /out/pkdump

# --- Stage 2: SvelteKit build -----------------------------------------------
# adapter-static emits the SPA to frontend/build. The committed ts-rs types in
# frontend/src/lib/types/ mean this stage needs nothing from the Rust build.
FROM docker.io/library/node:22-slim AS frontend

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
COPY --from=frontend /app/frontend/build /srv/pkdump/static

# Catalog + per-user databases live on a mounted volume.
ENV PKDUMP_HOME=/data
# Directory holding the built SvelteKit SPA, served by `pkdump serve`.
ENV PKDUMP_STATIC_DIR=/srv/pkdump/static
EXPOSE 8080

ENTRYPOINT ["pkdump", "serve", "--host", "0.0.0.0", "--port", "8080"]
