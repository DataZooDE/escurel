# Drafts on a lake failed on the first proposal; the publish tick rewrote idle tables

**Date:** 2026-10-17. **Where:** `create_draft`, `create_drafts_lake_table_sql`, `compact_append_tables`.

## Symptoms

- With the drafts table on the lake (it follows `ESCUREL_EVENTS_BACKEND=ducklake`) the first `create_draft` failed with
  `Constraint Error: NOT NULL constraint failed: escurel_drafts.decided_by`. The lake DDL declares `reason` and
  `decided_by` `NOT NULL` with no default (DuckLake has no server-side defaults), and the insert omitted them; the
  Postgres and local tables default them to `''`. No test covered drafts on a lake, so nothing noticed.
- The publish task rewrote the events and chat tables on **every** tick even when nothing had been appended
  (`CREATE OR REPLACE TABLE t AS SELECT * FROM t`: one upload and one snapshot per table, ~576 uploads a day on Drive),
  and never touched the drafts table, whose files only ever grew.

## Fix

`create_draft` supplies `reason` and `decided_by` as `''`. Compaction rewrites a table only when it has at least
`ESCUREL_APPEND_COMPACT_MIN_FILES` data files (counted with `ducklake_list_files`), and the drafts table is in the list.

## How to recognise it next time

- A proposal on a lake backend fails with a NOT NULL constraint naming a drafts column.
- Many lake snapshots with no new rows (`ducklake_snapshots`) on an idle system.
- **Testing trap:** a second DuckDB *instance* (`Connection::open_in_memory()` + `ATTACH` of the same DuckLake *file*
  catalog) reads the catalog as of its last checkpoint, not the newest appends of the indexer under test. Read the
  state through `Connection::try_clone()` of the indexer's own connection (same instance) when asserting on file counts
  or snapshots; this cost an hour of "compaction did nothing" before the reads were corrected.
