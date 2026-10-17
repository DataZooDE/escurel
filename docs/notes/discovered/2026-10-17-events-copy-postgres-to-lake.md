# Switching the events/chat backend orphans the rows: `events-copy` is the supported move

**Date:** 2026-10-17. **Where:** `ESCUREL_EVENTS_BACKEND` / `ESCUREL_CHAT_BACKEND`, `escurel-server events-copy`.

## Symptom

heron-escurel keeps `escurel_events`, `escurel_drafts` and `escurel_chat_messages` in the catalog Postgres (the default
backend is `postgres`). Setting the two variables to `ducklake` boots without error and starts EMPTY lake tables; the
Postgres rows (the inbox, the pending drafts, audit events such as the `kind` migration, the chat) are still there but
nothing reads them. Nothing warns. Drafts are the easy one to miss: they follow the events selector and have no knob.

## Fix

`escurel-server events-copy [--apply]` copies one tenant's rows, stopped writer, idempotent, dry run by default,
runbook `docs/deploy/events-to-lake.md`. Design choices worth knowing:

- **One `INSERT … SELECT … WHERE NOT EXISTS` per table.** On DuckLake that is one atomic snapshot and one Parquet file,
  so a failed run leaves a table untouched or complete; resume across tables is just a re-run. Batching would lose that
  and add one snapshot per batch (each ~seconds on Drive).
- **The lake has no PRIMARY KEY**, so idempotency is the anti-join on the logical key (`event_id`, `draft_id`, `msg_id`)
  plus the tenant. A row present but different is `diverged` and refuses the apply instead of being "fixed".
- **Columns are the lake's.** The Postgres events table has `created_at`; the lake table does not (nothing reads it),
  so it is not copied. A source column that predates a lake column (`seq`, `kind`, lineage) reads as NULL.
- **The source is attached `READ_ONLY`**, so rolling back is switching the variables back.

## How to recognise it next time

After a backend switch the inbox is empty although `SELECT count(*) FROM escurel_events` in the catalog database is not.
Run `events-copy` (dry run) with the writer stopped: `to_copy` equals the orphaned rows.
