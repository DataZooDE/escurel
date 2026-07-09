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
# Download mode (.cargo/config.toml: DUCKDB_DOWNLOAD_LIB=1) links libduckdb
# DYNAMICALLY: the binary carries `NEEDED libduckdb.so` with no rpath, so the
# runtime stage must ship the shared object too. Both artifacts are copied out
# of the cache-mounted `target/` in THIS RUN (the mount is not visible to later
# COPY stages), into non-mounted dirs that persist in the builder layer.
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p escurel-server --features gemini \
    && cp target/release/escurel-server /usr/local/bin/escurel-server \
    && cp "$(find target -name libduckdb.so -print -quit)" /usr/local/lib/libduckdb.so

# ---- runtime -------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# libstdc++6: the downloaded libduckdb links it dynamically and debian-slim
# does not ship it by default. curl: HEALTHCHECK probe.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/escurel-server /usr/local/bin/escurel-server
# The dynamically-linked libduckdb.so (see the builder note). Land it in a
# standard search dir and refresh the loader cache so the binary — which has
# no rpath — finds it at startup.
COPY --from=builder /usr/local/lib/libduckdb.so /usr/lib/libduckdb.so
RUN ldconfig

# Kamal (the substrate's deployer) asserts at deploy that the image carries a
# `service` label exactly matching the Kamal service name, else it refuses to
# boot it ("missing the 'service' label"). The substrate runs escurel as the
# `dz-escurel` service, so that is the default. A generic (non-Kamal) build —
# cloud / bare metal / Helm / OpenShift — can drop or rename it with
# `docker build --build-arg SERVICE_LABEL=…` (empty to omit). Keeping the
# default means the published substrate image is unchanged.
ARG SERVICE_LABEL=dz-escurel
LABEL service="${SERVICE_LABEL}"

# Defaults; a deployment overrides via env. Data dir is where the volume mounts.
# ESCUREL_REBUILD_INDEX_ON_BOOT=always: the derived DuckDB is a rebuildable
# cache, so drop + rebuild it from the canonical markdown LaneStore on every
# start. This is the container default because vss's experimental HNSW
# persistence segfaults when a restart reloads the on-disk index. The binary
# handles this itself now (see config.rs) — no shell hack in the entrypoint.
# Fast-restart deployments that never hit the segfault can override this to
# `if-missing`. Trade-off: `always` re-embeds the corpus at boot.
ENV ESCUREL_SERVER_LISTEN_HTTP=0.0.0.0:8080 \
    ESCUREL_SERVER_DATA_DIR=/data \
    ESCUREL_REBUILD_INDEX_ON_BOOT=always
EXPOSE 8080 9090
VOLUME ["/data"]

# Liveness mirrors what kamal-proxy probes (dependency-free /healthz).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s \
  CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

# The derived-index drop-and-rebuild is now handled inside the binary, gated by
# ESCUREL_REBUILD_INDEX_ON_BOOT (set to `always` above). No shell wrapper — exec
# the server directly so it is PID 1 and receives SIGTERM for graceful shutdown.
ENTRYPOINT ["/usr/local/bin/escurel-server"]
