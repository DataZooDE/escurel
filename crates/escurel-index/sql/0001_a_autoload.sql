-- Stage 1 of the v1 schema migration: enable auto-install for
-- known DuckDB extensions and force-load `vss` + `fts`.
--
-- Why explicit LOAD if autoload is on?
-- `autoload_known_extensions` triggers on function/type
-- references in queries, but `CREATE INDEX ... USING HNSW` is a
-- DDL token, not a function call — the loader is never invoked
-- and the binder reports "Unknown index type: HNSW".
--
-- With `autoinstall_known_extensions = true` set, the explicit
-- LOAD still downloads the binary on first call (no separate
-- INSTALL needed). Substrate deployments pre-bake the binaries
-- in the golden image; this LOAD finds them on disk and no
-- network call happens.
--
-- See docs/notes/discovered/2026-05-24-duckdb-vss-fts-autoload.md.
SET autoinstall_known_extensions = true;
SET autoload_known_extensions    = true;
LOAD vss;
LOAD fts;

-- HNSW persistence is gated behind an "experimental" flag in the
-- vss extension. The Escurel storage spec (storage.md §HNSW
-- persistence model) relies on the on-disk HNSW index being
-- loaded as-is on `DuckDB.Open()` and rolled back atomically on
-- mid-write SIGKILL, so persistent HNSW is mandatory.
-- See docs/notes/discovered/2026-05-24-vss-hnsw-experimental-persistence.md.
SET hnsw_enable_experimental_persistence = true;
