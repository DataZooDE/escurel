# The attached-Postgres events table was read in full by every list, capture and prune

**Date:** 2026-10-16. **Found by:** a Cloud SQL cost alert (internet egress on the DuckLake catalog instance grew from
18 GiB/day to 123 GiB/day, about EUR 200 of EUR 215 in a month).

## Symptom

~90 % of the rows leaving the catalog instance came from heron-escurel's database, from one table:
`public.escurel_events` (60k rows, 52 MB), read in full about six times a minute by the runner's poll (one `list_inbox`
plus two label-filtered tails), and once more by every `capture_event`.

## Cause

Two things, one of them a deployment fact.

1. The runner heartbeat (`escurel:runner-status`, every 30 s) is pruned to the newest 50 rows by the gateway. That prune
   landed in #554 (2026-09-22); heron-escurel ran an image built before it, so ~58k heartbeats accumulated. (Deploying a
   current gateway prunes them on the first heartbeat; nothing else is needed for the backlog.)
2. Even with a pruned table the engine could not keep the cost flat, because on the attached-Postgres backend the events
   SQL ran through DuckDB's postgres scanner, which only pushes simple column comparisons into Postgres:
   * `COALESCE(kind, 'user') <> 'system'` (the filter that hides bookkeeping from the inbox), `ORDER BY` and `LIMIT`
     ran in DuckDB after the scan, so every inbox read transferred every hidden system row;
   * `capture_event`'s next position, `COALESCE((SELECT MAX(seq) ...), 0) + 1`, scanned the tenant's whole `seq` column;
   * the prune's `DELETE ... NOT IN (SELECT ... LIMIT n)` pulled the whole label across the wire first.

## Fix

On the attached-Postgres backend only (the DuckLake and local backends are unchanged):

* bounded listings (`list_inbox`, `list_events` with a limit) and every paged listing (`list_inbox_page`,
  `list_events_page`, `list_events_filtered_page`, the lineage and tail reads) run natively through `postgres_query`:
  the whole `WHERE ... ORDER BY ... LIMIT` runs inside Postgres, values inlined as quoted literals (a value that cannot
  be quoted with certainty, i.e. containing a backslash or NUL, falls back to the scanner path);
* `capture_event` reads the next `seq` with a native `max(seq)`;
* `prune_label_events` and `prune_run_progress` run their `DELETE` inside Postgres (`postgres_execute`);
* `attach_events_pg` creates the supporting indexes idempotently: `(tenant, label_skill, seq)`,
  `(tenant, status, at_ts DESC, event_id DESC)`, the partial inbox index
  `(tenant, at_ts DESC, event_id DESC) WHERE status = 'inbox' AND COALESCE(kind, 'user') <> 'system'` (its predicate must
  match the listing's, which is why the listing spells the filter exactly so) and `(tenant, instance_page_id, at_ts)`.

## Numbers

* Live Postgres, 5,000 hidden heartbeats + 3 user events (`events_pg_cost_live.rs`, rows of the events table read per
  call, from `pg_stat_user_tables`): inbox, paged inbox and capture: **5,003 rows read before, 2 after**; the newest
  heartbeat (a label tail, newest first): **5,003 before, 14 after**; the prune of a 2,000-row backlog: **2,003 before,
  2 after**.
* A 58,000-row prune, local Postgres: scanner path 2.6 s for 116k rows (twice the backlog) vs native 0.22 s.
* The same prune statement on a copy of the lab's heron catalog (59,368 `escurel:runner-status` rows, 131 other rows):
  **1.09 s, 59,318 deleted, the 131 other rows untouched.**

## How to recognise it next time

* Cloud SQL egress grows linearly with the age of an events table; `pg_stat_statements` shows `COPY (SELECT ... FROM
  escurel_events ...) TO STDOUT` at the top of rows returned.
* In a test: `pg_stat_user_tables.seq_tup_read + idx_tup_fetch` for `escurel_events` around one call is the whole table
  instead of a handful of rows. Do not use `pg_stat_database.tup_returned` for this: reading it perturbs it and its
  flush is lazy (a long-lived probe connection releases seeding counters seconds later, into a later measurement).

## Changelog lines for the release notes

* Fixed: on the attached-Postgres events backend (heron-escurel), `list_inbox`, the paged event listings,
  `capture_event` and the runner-status prune no longer read the whole `escurel_events` table through the DuckDB scanner;
  they run inside Postgres, with new supporting indexes created at attach. A deployment whose runner heartbeats were never
  pruned (a gateway older than #554) frees them at the first heartbeat after upgrading. No wire change.
