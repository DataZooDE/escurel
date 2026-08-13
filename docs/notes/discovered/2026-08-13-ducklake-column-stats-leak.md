# DuckLake column stats put customer text in the catalog

**Date:** 2026-08-13 · **Found by:** the `no_payload_in_catalog_live`
guard test (#309), first run.

## Symptom

The residency guard — write sentinel strings through pages/chat/events,
then grep every catalog table — failed immediately on a fresh lake:

```
customer bytes leaked into the catalog (CUSTOMER-PAYLOAD-PAGE-…):
public.ducklake_file_column_stats: (…)
public.ducklake_table_column_stats: (…)
```

DuckLake maintains per-column `min_value` / `max_value` **as VARCHAR**
in `ducklake_table_column_stats` and `ducklake_file_column_stats`. For a
text column those bounds are literal customer values — for a
small-cardinality file, the *complete* page body / chat message / event
body — sitting in the catalog database (Cloud SQL on GCP in
production), which substrate SPEC §5 reserves for metadata. This is a
third residency door, independent of the two known ones (data inlining,
attached Postgres append tables).

## What does NOT fix it

- `ducklake_set_option` has **no option** covering stats — probed
  `column_stats`, `write_column_stats`, `max_string_stats_length`, and
  friends; all "Unsupported option".
- `ATTACH … (ENCRYPTED)` encrypts the **parquet files** and still
  writes plaintext min/max into the catalog. Verified empirically: the
  full sentinel string appeared in `ducklake_table_column_stats` of an
  encrypted lake.

## The fix

`scrub_column_stats_sql` (`crates/escurel-index/src/snapshot/lake.rs`):
after every commit escurel makes — corpus `publish_lake`, append-table
`compact_append_table` — blank `min_value` / `max_value` / `extra_stats`
in both stats tables through the `__ducklake_metadata_<alias>` side
catalog (present for the DuckDB-file and the Postgres catalog alike, even
though `duckdb_databases()` does not list it).

NULL bounds are spec-legal "unknown": DuckLake's own pruning query in
the spec skips a file when `min_value IS NULL`. escurel's readers adopt
whole tables and never prune on these columns, so nothing regresses.

## Residual window

Appends between two publishes leave their stats in the catalog until the
next publish/compaction pass (bounded by the effective publish interval —
300s on a lake-append deployment). A crash after commit but before the
scrub leaves them until the next publish. Convergent, not transactional.

## How to recognise it next time

Any grep of the catalog showing customer text in a `ducklake_*` table —
check the stats pair first. If a DuckLake upgrade adds a real
disable-stats option, replace the scrub with it and keep the guard test;
the guard is the thing that catches the next door.
