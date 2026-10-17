# A runner's health is one overwritten record, not a heartbeat stream

**Symptom.** On the lab's shared Postgres events table (`escurel_events`) about 58k
`escurel:runner-status` rows piled up: one heartbeat per runner every 30 s, and a keep-50 prune
(#554) that never ran on the old gateway image. Every poll then read the table out of Cloud SQL
(the scanner does not push `COALESCE(kind,'user') <> 'system'`, `ORDER BY` or `LIMIT` down),
which cost ~123 GiB/day of egress. #729 pushed the reads down, #730 slowed the heartbeat to five
minutes and #731 added `admin purge-events`. Those limit the cost; they keep a stream whose only
useful row is the newest.

**Fix.** The runner reports through `report_runner_status` (`escurel-server`
`mcp/tools_status.rs`), which calls `Indexer::upsert_runner_status`: ONE row per runner,
`runner-status:<runner_id>`, `kind: system`, label `escurel:runner-status`, overwritten in place
(`at_ts`, `title` = the runner's state, `body`, `provenance`). The row sorts by its fresh `at_ts`,
so the existing newest-first label read still finds it first, and the table no longer grows.

- **Backends.** Local and lake: `UPDATE`, then `INSERT` (via `capture_event`) when 0 rows
  changed (DuckLake has no primary key to conflict on). Attached Postgres: one native
  `INSERT … ON CONFLICT (tenant, event_id) DO UPDATE` through `postgres_execute`, with the next
  `seq` computed inside Postgres.
- **Backlog.** A runner's FIRST report (the record did not exist) also deletes that runner's older
  `escurel:runner-status` rows, matched by `provenance.runner_status.runner_id`. On Postgres that
  is one `DELETE` inside the database (58,000 rows, the lab's backlog: ~130 ms against a local Postgres). Rows of runners that no longer
  exist (a pod that was renamed) are not matched; `escurel admin purge-events` clears those.
- **Gate.** Same bookkeeper rule as a reserved-label `capture_event` (admin, not run-bound); the
  `runner-status:` id prefix is minted only by the new tool (`capture_event` refuses it). No
  webhook and no `/ws` fan-out.
- **Compatibility.** A runner whose gateway answers `-32601` (an image older than the tool, e.g.
  heron's `6d942410ef13`) falls back, once, to the old `capture_event` heartbeat path. A new
  gateway keeps pruning those to 50. The extension reads `body.status_interval_s` and, for an
  old runner, the median gap of that runner's own rows.

**How to recognise the next problem like it.** A label whose only used row is the newest, written
on a timer into a table that is read by scanner: count rows per label, then look at
`pg_stat_user_tables` (`seq_tup_read + idx_tup_fetch`) around one call.

**Gotchas met while building it.**

- `get_event` (the usual read-back) pulled the whole Postgres table through the scanner when the
  row existed; the native path therefore builds the returned `EventInfo` from what it wrote.
- `pg_stat_user_tables` counters of a statement run on DuckDB's long-lived Postgres connection are
  flushed lazily, so the first statements after a big `DELETE` are charged with its rows. The live
  test does two warm-up reports before it measures.
- `pg_literal` refuses a value with a backslash, so a body containing one takes the generic
  (scanner) path; it is correct, only less cheap.

**Follow-up (2026-10-17): the record of a replaced pod.** With the default `ESCUREL_RUNNER_ID`
(`<HOSTNAME>:<pid>`, the pod name on k8s) every redeploy leaves a record nobody overwrites, and the
first-report cleanup only matches the runner's own id. Fix: every report also deletes other
`runner-status:*` records of the tenant silent for longer than `max(10 x their own status_interval_s, 1 h)`,
judged by the *reporting runner's* `at` (not the gateway's wall clock). It cannot run on the first report
alone: during a Recreate rollout the old pod's last report is seconds old, so the window has not elapsed;
it has to run on later reports. The candidate query is a primary-key range (`event_id >= 'runner-status:' AND
< 'runner-status;'`); a plain `LIKE` or the label alone read the whole legacy backlog again (5,037 rows per
report in the live test). A stable default id was not added: the GC makes the pod-name id harmless.
