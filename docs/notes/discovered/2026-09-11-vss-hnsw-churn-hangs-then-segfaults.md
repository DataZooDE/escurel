# A vss HNSW index created on an empty table breaks after ~192 delete+insert cycles

**Date:** 2026-09-11
**Scope:** `escurel-index` (`blocks.hnsw_blocks_vec`), DuckDB 1.5.5 + `vss` b833341
**Issue:** [#431](https://github.com/DataZooDE/escurel/issues/431)

## Symptom

Two, from the same defect:

- **Hang.** `tx.commit()` in `Indexer::update_page_as` never returns. Zero CPU.
  The stack ends in the extension:

  ```
  unum::usearch::index_dense_gt<long, unsigned int>::remove(long)   ← vss
  duckdb::HNSWIndex::Delete
  duckdb::BoundIndex::Delete
  duckdb::RowGroupCollection::RemoveFromIndexes
  duckdb::IndexDataRemover::Flush / PushDelete
  duckdb::UndoBuffer::Commit
  duckdb::DuckTransaction::Commit
  ```

- **SIGSEGV (container exit 139)** — what #431 reported from lab on 2026-08-30.
  Same table, the insert side rather than the delete side:

  ```
  unum::usearch::metric_punned_t::equidimensional_<metric_cos_gt<float,float>>
  unum::usearch::index_dense_gt<long, unsigned int>::add_<float>
  duckdb::HNSWIndex::Construct
  duckdb::HNSWIndex::Append
  ```

## Cause

Not escurel's. Reproducible in the `duckdb` CLI with nothing but `vss`:

```sql
INSTALL vss; LOAD vss;
SET hnsw_enable_experimental_persistence = true;
CREATE TABLE blocks(page_id VARCHAR, dense_vec FLOAT[768]);
CREATE INDEX hnsw_blocks_vec ON blocks USING HNSW (dense_vec)
  WITH (metric = 'cosine', ef_construction = 128, ef_search = 64, M = 16);
-- then, 192 times:
BEGIN;
  DELETE FROM blocks WHERE page_id = 'p1';
  INSERT INTO blocks SELECT 'p1', (SELECT list(0.0::FLOAT) FROM range(768))::FLOAT[768];
COMMIT;
```

The 192nd cycle blocks for ever. What the experiments pin down:

| variant | result |
|---|---|
| 8 pages churned, or 24 | hangs at op **192** either way |
| all-zero vectors, or random | hangs at op **192** either way |
| in-memory database (no persistence flag) | hangs at op **192** |
| **no HNSW index** (control) | 384 ops, clean |
| inserts only, no deletes | 400 ops, clean |
| `PRAGMA hnsw_compact_index` every 100 ops | still hangs at op 192 |
| `DROP INDEX` + `CREATE INDEX` at op 150, then continue | **SIGSEGV at op 197** |
| 50 rows inserted **before** `CREATE INDEX`, then churn | hangs at op **231** |
| 500 rows inserted **before** `CREATE INDEX`, then churn | hangs at op **1573** |

So it is neither persistence (in-memory fails too), nor the vectors, nor the
delete backlog. What matters is **how populated the table was when the index
was created**: 0 rows buys 192 churn cycles, 50 rows buys 231, 500 rows buys
1573. The budget grows with the starting size but is finite in every case, so
pre-seeding postpones the failure rather than removing it. Rebuilding the index
mid-stream does not reset it either — it converts the hang into a segfault,
because the rebuilt index is again a nearly-empty one.

## Why escurel walks straight into it

`Migrator::up` creates `hnsw_blocks_vec` on a **fresh, empty** DuckDB, and the
container default `ESCUREL_REBUILD_INDEX_ON_BOOT=always` makes every boot a
fresh one. The corpus then arrives incrementally through `rebuild()`, and every
`update_page` after that does exactly one delete+insert on `blocks`
(`materialise::replace_blocks`). So a writer has a budget of roughly 192 page
writes per boot before its next commit hangs or crashes — which is why the lab
process survived seven days of light use and died inside a seeding burst.

## How to recognise next time

A write path that ends in `tx.commit()` with **zero CPU** and no Rust frames
below the `duckdb` crate is not a Rust deadlock; attach to the process (or run
it under gdb from the start — `ptrace_scope=1` refuses a late attach) and read
the native stack. `usearch` frames mean the vector index, not the storage
engine.

## Watch for

- vss is labelled experimental for persistence only, but this reproduces
  in-memory too: treat incremental churn against an HNSW index as unsupported
  at any size, not just on disk.
- Any code that does `DROP INDEX` + `CREATE INDEX` on `blocks` as a repair
  makes the next failure a segfault rather than a hang.
