# DuckPGQ is not installable on the pinned libduckdb (v1.5.3)

**Date:** 2026-07-31
**Area:** provenance graph (ADR-0010), DuckDB community extensions

## Symptom

The ADR-0010 go/no-go spike
(`crates/escurel-index/tests/duckpgq_spike.rs`) tried the standard
community-extension load against the workspace's pinned libduckdb
(duckdb-rs `1.10503.1` → DuckDB **v1.5.3**):

```sql
SET allow_unsigned_extensions = true;   -- (via open-time Config, see below)
INSTALL duckpgq FROM community;
LOAD duckpgq;
```

It fails:

```
HTTP Error: Failed to download extension "duckpgq" at URL
"http://community-extensions.duckdb.org/v1.5.3/linux_amd64/duckpgq.duckdb_extension.gz" (HTTP 404)
Candidate extensions: "ducklake", "quack", "odbc", "uc_catalog", "autocomplete"
```

There is **no prebuilt `duckpgq` binary for DuckDB v1.5.3**. DuckPGQ
tracks specific DuckDB versions and lags the latest releases (it was
present around 1.1.x–1.4.x; absent in 1.5.x). This is not a config or
network problem — the artefact simply does not exist in the registry
for our version/platform.

## Verdict: NO-GO (for now)

The provenance-graph feature (ADR-0010) therefore ships **entirely on
the recursive-CTE backend** — which was always the default and delivers
the full query surface (`provenance_ancestry`, `provenance_path`,
`expectation_drift`, `abandoned_paths`) with zero new dependency. The
`GraphBackend::DuckPgq` seam in `crates/escurel-index/src/graph.rs`
stays reserved but unwired; no DuckPGQ `MATCH` backend is added.

## How to recognise / re-check

- The spike is `#[ignore]`d and **passes green while printing NO-GO** —
  it is a probe, not a gate. Re-run it after any DuckDB bump:

  ```sh
  cargo test -p escurel-index --test duckpgq_spike -- --ignored --nocapture
  ```

  The day a `duckpgq` binary exists for the pinned DuckDB, the spike
  stops early-returning and instead exercises `CREATE PROPERTY GRAPH`
  over `resolved_links` + a `MATCH` — turning green *by verifying the GO
  path*. That is the signal to build the DuckPGQ backend arm.

## Gotcha found along the way

`allow_unsigned_extensions` is a **startup** setting: `SET
allow_unsigned_extensions = true` after the database is already running
(extensions loaded) errors with *"Cannot change allow_unsigned_extensions
setting while database is running."* Set it on the open-time
`duckdb::Config` (`Config::default().allow_unsigned_extensions()` →
`Connection::open_with_flags`), before any `INSTALL`/`LOAD`.
