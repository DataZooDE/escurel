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

# The DuckDB release the image bakes extensions for. It must be the one libduckdb-sys links (Cargo.lock):
# extensions resolve under <version>/<platform>, so a mismatch is a silent 137 MB download at boot, or an
# extension that refuses to load. The builder stage asserts the two agree.
ARG DUCKDB_VERSION=v1.5.6

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
# Fail the build, not the pod, when the pinned DuckDB release drifts from the linked one.
# libduckdb-sys 1.<MMPP>.x is DuckDB 1.<MM>.<PP> (1.10506.0 -> v1.5.6).
ARG DUCKDB_VERSION
RUN set -eu; \
    n="$(grep -A1 '^name = "libduckdb-sys"$' Cargo.lock | sed -n 's/^version = "1\.\([0-9]*\)\..*/\1/p' | head -n1)"; \
    [ -n "$n" ] || { echo "libduckdb-sys not found in Cargo.lock"; exit 1; }; \
    want="v1.$((n / 100 % 100)).$((n % 100))"; \
    [ "$want" = "${DUCKDB_VERSION}" ] || { echo "DUCKDB_VERSION=${DUCKDB_VERSION} but Cargo.lock links libduckdb-sys for DuckDB ${want}: bump the ARG (the baked extensions would not match)"; exit 1; }
# Serialise codegen/link: linking the release binary against libduckdb is
# memory-hungry and OOMs a default-parallelism release+LTO build on a 7 GB CI
# runner (the CI workflow caps this the same way). Release profile already
# strips symbols.
# `duckvfs` is in the feature list because the backend is otherwise absent
# from the binary entirely: ESCUREL_STORAGE_BACKEND=duckvfs then refuses at
# boot with "requires the 'duckvfs' cargo feature; this binary was built
# without it". It costs nothing to carry -- the feature is `dep:duckdb`, and
# libduckdb is already linked for escurel-index -- so leaving it out only
# meant the image could not serve a Google Drive lane store.
#
# Two runtime requirements come with it, both satisfied by the deployment
# rather than the image: the duckdb-gdrive extension is fetched from the
# DuckDB community repository on first use (so the process needs egress and a
# writable $HOME for ~/.duckdb), and ESCUREL_STORAGE_DUCKVFS_EXTENSION_REPO
# must name that repository.
ENV CARGO_BUILD_JOBS=1
# Download mode (.cargo/config.toml: DUCKDB_DOWNLOAD_LIB=1) links libduckdb
# DYNAMICALLY: the binary carries `NEEDED libduckdb.so` with no rpath, so the
# runtime stage must ship the shared object too. Both artifacts are copied out
# of the cache-mounted `target/` in THIS RUN (the mount is not visible to later
# COPY stages), into non-mounted dirs that persist in the builder layer.
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p escurel-server --features gemini,s3,gcs,duckvfs \
    && cargo build --release -p escurel-cli \
    && cp target/release/escurel-server /usr/local/bin/escurel-server \
    && cp target/release/escurel /usr/local/bin/escurel \
    && cp "$(find target -name libduckdb.so -print -quit)" /usr/local/lib/libduckdb.so

# ---- extension cache ------------------------------------------------------
# Pre-install every DuckDB extension escurel loads, so a container downloads
# NOTHING at boot.
#
# Measured in the cluster before this existed: a cold pod fetched ~137MB
# (postgres_scanner 41MB, ducklake 36MB, vss 32MB, gdrive 20MB, fts 11MB) and
# had not finished after 7m56s, while the next start in the SAME pod reached
# the catalog in 7 SECONDS off a warm cache. A PersistentVolume does not fix
# it: `helm upgrade --atomic` deletes the PVC when the release is rolled back,
# so the cache never survives the very failure it would prevent.
#
# The version and platform in the path are not cosmetic -- DuckDB resolves
# extensions under <version>/<platform>, so a mismatch silently downloads
# again at runtime. Both are asserted below rather than assumed.
FROM debian:bookworm-slim AS extensions
ARG DUCKDB_VERSION
# gdrive from the erpl.io mirror, NOT `community`: workload identity federation
# only exists in v2026.09.01 and the community repository still serves
# v2026.08.07, whose credential_chain refuses external_account outright. Swap
# once duckdb/community-extensions#2588 is merged and built.
ARG GDRIVE_REPO=https://get.erpl.io
# Optional integrity pin for the (unsigned, moving) mirror artifact: when set, the build fails unless the
# gz served at <repo>/<duckdb version>/linux_amd64/gdrive.duckdb_extension.gz has this sha256.
ARG GDRIVE_SHA256=
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl unzip \
    && rm -rf /var/lib/apt/lists/*
RUN curl -sSfL "https://github.com/duckdb/duckdb/releases/download/${DUCKDB_VERSION}/duckdb_cli-linux-amd64.zip" \
      -o /tmp/duckdb.zip \
 && unzip -q /tmp/duckdb.zip -d /usr/local/bin \
 && chmod +x /usr/local/bin/duckdb \
 && rm /tmp/duckdb.zip
ENV HOME=/opt/escurel
RUN if [ -n "${GDRIVE_SHA256}" ]; then \
      echo "${GDRIVE_SHA256}  -" > /tmp/gdrive.sha256 \
      && curl -sSfL "${GDRIVE_REPO}/${DUCKDB_VERSION}/linux_amd64/gdrive.duckdb_extension.gz" | sha256sum -c /tmp/gdrive.sha256; \
    fi
# gdrive comes from the erpl.io mirror (WIF-capable, v2026.09.01+) and the mirror lags each DuckDB release:
# when it has no artifact for this DuckDB version the image is built WITHOUT gdrive, LOUDLY, rather than
# substituting the older community build (its credential_chain refuses external_account, so WIF would
# break silently at runtime). Consequence: ESCUREL_STORAGE_BACKEND=duckvfs (Google Drive lane store) is
# unavailable in such an image. Set REQUIRE_GDRIVE=1 to make a missing artifact fail the build instead.
ARG REQUIRE_GDRIVE=0
RUN mkdir -p /opt/escurel \
 && duckdb -unsigned -c "INSTALL ducklake; INSTALL postgres; INSTALL sqlite; INSTALL mysql; INSTALL httpfs; INSTALL fts; INSTALL vss;" \
 && if curl -sfI "${GDRIVE_REPO}/${DUCKDB_VERSION}/linux_amd64/gdrive.duckdb_extension.gz" >/dev/null; then \
      duckdb -unsigned -c "INSTALL gdrive FROM '${GDRIVE_REPO}';"; \
    else \
      echo "################################################################################"; \
      echo "WARNING: ${GDRIVE_REPO} has no gdrive artifact for DuckDB ${DUCKDB_VERSION}: building WITHOUT gdrive."; \
      echo "         The duckvfs (Google Drive) lane store is unavailable in this image."; \
      echo "################################################################################"; \
      [ "${REQUIRE_GDRIVE}" = "0" ] || { echo "REQUIRE_GDRIVE=${REQUIRE_GDRIVE}: failing the build"; exit 1; }; \
    fi
# Fail the BUILD, not the pod, if anything did not land where DuckDB looks for
# it. A missing extension here is a silent 137MB download at boot.
RUN set -eu; \
    d="/opt/escurel/.duckdb/extensions/${DUCKDB_VERSION}/linux_amd64"; \
    # sqlite_scanner / mysql_scanner (SQL rows connectors, `docs/spec` backends) are baked for the same reason.
    # postgres_scanner, not postgres: `INSTALL postgres` is an ALIAS and the
    # artifact it lands is postgres_scanner.duckdb_extension. Checking the
    # alias name failed the build while all six were present (now eight).
    want="ducklake postgres_scanner sqlite_scanner mysql_scanner httpfs fts vss"; \
    # gdrive is expected only when the mirror had it (see the install step above); a REQUIRE_GDRIVE build never gets here without it.
    if curl -sfI "${GDRIVE_REPO}/${DUCKDB_VERSION}/linux_amd64/gdrive.duckdb_extension.gz" >/dev/null; then want="$want gdrive"; fi; \
    for e in $want; do \
      test -s "$d/$e.duckdb_extension" || { echo "MISSING: $e in $d"; ls -la "$d" || true; exit 1; }; \
    done; \
    echo "baked $(ls "$d"/*.duckdb_extension | wc -l) extensions, $(du -sh /opt/escurel/.duckdb | cut -f1)"

# ---- demo gateway (web workbench demo only; NOT the default target) -------
# The demo stack needs a gateway that VERIFIES tokens and can tell which run wrote what (the thread, the
# trace, the held proposals): that is `escurel-test-gateway` (escurel-server + a built-in token issuer).
# It is built into its own stage, placed BEFORE `runtime` so `runtime` stays the Dockerfile's last stage
# (the default target the published image uses). Select it with `target: demo-gateway`.
FROM builder AS demo-builder
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p escurel-test-support --bin escurel-test-gateway \
    && cp target/release/escurel-test-gateway /usr/local/bin/escurel-test-gateway

# trixie, not bookworm: the optimizer extension is built against a newer glibc (GLIBC_2.38) than bookworm ships (2.36).
FROM debian:trixie-slim AS demo-gateway
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=demo-builder /usr/local/bin/escurel-test-gateway /usr/local/bin/escurel-test-gateway
COPY --from=builder /usr/local/bin/escurel /usr/local/bin/escurel
COPY --from=builder /usr/local/lib/libduckdb.so /usr/lib/libduckdb.so
RUN ldconfig
COPY --from=extensions --chown=65532:65532 /opt/escurel/.duckdb /opt/escurel/.duckdb
ENV HOME=/opt/escurel
RUN mkdir -p /data && chown 65532:65532 /data
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/escurel-test-gateway"]

# ---- runtime -------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# libstdc++6: the downloaded libduckdb links it dynamically and debian-slim
# does not ship it by default. curl: HEALTHCHECK probe.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/escurel-server /usr/local/bin/escurel-server
# The operator CLI (6.7 MiB), so a breaking-release migration runs where the data is, with no admin API
# exposed: `docker exec <c> escurel --server http://127.0.0.1:8080 admin migrate-kind --tenant <t>`
# (docs/deploy/kind-migration.md). It is a pure HTTP client of the server above.
COPY --from=builder /usr/local/bin/escurel /usr/local/bin/escurel
# The one-shot migration job (exits non-zero unless the tenant ends migrated and un-quarantined).
COPY --from=builder /build/scripts/migrate-kind-job.sh /usr/local/bin/migrate-kind-job
# The dynamically-linked libduckdb.so (see the builder note). Land it in a
# standard search dir and refresh the loader cache so the binary — which has
# no rpath — finds it at startup.
COPY --from=builder /usr/local/lib/libduckdb.so /usr/lib/libduckdb.so
RUN ldconfig
# The pre-installed extensions, owned by the uid the substrate chart runs as
# (securityContext.runAsUser 65532) so DuckDB can read them under a read-only
# root filesystem. HOME is set to match, because HOME is what decides where
# DuckDB looks: pointing it anywhere else silently reverts to downloading.
COPY --from=extensions --chown=65532:65532 /opt/escurel/.duckdb /opt/escurel/.duckdb
ENV HOME=/opt/escurel

# Run as the uid the substrate chart already uses (securityContext.runAsUser 65532), NOT root, so a
# plain `docker run` is as unprivileged as the cluster. /data is created and owned by that uid BEFORE
# the VOLUME line, so a fresh (anonymous or empty named) volume mounted there is writable; a bind
# mount or PersistentVolume must be owned by 65532 (the chart's fsGroup does this).
RUN mkdir -p /data && chown 65532:65532 /data
USER 65532:65532

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
# ESCUREL_REBUILD_INDEX_ON_BOOT is deliberately NOT set here any more, so the
# binary's own default (`if-missing`) applies: reuse an existing DuckDB and
# rebuild only when it is absent.
#
# It used to be `always`, and the reason given was that "vss's experimental
# HNSW persistence segfaults when a restart reloads the on-disk index". That
# index no longer exists — it was removed in `sql/0001_b_tables.sql` because
# vss corrupts itself under the delete+insert cycle every page write performs,
# and because it bought nothing measurable (29.6ms vs 30.6ms at 10k blocks).
# The workaround outlived the bug it worked around, and its cost is large: a
# full re-embed of the corpus on every single boot, measured at 16 minutes and
# budgeted at 29.
#
# A host that legitimately starts with no index still rebuilds once, on its
# own: `SnapshotStore` rebuilds when the DuckDB file was freshly created (the
# wiped-volume case). That path is now guarded — a rebuild refuses to empty a
# populated index, and `LaneStore::list` errors rather than reporting an
# unreachable store as empty.
ENV ESCUREL_SERVER_LISTEN_HTTP=0.0.0.0:8080 \
    ESCUREL_SERVER_DATA_DIR=/data
EXPOSE 8080 9090
VOLUME ["/data"]

# Liveness mirrors what kamal-proxy probes (dependency-free /healthz).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s \
  CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

# The derived-index boot policy is handled inside the binary, gated by
# ESCUREL_REBUILD_INDEX_ON_BOOT (left unset here — see above). No shell wrapper
# — exec the server directly so it is PID 1 and receives SIGTERM for graceful
# shutdown.
ENTRYPOINT ["/usr/local/bin/escurel-server"]
