# The demo gateway ran DuckDB 1.5.5 after the build was pinned to 1.5.6

**Symptom.** After the `libduckdb-sys` bump, `editors/vscode/demo/run.sh start` still logged a 1.5.5
gateway, and the freshly built `anofox_optimize` extension (version-locked to 1.5.6) refused to load.
Nothing in the build was wrong: `cargo build` links `libduckdb.so` dynamically (download mode,
`.cargo/config.toml`), and `run.sh` put the FIRST `libduckdb.so` it found under `target/` on
`LD_LIBRARY_PATH`: a stale 1.5.5 copy from an older build directory of the same target tree.

**Fix.** `run.sh` now derives the wanted DuckDB version from `Cargo.lock`
(`editors/vscode/demo/s2d/pinned_duckdb.py`: `libduckdb-sys 1.10506.0` → `1.5.6`) and picks the
`libduckdb.so` whose directory matches it (`libduckdb_dir()`; override with
`ESCUREL_DEMO_LIBDUCKDB_DIR`). `gateway_duckdb_version()` reports what the gateway will actually load, and
the S2D sync loads an optional extension only when its footer matches that version.

**How to recognise it.** The gateway's startup log or `SELECT version()` through a `sql_view` says a
version other than the one `Cargo.lock` pins; an extension `LOAD` fails with a version-mismatch message
while the file's footer looks right. `ldd target/release/escurel-server | grep libduckdb` shows which
copy is resolved; `find target -name libduckdb.so` shows how many there are.

**General rule.** A DuckDB extension is built for ONE DuckDB version. Whenever the pin moves, every
locally built extension (`anofox_*`, `gdrive`) must be rebuilt, and anything that hand-picks a
`libduckdb.so` must pick by version, not by directory order.
