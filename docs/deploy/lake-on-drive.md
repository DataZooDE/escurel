# A DuckLake on Google Drive: settings that keep it bounded

A lake whose `ESCUREL_DUCKLAKE_DATA_PATH` is `gdrive://…` pays a REST round trip per file operation: ~270 ms to open a
file (the extension revalidates its metadata), ~1.2 s for a cold media request, whatever the size. Every capture,
draft and chat message on a lake-backed append surface writes one Parquet object and one DuckLake snapshot
(`DATA_INLINING_ROW_LIMIT 0` is required: inlined rows would live in the catalog database). These are the settings and
behaviours that keep the file count, the per-call cost and the Drive trash bounded. They apply only to a `gdrive://`
data path; the lane store (`ESCUREL_STORAGE_DUCKVFS_*`) is a different DuckDB instance and is not touched.

## Applied automatically

- **`gdrive_immutable_prefixes` = the lake's `DATA_PATH`.** DuckLake never rewrites a data file in place (every write
  is a new, uniquely named Parquet file), so the extension may trust its cached file metadata instead of one
  `files.get` per open. A lake read opens one file per Parquet object, so this removes ~270 ms per file. Do not put
  anything rewritten in place under the `DATA_PATH`.
- **Compaction is data-driven.** The publish tick (`ESCUREL_SNAPSHOT_PUBLISH_SECS`, default 300 while an append
  surface is lake-backed) rewrites an append table (events, **drafts**, chat) to one file only once it holds at least
  `ESCUREL_APPEND_COMPACT_MIN_FILES` data files (default 8). Before, every tick rewrote the events and chat tables
  unconditionally (about 576 uploads a day even when idle) and the drafts table was never compacted. An idle system
  now uploads nothing; a read opens at most that many files between compactions.

## Knobs

| Variable | Default | Effect |
|---|---|---|
| `ESCUREL_APPEND_COMPACT_MIN_FILES` | `8` | Files a lake-backed append table may accumulate before the tick rewrites it. Raise it to rewrite less often (more files per read); `1` restores the old every-tick behaviour. |
| `ESCUREL_SNAPSHOT_PUBLISH_SECS` | `300` (append surface on the lake) | The tick interval. Lengthening it lengthens the compaction and GC cadence, so more files and snapshots exist in between. `0` disables compaction. |
| `ESCUREL_SNAPSHOT_KEEP` | `5` | Snapshots kept by the GC. A large value keeps superseded Parquet alive (and in Drive) for longer. |
| `ESCUREL_DUCKLAKE_GDRIVE_PERMANENT_DELETE` | `false` | The GC's expired Parquet goes to the Drive trash (recoverable, counts against quota until the trash empties; Drive empties it after 30 days). `true` sets `gdrive_permanent_delete` and deletes for good. |

## What a call costs, measured

File operations per call on a lake with 25 events, before and after compaction (a real local DuckLake, DuckDB's
`FileSystem` log; `crates/escurel-index/tests/suite/append_lake_compaction.rs`, run with `-- --nocapture`):

| call | OPEN before → after | READ before → after |
|---|---|---|
| `capture_event` | 27 → 5 | 65 → 71 |
| `list_inbox` | 25 → 2 | 350 → 387 |
| `list_events` (by label) | 25 → 2 | 375 → 408 |

On Drive, multiply: each OPEN costs ~0.27 s unless the prefix is declared immutable (it is, now), and each cold media
request ~1.2 s (16 MiB blocks, shared between threads). So an uncompacted 25-file table is on the order of 25 cold
requests (~30 s) per list, a compacted one a handful (~1–3 s). This is arithmetic from the extension's documented
latencies, not a measurement on Drive; the rehearsal on a separate Drive measures the real numbers.

## Capture on Drive

A capture is a synchronous lake write: one Parquet object and one snapshot, so expect seconds per capture, not
milliseconds (a Postgres-backed capture is ~13 ms). A producer that bursts (an import, a runner hopping a long
lineage) should batch or throttle.
