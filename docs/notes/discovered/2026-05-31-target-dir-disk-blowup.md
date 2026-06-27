# target/ ballooned to 600+ GB per worktree (DuckDB debug symbols)

**Symptom.** `target/` for a single escurel checkout grew to ~635 GB
(631 GB of it in `target/debug`), and with five git worktrees the
working tree was consuming ~975 GB — a disk hit 99% full. `du` showed
`target/debug/deps` at 523 GB, dominated by:

- ~919 integration-test binaries (`e2e-*`, `cli_e2e-*`, `ws_session-*`,
  `grpc_read_tools-*`), each **~930 MB**, totalling ~486 GB.
- 17 stale copies of `liblibduckdb_sys-*.rlib`, each **1.1 GB**.
- `target/debug/build` at 58 GB (57 GB of DuckDB C++ object output).

**Cause.** Two compounding effects:

1. **DuckDB's bundled C++ amalgamation is compiled with `-g`.**
   `libduckdb-sys`'s build script compiles the whole DuckDB amalgamation
   via the `cc` crate, which emits `-g` whenever cargo sets the build
   script's `DEBUG` env truthy — i.e. for any `profile.dev` build with
   debug info (the default). Those debug symbols are **statically linked
   into every test binary**, so each one carried a full copy of DuckDB's
   debuginfo → ~930 MB apiece.

2. **Cargo never garbage-collects stale artifacts.** Every rebuild with
   a changed fingerprint leaves the previous ~930 MB binary (and 1.1 GB
   rlib) behind. Across months of builds × 5 worktrees, it ran away.

**Fix.** Two parts:

1. Drop debug info for the DuckDB C++ only, in the workspace root
   `Cargo.toml` — keeps full debuginfo for our own Rust crates:

   ```toml
   [profile.dev.package."libduckdb-sys"]
   debug = false
   ```

   This sets the build script's `DEBUG` env to `false`, so `cc` omits
   `-g` and the build also defines `NDEBUG`. Measured effect: the
   `liblibduckdb_sys` rlib dropped **1.1 GB → 433 MB**; test binaries
   shrink correspondingly (~930 MB → ~300–400 MB). Rebuild of
   `libduckdb-sys` was ~1m37s.

2. Reap stale artifacts periodically with
   [`cargo-sweep`](https://github.com/holmgr/cargo-sweep). A weekly
   **user** systemd timer runs across the whole projects tree:

   ```
   cargo-sweep sweep --recursive --time 7 ~/Projects/datazoo
   ```

   (`~/.config/systemd/user/cargo-sweep.{service,timer}` — not in this
   repo; it's machine-local dev hygiene.) `--time 7` keeps anything
   touched in the last week, so active builds are never disturbed.

**How to recognise it next time.** If `target/debug/deps` is dominated
by many same-named `*-<hash>` test binaries in the hundreds of MB each,
it's stale-artifact accumulation — run `cargo-sweep --time N` to reap.
If individual binaries are huge, suspect a native dep statically linking
debug symbols and scope `debug = false` to that `-sys` package.

**Note on worktrees.** A shared `CARGO_TARGET_DIR` was considered to
de-duplicate across the five worktrees but rejected: they sit on
different branches, so a shared target thrashes (recompiles on switch)
and serialises concurrent builds (cargo locks the target dir). With the
debuginfo fix the per-worktree footprint is small enough that the
duplication no longer matters.
