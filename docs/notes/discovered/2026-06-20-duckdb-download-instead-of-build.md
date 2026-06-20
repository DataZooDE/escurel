# DuckDB: download the precompiled libduckdb instead of compiling it

**Date:** 2026-06-20
**Scope:** whole workspace build time

## Symptom

Every cold build paid a ~2–3 min DuckDB compile: `libduckdb-sys`'s
`bundled` feature compiles DuckDB's entire C++ amalgamation from source
via the `cc` crate. On a clean target (new worktree, no cache, CI
without a warm `target/`) this dominated build time and was the reason
GitHub Actions CI is paused during bootstrap.

## Fix

`libduckdb-sys >= 1.10503` ships a built-in **download backend** in its
`build.rs` that we previously didn't use. Switching to it removes the
C++ compile entirely.

Two coordinated changes:

1. **Drop the `bundled` feature** from every `duckdb` dependency so the
   crate is in "linked" mode:

   ```toml
   duckdb = { version = "1", default-features = false }
   ```

   `default = []` for the `duckdb` crate, so `default-features = false`
   only drops `bundled` — nothing else is lost. Note feature
   unification: if **any** crate in the build graph still enables
   `bundled` (directly, or via `duckdb`'s `json`/`parquet` features,
   which imply `bundled`), the whole build reverts to the C++ compile.
   All eight declarations had to change together.

2. **Set `DUCKDB_DOWNLOAD_LIB=1`** in `.cargo/config.toml`:

   ```toml
   [env]
   DUCKDB_DOWNLOAD_LIB = "1"
   ```

   The download is gated **only** on this env var — there is no cargo
   feature for it. `.cargo/config.toml` makes it the default for every
   `cargo build/test/run` in the tree.

What the build script then does (`build.rs`, `not(feature="bundled")`
branch → `download_libduckdb`):

- derives the DuckDB version from the crate version
  (`1.10503.1` → DuckDB **1.5.3**; `1.10504.0` → **1.5.4**), so the
  downloaded lib always matches the generated bindings — no manual
  version pinning;
- fetches `github.com/duckdb/duckdb/releases/download/v<ver>/libduckdb-linux-amd64.zip`;
- caches it under `target/duckdb-download/<target>/<ver>/` (survives
  rebuilds, respects `CARGO_TARGET_DIR`);
- copies `libduckdb.so` into `target/<profile>/deps/` and emits
  `-Wl,-rpath,<dir>` so binaries/tests load it at runtime.

Measured: clean workspace build dropped from the multi-minute bundled
compile to **~25s**; `cargo test -p escurel-index` (real DuckDB, vss +
fts) green; `escurel-server` binary links and loads the lib via rpath.

## CI gotcha: rust-cache hides the downloaded lib (link fails)

**Symptom (escurel #189, the first warm-cache run after this merged):**
`cargo fmt`/`clippy` pass, then `cargo test` dies with
`/usr/bin/ld: cannot find -lduckdb` — the link search path
`target/duckdb-download/<triple>/<ver>` is on the command line but
`libduckdb.so` isn't in it. `clippy` passes because it never links final
executables; `test`/`build --release` do.

**Cause.** `Swatinem/rust-cache` caches `~/.cargo` + dependency artifacts
(incl. the libduckdb-sys **build-script fingerprint + output**) but NOT
the custom `target/duckdb-download/` dir. On a cache hit the build script
does **not** re-run (fingerprint restored), so it never re-downloads —
yet its cached output still emits `-L .../duckdb-download/...` and
`-lduckdb`. The `.so` was never restored → link fails. A cold run (e.g.
the merge commit's own run) passes because the script runs and downloads;
the breakage only shows on the **next** run that hits the cache.

**Fix (`.github/workflows/ci.yml`).** Three parts:
1. `cache-directories: target/duckdb-download` so the lib is cached
   alongside the fingerprint that references it.
2. Bump `shared-key` to discard the already-poisoned caches.
3. A safety step before the cargo steps: if
   `target/duckdb-download/*/*/libduckdb.so` is absent, `cargo clean -p
   libduckdb-sys` to force the script (hence the download) to re-run.

**How to recognise next time.** Any `-sys` crate whose build script
writes a prebuilt artifact into a *non-standard* `target/` subdir and
bakes that path into its link output will hit this: rust-cache restores
the fingerprint but not the artifact. Cache the artifact dir explicitly,
and don't trust a green cold/merge run — verify a second (warm-cache)
run. Consumers with their own CI (carl, agent-template) need the same
rust-cache treatment.

## Trade-offs (why this isn't free)

- **Network at first build.** A clean target for a new triple needs
  egress to github.com. Cached thereafter. Hermetic/offline builds must
  pre-seed `target/duckdb-download/` or vendor the lib.
- **No bundled fallback.** With `bundled` removed, a build with the env
  var unset falls through to pkg-config/system lookup and **fails** (no
  system libduckdb here). Every project that builds these crates MUST
  ship the `.cargo/config.toml` env. This is a breaking change for
  consumers (see below).
- **No checksum on the download.** `build.rs` fetches the release by URL
  with no hash verification (bundled compiles `Cargo.lock`-checksummed
  source). Supply-chain note for the milestone audit; `cargo deny` does
  not see the downloaded binary.
- **rpath points at an absolute build path.** Fine for dev/test. A
  shippable artifact must co-locate `libduckdb.so` and set `$ORIGIN`
  rpath, or static-link (`DUCKDB_STATIC=1` — the release zip also ships
  `libduckdb_static.a`).

## Consumers (submodule of DataZooDE/escurel)

`carl`, `herkules`, and `datazoo-agent-template` all pull escurel as a
git **submodule** at `vendor/escurel` and depend on `escurel-client` /
`escurel-test-support` by path. To get download mode each must:

1. bump the `vendor/escurel` submodule to the commit carrying this
   change;
2. add a root `.cargo/config.toml` with `DUCKDB_DOWNLOAD_LIB=1`
   (cargo does not read the submodule's `.cargo/config.toml`);
3. **agent-template only** also declares its own `duckdb` — drop
   `bundled` there too.

Both required releases exist (`v1.5.3`, `v1.5.4` → HTTP 200). Without
step 2 the consumer build breaks (no bundled fallback).

## How to recognise next time

If a `-sys` crate's bundled C++ compile dominates build time, check
whether the crate has a download/prebuilt backend (`DUCKDB_DOWNLOAD_LIB`
here) before reaching for sccache or a system-lib install. And remember
feature unification: one stray `bundled` (or a feature that implies it)
anywhere in the graph silently re-enables the slow path.
