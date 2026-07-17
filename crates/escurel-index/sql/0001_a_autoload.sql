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

-- NOTE: the experimental-HNSW-persistence flag
-- (`SET hnsw_enable_experimental_persistence = true;`) deliberately
-- does NOT live here any more. It is per-connection session state
-- that only PERSISTENT (single-file) databases need; snapshot-style
-- backends must be able to load vss/fts without also opting into
-- experimental HNSW persistence. It is applied explicitly by
-- `Migrator::enable_hnsw_persistence` (and by `Migrator::up`, whose
-- HNSW `CREATE INDEX` requires it on a file-backed DB).
