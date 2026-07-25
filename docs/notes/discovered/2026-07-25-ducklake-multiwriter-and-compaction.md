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

## What this means for the design

An append-per-message lake table grows one S3 object per message,
unbounded, with no working compaction. Mitigations, in order of strength:

1. **Batch appends into one transaction** — one file per flush window
   rather than per message. Makes file growth proportional to *time*, not
   traffic. Necessary regardless; not sufficient on its own.
2. **Retention** — deleting old rows also rewrites files on a lake, so it
   bounds reads but not necessarily object count.
3. **Keep the hot, high-cardinality surface on a transactional store** and
   use the lake for the cold/bulk tail — the shape `docs/spec/storage.md`
   already prescribes for high-volume events.

S1 removes the *correctness* objection to lake-backed chat/events. S2 is an
*operational* one and it is unresolved: it needs a decision, not a
workaround, before the surface is built.

The spike assertions in `ducklake_spikes_live.rs` deliberately pin the
current no-op behaviour. If a ducklake upgrade starts compacting, that test
fails loudly and this decision should be revisited.
