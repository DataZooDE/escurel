# DuckLake silently inlines small writes into the Postgres catalog — no Parquet on the DATA_PATH

## Symptom

A DuckLake publish against a Postgres catalog "fully works" — the
transaction commits, `ducklake_snapshots()` advances, a fresh `READ_ONLY`
reader sees every row — **but the object store (MinIO/GCS `DATA_PATH`) is
empty**. `glob('s3://…/**/*.parquet')` returns 0, `mc ls -r` shows
nothing, and the catalog's `ducklake_data_file` table has 0 rows. Caught
by `ducklake_publish_live.rs`'s "MinIO now contains Parquet objects"
assertion (DuckLake PR 3).

## Cause

DuckLake *data inlining*: small inserts are stored as rows in catalog-side
`ducklake_inlined_data_*` tables instead of being written as Parquet
files. With the Postgres catalog (ducklake `d318a545`, DuckDB 1.5.x) this
happened by default for small `CREATE OR REPLACE TABLE … AS SELECT`
publishes. The data is readable (readers transparently union the inlined
rows), which is exactly why the failure is silent — everything except the
object-store layout behaves.

The spike (2026-07-17-ducklake-spike-results.md) never hit this because it
inserted 1 000 rows per batch — above the inlining threshold.

## Fix

The writer attach disables inlining, so a publish always lands as Parquet
on the DATA_PATH (catalog = coordination, object store = data — a lake
whose blocks live inside Cloud SQL rows defeats the design and bloats the
catalog):

```sql
ATTACH 'ducklake:postgres:<dsn>' AS lake
  (DATA_PATH 's3://…/', DATA_INLINING_ROW_LIMIT 0);
```

`crates/escurel-index/src/snapshot/lake.rs::attach_sql` emits this for
every writer attach (readers attach `READ_ONLY` and don't need it).
Verified: with the option, a two-table transaction writes one Parquet
file per table and still commits as exactly ONE snapshot.

## How to recognise it next time

Lake reads succeed but the bucket/prefix has no objects; catalog DB size
grows with data volume; `SELECT count(*) FROM
postgres_query('__ducklake_metadata_<alias>', 'SELECT * FROM
ducklake_data_file')` is 0 while `ducklake_inlined_data_*` tables exist.
An existing lake with inlined rows can be drained with
`CALL ducklake_flush_inlined_data('<alias>')` (note: the flush is its own
snapshot).
