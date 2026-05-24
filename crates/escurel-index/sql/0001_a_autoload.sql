-- Stage 1 of the v1 schema migration: enable auto-install for
-- known DuckDB extensions and force-install + force-load
-- `vss` + `fts`.
--
-- Why explicit INSTALL + LOAD if autoload + autoinstall are on?
--
-- - `autoload_known_extensions` triggers on function/type
--   references in queries, but `CREATE INDEX ... USING HNSW` is a
--   DDL token, not a function call — the loader is never invoked
--   and the binder reports "Unknown index type: HNSW".
-- - `autoinstall_known_extensions` does not fire on `LOAD`
--   alone — `LOAD vss` against a host that has never installed
--   vss errors with "Extension not found. Install it first using
--   INSTALL vss." (caught on the first CI run; see
--   discovered/2026-05-24-duckdb-load-needs-install.md).
--
-- INSTALL is idempotent (no-op if already present), so this works
-- on both fresh CI runners and laptops with a populated extension
-- cache. Substrate deployments bake the binaries into the golden
-- image — both INSTALL and LOAD find them on disk and never
-- touch the network.
SET autoinstall_known_extensions = true;
SET autoload_known_extensions    = true;

INSTALL vss;
LOAD vss;

INSTALL fts;
LOAD fts;

-- HNSW persistence is gated behind an "experimental" flag in the
-- vss extension. The Escurel storage spec (storage.md §HNSW
-- persistence model) relies on the on-disk HNSW index being
-- loaded as-is on `DuckDB.Open()` and rolled back atomically on
-- mid-write SIGKILL, so persistent HNSW is mandatory.
-- See docs/notes/discovered/2026-05-24-vss-hnsw-experimental-persistence.md.
SET hnsw_enable_experimental_persistence = true;
