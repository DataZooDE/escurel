# The runner heartbeat default (30 s) spent the lake's snapshot retention in hours

**Date:** 2026-10-16. Companion of
[`2026-10-16-events-table-read-in-full-on-attached-postgres.md`](2026-10-16-events-table-read-in-full-on-attached-postgres.md).

## Symptom

On the lab's dz-escurel (events on the lake, `ESCUREL_EVENTS_BACKEND=ducklake`, `ESCUREL_SNAPSHOT_KEEP=2100`,
publish tick 300 s) the catalog showed ~106 scans a minute of `ducklake_file_column_stats`, ~93 % of whose rows belonged to
files long retired, and `as_of` history reached back only hours.

## Cause

Not a missing maintenance step: the periodic publish task already runs `compact_append_lake` and `gc_lake_snapshots`
every cycle (`escurel-server/src/snapshot_publish.rs`), and a regression test now pins that pair keeps the catalog bounded
(`events_lake_churn_live.rs`: 193 snapshots -> the retention count, no statistics rows of vanished files).

The amplifier is the **heartbeat**. Every `escurel:runner-status` row is one lake snapshot (inlining is off), and the
gateway's prune after it deletes a row: a second snapshot. At the old 30 s default that is ~4,600 snapshots a day per
runner; a retention of `ESCUREL_SNAPSHOT_KEEP=2100` snapshots then covers about **11 hours**, not the days the setting
suggests, and the retired-but-retained files (and their statistics rows) pile up behind every scan.

## Fix

`ESCUREL_RUNNER_STATUS_INTERVAL` defaults to **5 minutes** (was 30 s): ten times fewer rows and snapshots (~460 a day,
retention at the same `KEEP` ~4.5 days). The readiness window follows (twice the interval + 5 s). The VS Code extension's
default assumption moves with it (`ESCUREL_RUNNER_STATUS_INTERVAL_MS`, 300,000); it always prefers the interval
OBSERVED from the heartbeat rows, so deployments that set their own value are unaffected, and its staleness rule (three
intervals) now also tolerates a runner that polls every ten minutes.

Left as is, on purpose: the prune keeps exactly the newest 50 rows (a lazy prune would halve the snapshot churn again, but
changes the documented "keeps the last 50" contract; revisit if the lake still churns).

## Changelog lines for the release notes

* Changed: `ESCUREL_RUNNER_STATUS_INTERVAL` now defaults to `5m` (was `30s`). Every heartbeat is an event row and, on a
  lake events backend, a snapshot; set the variable to keep a different cadence. The runner's `/readyz` staleness window
  (twice the interval) and the VS Code runner view follow the new default.
