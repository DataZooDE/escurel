# A lake GC over a backlog held the index connection for 31 minutes and stalled every read

**Symptom.** heron-escurel (lake and lanes on `gdrive://`) answered `/healthz` but no tool call for 31 minutes after a
single `update_page`. Every `expand`, `list_events` and `report_runner_status` call started and did not finish. Then, with
no restart, everything was fast again. The log around it:

```
15:25:10  tool.completed update_page
15:25:26  periodic publish: published a new snapshot   snapshot_id=3654
          (no tool.completed for 31 minutes)
15:56:09  periodic publish: gc_lake_snapshots failed (publish itself still committed)
          … Drive API quota exceeded … User rate limit exceeded.
15:56:18  tool.completed report_runner_status
```

**Cause.** `gc_lake_snapshots` ran its whole pass (`ducklake_expire_snapshots`, then
`ducklake_cleanup_old_files(cleanup_all => true)`) on the indexer's connection, under its connection mutex and its write
guard. Every read tool takes that mutex. On heron the pass had a large backlog. Every event and chat append to the lake
is a DuckLake snapshot (3654 of them), but GC runs only after a page publish, and page writes on heron are rare. So the
first publish in a long time had about 3,650 snapshots to expire and their Parquet files to delete. Drive deletes one
file per request and rate-limits, so the pass ran until Drive refused it. The failed pass leaves the backlog in place,
so the next publish starts it again.

**Fix.** With a Postgres catalog (every deployment) the GC opens its own lake connection, so a read never waits on
it. A DuckDB-file catalog (tests, local dev) can't be shared that way: a second DuckDB instance's catalog writes were
lost under the first one's (`admin_publish::expire_keeps_recent_snapshots` caught it), so there the GC stays on the
indexer's connection and frees it between chunks. It expires in chunks (`GcOptions::chunk`,
100 snapshots), and each chunk holds the write guard only for the catalog-only expire step. The file cleanup after each
chunk takes no write guard, because no snapshot references those files any more. A separate `gc_lock` keeps two passes from
deleting the same files. Test:
`crates/escurel-index/tests/suite/ducklake_publish.rs::a_read_and_a_write_get_through_while_gc_works_through_a_backlog`.

**Recognise it.** The gateway is healthy but its tool calls hang, and the last line before the gap is `periodic
publish: published a new snapshot`. A restart does not help: it costs a cold boot, and the backlog is still there
afterwards. On heron the pass had already ended when the pod was restarted.
