# libduckdb download panics in a network-restricted session

**Symptom.** A cold `cargo build` / `cargo test` (any crate that links
`libduckdb-sys`, i.e. `escurel-index` and everything downstream) panics in the
build script:

```
error: failed to run custom build command for `libduckdb-sys v1.10503.1`
thread 'main' panicked at libduckdb-sys/build.rs:
Failed to download libduckdb: error sending request for url
(https://github.com/duckdb/duckdb/releases/download/v1.5.3/libduckdb-linux-amd64.zip)
```

**Cause.** Per `.cargo/config.toml` (`DUCKDB_DOWNLOAD_LIB=1`, see
[`2026-06-20-duckdb-download-instead-of-build.md`](2026-06-20-duckdb-download-instead-of-build.md))
the build fetches the **prebuilt** libduckdb from GitHub *releases* instead of
compiling the amalgamation. In a sandbox whose egress policy scopes GitHub to
the session's own repos (e.g. Claude Code on the web), that release URL returns
**403** ("GitHub access to this repository is not enabled for this session"),
and the build script aborts. The `bundled` fallback does **not** rescue it —
the vendored `libduckdb-sys` crate (~7 MB) does not ship the amalgamation, so
it would also try to fetch.

**Recognise it by:** the panic is in `libduckdb-sys/build.rs` at
`find_duckdb`, and `curl -sSL <that release url>` returns HTTP 403 with a
GitHub "access … not enabled for this session" body — a *policy* block, not a
transient network error, so retrying and exponential backoff do nothing.

**Fix / workarounds (in order of preference).**

1. Run in an environment whose egress allows `github.com/duckdb/duckdb/releases`
   (the normal dev box / CI). This is the intended path.
2. Pre-seed the library and point the build at it, bypassing the download:
   set `DUCKDB_LIB_DIR` (and `DUCKDB_INCLUDE_DIR`) to a directory holding a
   matching `libduckdb.so` + headers for the pinned DuckDB version (v1.5.3 for
   `libduckdb-sys 1.10503.1`). `libduckdb-sys` then links that copy and skips
   the fetch entirely.
3. Do **not** try to route around the egress policy (the agent-proxy README is
   explicit: report a 403'd host, don't tunnel around it).

**Consequence for reviews.** In such a session the local merge gate
(`cargo fmt/clippy/test/build`, CLAUDE.md principle 2) **cannot run** — code
written there must be built + tested once in an unblocked environment before it
is trusted green.
