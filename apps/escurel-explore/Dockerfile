# syntax=docker/dockerfile:1.7
#
# Multi-stage build for the escurel-explore companion app, served by the
# escurel-explore-bff auth-bridging reverse proxy (NOT nginx). The browser
# SPA cannot hold a signing key, so the BFF mints a short-lived OIDC JWT on
# each POST /mcp and forwards to escurel; it publishes its public key at
# /jwks.json for escurel to trust.
#
# Stage 1 (Flutter): build the web bundle in HTTP mode. Base URL is left
#   empty → the SPA uses same-origin (it talks to the BFF that serves it),
#   and auth stays `none` because the BFF injects the bearer server-side.
# Stage 2 (Rust): build the `escurel-explore-bff` binary. This crate does
#   NOT depend on duckdb / escurel-server, so it compiles fast.
# Runtime: debian-slim with the bff binary + the bundle at
#   /usr/share/escurel-explore/web. Listens on :8080.
#
# IMPORTANT: the build context MUST be the repo root (`context: .`) so the
# Rust workspace sources are available to stage 2. Build:
#   docker build -t escurel-explore:local \
#     -f apps/escurel-explore/Dockerfile \
#     --build-arg BUILD_VERSION=$(git rev-parse --short HEAD) .
#
# Run locally:
#   docker run --rm -p 8080:8080 \
#     -e EXPLORE_BFF_ESCUREL_BACKEND=http://host.docker.internal:8081 \
#     -e EXPLORE_BFF_ISSUER=http://localhost:8080 \
#     escurel-explore:local
#   curl localhost:8080/healthz

# --- stage 1: Flutter web bundle (http mode, same-origin) -------------
FROM ghcr.io/cirruslabs/flutter:3.44.0 AS flutter

WORKDIR /src
COPY apps/escurel-explore/pubspec.yaml apps/escurel-explore/pubspec.lock ./
RUN flutter pub get

COPY apps/escurel-explore/ ./

ARG BUILD_VERSION=dev
RUN flutter build web --release \
      --dart-define=ESCUREL_EXPLORE_MODE=http \
      --dart-define=ESCUREL_EXPLORE_VERSION=${BUILD_VERSION}

# --- stage 2: Rust BFF binary -----------------------------------------
FROM rust:1.88-slim-bookworm AS rust

WORKDIR /build
# Bring the whole cargo workspace so path-deps + the workspace manifest
# resolve. The build context is the repo root (see header).
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY examples/ ./examples/

# Build only the BFF (and its path deps). No duckdb / escurel-server, so
# this is a fast compile relative to the full workspace.
RUN cargo build --release -p escurel-explore-bff \
 && cp target/release/escurel-explore-bff /escurel-explore-bff

# --- runtime ----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/*

COPY --from=rust /escurel-explore-bff /usr/local/bin/escurel-explore-bff
COPY --from=flutter /src/build/web /usr/share/escurel-explore/web

ENV EXPLORE_BFF_LISTEN=0.0.0.0:8080 \
    EXPLORE_BFF_BUNDLE_DIR=/usr/share/escurel-explore/web

# Substrate liveness contract — dependency-free, fast.
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://localhost:8080/healthz || exit 1

EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/escurel-explore-bff"]
