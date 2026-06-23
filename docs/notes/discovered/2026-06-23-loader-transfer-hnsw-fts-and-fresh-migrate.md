# Loader transfer: bulk-load order + `Migrator::up` is fresh-only

**Date:** 2026-06-23
**Area:** `escurel-loader` transfer, `escurel-index` (HNSW/FTS/migrations).

Three non-obvious things the DuckDB→DuckDB transfer (`Indexer::merge_from_attached`
+ `escurel_loader::transfer`) has to get right.

## 1. `Migrator::up` is NOT idempotent — gate it on a fresh DB

**Symptom.** A second transfer into the same live tenant failed with
`Catalog Error: Table with name "pages" already exists!`.

**Cause.** `Migrator::up` issues plain `CREATE TABLE` (the fresh-DB path). Running
it against an already-migrated DB throws.

**Fix.** Mirror the server boot recipe (`escurel-server/src/config.rs`): compute
`let fresh = !db_path.exists();` and run `up()` **only when fresh**.
`load_extensions` + `ensure_group_members` + `ensure_external_credentials` are
idempotent (`CREATE … IF NOT EXISTS` / per-connection session state) and run every
time. Recognise it: any code path that opens a tenant DB that *might already
exist* must not call `up()` unconditionally.

## 2. HNSW (vss): drop before bulk insert, recreate after

Per-row HNSW index maintenance over millions of inserted rows is the slow path.
The merge does `DROP INDEX hnsw_blocks_vec` → bulk `INSERT … BY NAME SELECT` →
recreate the index with the schema's DDL (`metric='cosine', ef_construction=128,
ef_search=64, M=16`). Correctness holds even with the index absent: `search.rs`
ranks vectors with a plain `array_cosine_distance` scan; HNSW only accelerates it.
The same drop/recreate is exposed as `Indexer::reindex_vectors()` and reused by
the loader build.

## 3. BM25 FTS is a one-shot snapshot — refresh once, after

The DuckDB FTS index is a point-in-time snapshot built by a PRAGMA; rows inserted
afterwards are invisible to FTS search until it is rebuilt. So the merge (and the
loader build) call `Indexer::refresh_fts()` exactly **once** after the bulk insert
— not per row. Forget it and vector search still works but lexical/BM25 hits for
the imported docs silently return nothing.

## Bonus: proving "no re-embed" in a test

To assert the transfer copies vectors verbatim rather than re-embedding, build the
source with a **non-zero** embedder (`HashEmbedder`) and open the transfer's live
Indexer with a **`ZeroEmbedder`** placeholder (the merge never embeds, so the
placeholder is harmless). If any re-embed crept in, the imported `dense_vec` would
read back as zeros — a loud, obvious failure. The capstone test asserts the live
`dense_vec` is byte-identical to the loader's.
