# `duckdb-rs` bundled-cmake / Ninja: not usable from crates.io

**Date:** 2026-05-24
**Scope:** `escurel-index` build pipeline

## Symptom

Goal: make the bundled DuckDB compile use Ninja for parallel
speedup (user asked: "use `GEN=ninja` to build duckdb if it is
available").

Attempt: enable the `bundled-cmake` feature on duckdb-rs.

```toml
duckdb = { version = "1", features = ["bundled", "bundled-cmake"] }
```

Build fails immediately:

```
thread 'main' panicked at libduckdb-sys-1.10503.1/build_bundled_cmake.rs:19:9:
`bundled-cmake` requires a duckdb-rs checkout with DuckDB sources
at duckdb-sources/CMakeLists.txt
```

## Cause

`libduckdb-sys` has two bundled build paths:

- **`bundled`** (default) — uses the `cc` crate. The crates.io
  release ships pre-processed DuckDB **amalgamation files** (one
  huge `.cpp`); the `cc` backend compiles them directly with
  cargo's parallel job scheduler. No CMake involved.
- **`bundled-cmake`** — uses the `cmake` crate. Auto-detects
  `ninja` on PATH (from inspecting
  `libduckdb-sys-1.10503.1/build_bundled_cmake.rs`:
  *"bundled-cmake generator: Ninja (autodetected via {ninja})"*).
  **But this path requires a git-checkout layout with DuckDB
  sources vendored at `duckdb-sources/CMakeLists.txt`** —
  something the crates.io publish strips.

So `bundled-cmake` is only usable when you depend on duckdb-rs
via a git `[patch]` with submodules / vendored sources. Heavy
infra commitment.

## Decision

Stick with the `cc` backend (the default `bundled` feature). It
parallelises across cargo's job pool, so a modern multi-core host
finishes the cold compile in ~2–3 min. CI runs `Swatinem/rust-cache`
which caches the build output between runs, so post-cache CI
compile is ~30s.

The user's "use Ninja if available" request can't be satisfied
through duckdb-rs configuration alone without taking on a
git-dependency-with-submodules build.

## How to recognise next time

If a Rust `*-sys` crate has both `bundled` and `bundled-cmake`
features and the cmake path errors with "requires a checkout":
the crates.io release isn't shipping the CMake-buildable source
layout. Either accept the cc backend, or take the cost of a
`[patch.crates-io]` git dep with submodules.

## Future possibilities

- If CI feels slow even with caching, evaluate switching to a
  pre-built `duckdb` shared library installed via apt
  (Ubuntu doesn't currently ship one — would need a custom PPA)
  or via the [official DuckDB binaries](https://duckdb.org/docs/installation/).
  This would also help local devs who don't want the 2–3 min
  first compile.
- A future M5 substrate golden image bakes the libduckdb binary;
  production builds link against it. CI could mirror that pattern
  if we publish a small CI image with libduckdb pre-installed.
