# Escurel server image (HTTP-only, MCP-over-HTTP + WebSocket on :8080).
#
# Built + published to ghcr by .github/workflows/publish-image.yml so the
# DataZoo Kamal substrate can `kamal deploy --skip-push` it (ADR-0013). The
# server is single-tenant: one container serves one tenant's KB, persisting
# DuckDB + the FsStore lane corpus under $ESCUREL_SERVER_DATA_DIR (mount a
# durable volume there). See crates/escurel-server/src/config.rs for the full
# ESCUREL_* surface.
#
# Built with the `gemini` feature so a deployment can use the HTTP Gemini
# embedder (light: reqwest only — no local model). `zero` (default) and
# `embeddinggemma` (heavy, local candle model) remain selectable at runtime
# via ESCUREL_EMBEDDING_PROVIDER, but `embeddinggemma` needs its own feature
# build + a baked model, so it is intentionally not compiled in here.

# ---- builder -------------------------------------------------------------
# Pinned to the workspace toolchain (rust-toolchain.toml: 1.91.0).
# libduckdb-sys downloads the precompiled libduckdb release instead of
# compiling the bundled DuckDB C++ amalgamation from source (see
# .cargo/config.toml: DUCKDB_DOWNLOAD_LIB=1), so no g++/make is needed
# at build time; reqwest uses rustls (no OpenSSL), so no extra apt is
# required. First clean build needs network to fetch libduckdb.
FROM rust:1.91-bookworm AS builder
WORKDIR /build
COPY . .
# Serialise codegen/link: linking the release binary against libduckdb is
# memory-hungry and OOMs a default-parallelism release+LTO build on a 7 GB CI
# runner (the CI workflow caps this the same way). Release profile already
# strips symbols.
ENV CARGO_BUILD_JOBS=1
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p escurel-server --features gemini \
    && cp target/release/escurel-server /usr/local/bin/escurel-server

# ---- runtime -------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# libstdc++6: the downloaded libduckdb links it dynamically and debian-slim
# does not ship it by default. curl: HEALTHCHECK probe.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/escurel-server /usr/local/bin/escurel-server

# Kamal (the substrate's deployer) asserts at deploy that the image carries a
# `service` label exactly matching the Kamal service name, else it refuses to
# boot it ("missing the 'service' label"). The substrate runs escurel as the
# `dz-escurel` service, so stamp that here. (If escurel is ever deployed under a
# different Kamal service name, override with `docker build --label service=…`.)
LABEL service="dz-escurel"

# Defaults; a deployment overrides via env. Data dir is where the volume mounts.
ENV ESCUREL_SERVER_LISTEN_HTTP=0.0.0.0:8080 \
    ESCUREL_SERVER_DATA_DIR=/data
EXPOSE 8080 9090
VOLUME ["/data"]

# Liveness mirrors what kamal-proxy probes (dependency-free /healthz).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s \
  CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

# Rebuild-on-boot: vss's EXPERIMENTAL HNSW persistence segfaults when a
# restarted process reloads the on-disk index, so treat the DuckDB index as an
# ephemeral derived cache — drop it before every start and let escurel rebuild
# a fresh index from the durable markdown LaneStore (its fresh-boot recovery
# path). The canonical markdown corpus is NEVER touched. Inline (no extra file)
# so editing this Dockerfile doesn't bust the build cache (.dockerignore excludes
# the Dockerfile from the context). Trade-off: re-embeds the corpus at boot.
ENTRYPOINT ["/bin/sh", "-c", "rm -f \"${ESCUREL_SERVER_DATA_DIR:-/data}\"/tenants/*/escurel.duckdb \"${ESCUREL_SERVER_DATA_DIR:-/data}\"/tenants/*/escurel.duckdb.wal 2>/dev/null; exec /usr/local/bin/escurel-server"]
