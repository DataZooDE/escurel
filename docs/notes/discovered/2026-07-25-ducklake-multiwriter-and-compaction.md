# DuckLake is multi-writer, but cannot compact an append-shaped table

Two Phase-0 spikes for the question "can `chat_messages` / `events` live in
DuckLake tables instead of the attached Cloud SQL Postgres?". Reproducers:
`crates/escurel-index/tests/ducklake_spikes_live.rs`
(`cargo test -p escurel-index --features live-ducklake --test ducklake_spikes_live -- --nocapture`).

Environment: DuckDB 1.10503.1 / ducklake as vendored, real Postgres 16
catalog + real MinIO `s3://` DATA_PATH, both testcontainers.

## S1 — concurrent read-write ATTACH: **works**

The original spike
(`2026-07-17-ducklake-spike-results.md`) only ever tested ONE read-write
attacher alongside `READ_ONLY` readers, and ADR-0009 states multi-writer
conflict was *"avoided entirely"* — so this was uncharacterised.

Two independent connections both `ATTACH ... ` read-write against the same
Postgres-catalog lake and interleaved 50 single-row inserts each:

```
attached=2 rw, inserts=50+50, errors a=0 b=0, rows total=100 a=50 b=50
```

No errors, no lost writes, correct attribution. A `READ_ONLY` attach opened
after a commit sees it immediately.

**Consequence:** a reader replica *can* append to a lake-backed table
directly. Write-forwarding to the single writer is not required, and the
"readers would lose write access" objection to moving chat/events into the
lake does not hold.

## S2 — compaction: **every entry point is a silent no-op**

`DATA_INLINING_ROW_LIMIT 0` is mandatory (inlining stores rows in the
catalog — the residency problem the move exists to escape,
`2026-07-17-ducklake-data-inlining.md`). That means one Parquet object per
autocommitted INSERT, which is exactly the shape `append_chat_message` and
`capture_event` produce. Confirmed: **200 single-row appends → 200 Parquet
files.**

ADR-0009:81-83 names `ducklake_merge_adjacent_files` as the follow-up that
would fix this. It does not. Neither does anything else this build exposes:

```
CALL ducklake_merge_adjacent_files('lake');              -> files=101 rows=100 err=None
CALL ducklake_merge_adjacent_files('lake', 'appends');   -> files=101 rows=100 err=None
CALL ducklake_rewrite_data_files('lake');                -> files=101 rows=100 err=None
CALL ducklake_rewrite_data_files('lake', 'appends');     -> files=101 rows=100 err=None
```

(started at 100 files / 100 rows). Rows are preserved; the file count is
not reduced by any call, and none of them errors — so a naive
implementation would "run compaction" forever with no effect and no signal.

Maintenance functions available on this build:
`ducklake_cleanup_old_files`, `ducklake_expire_snapshots`,
`ducklake_flush_inlined_data`, `ducklake_merge_adjacent_files`,
`ducklake_rewrite_data_files` (the last two with two overloads each).

**Measurement trap worth recording:** the first run of this spike counted
files with `SELECT count(*) FROM ducklake_table_info('lake') WHERE
table_name = 'appends'`, which returns one row per TABLE, not per file — it
reports `1` regardless. That made compaction look like a working no-change.
Count files by globbing the data path
(`glob('s3://.../**/*.parquet')`), which is what the data-inlining note
does.

## S3 — the same lake can be attached twice on one connection: **works**

One connection attached the lake `READ_ONLY` as `lake` (the corpus shape
`adopt_lake` needs) and the SAME lake read-write under a second alias:

```
second attach err=None, rw insert=1, read_only alias sees 2 row(s)
```

The read-only alias observes the read-write insert immediately, with no
re-attach. So a lake-backed chat/events surface is a second attach
alongside the corpus one — mirroring how `chat_pg` / `events_pg` /
`crdt_pg` sit beside it today — and the reader's corpus attach does not
have to become read-write.

## S4 — self-compaction via `CREATE OR REPLACE TABLE`: **works, and fixes S2**

No built-in compaction call does anything (S2). But `publish_lake` already
compacts the corpus tables by construction: ADR-0009:81-83 notes
`CREATE OR REPLACE TABLE ... AS SELECT` "rewrites all Parquet per
publish". Applied to an append-shaped table:

```
100 appends → files before=100 after_replace=101 after_gc=1, rows 100->100
```

`CREATE OR REPLACE TABLE lake.t AS SELECT * FROM lake.t` writes one
consolidated file; the superseded files are still referenced by older
snapshots, so `ducklake_expire_snapshots` + `ducklake_cleanup_old_files`
(both already implemented in `gc_lake_snapshots`) is what actually removes
them. **100 files → 1.** Rows preserved.

**This is the missing compaction primitive**, built from machinery that
already exists in this repo rather than from a ducklake feature that does
not work.

## What this means for the design

Taken together the four spikes clear the path:

- **S1** — readers can append directly; no write-forwarding, and none of
  the 11 reader-servable tools regress.
- **S3** — the surface is a second attach beside the corpus, not a change
  to the corpus attach.
- **S2 + S4** — file-per-append is real and ducklake's own compaction is a
  no-op, but a periodic `CREATE OR REPLACE TABLE` + expire + cleanup
  collapses the table back to one file. File count is therefore bounded by
  *(compaction interval × append rate)*, not unbounded.

Note what S4 does **not** need: batching appends into a flush window. That
was the obvious mitigation for file-per-append, but it would have delayed
cross-replica visibility by the flush interval — breaking the
read-your-writes invariant asserted in `ducklake_chat_live.rs:98`
("a separate replica sees the row immediately"). Per-append commit plus
periodic self-compaction keeps that invariant and bounds the file count.

The cost moves to compaction instead: `CREATE OR REPLACE` rewrites the
whole table, so it is O(rows) per run. That makes retention
(`delete_chat_history(before_ts)`) the thing that bounds compaction cost,
and the compaction interval a tunable trade between object count and
rewrite volume.

The spike assertions in `ducklake_spikes_live.rs` deliberately pin the
current no-op behaviour. If a ducklake upgrade starts compacting, that test
fails loudly and this decision should be revisited.
