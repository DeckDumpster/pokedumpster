# PokeDumpster container image — multi-stage Rust build.
#
# Build:  podman build -t pkdump:latest -f Containerfile .
# The image runs `pkdump serve`; populate the catalog with deploy/seed.sh.

# --- Stage 1: build ---------------------------------------------------------
FROM rust:1.94-slim AS builder

WORKDIR /app

# Workspace manifests, sources, and the embedded override data
# (data/overrides/*.json is pulled in at compile time via include_str!).
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY data/ data/

RUN cargo build --release --locked --bin pkdump

# --- Stage 2: runtime -------------------------------------------------------
FROM debian:bookworm-slim

# ca-certificates for HTTPS during `pkdump setup`. No OpenSSL needed —
# reqwest is built with rustls.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/pkdump /usr/local/bin/pkdump

# Catalog + per-user databases live on a mounted volume.
ENV PKDUMP_HOME=/data
EXPOSE 8080

ENTRYPOINT ["pkdump", "serve", "--host", "0.0.0.0", "--port", "8080"]
