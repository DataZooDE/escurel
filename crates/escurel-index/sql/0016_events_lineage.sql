-- Events: `kind` + indexed lineage columns (knowledge-workbench backend, P1).
--
-- `kind` separates the two things the event bus now carries:
--   - 'user'   — work: something happened that a skill should fold into an
--                instance (every event before this migration).
--   - 'system' — bookkeeping ABOUT a run, written by the runner or the
--                gateway (`escurel:run`, `escurel:review`, …). Never work
--                for a human or an agent: it skips the inbox (captured with
--                a target page it lands `processed` at once) and the list
--                surfaces hide it unless asked (`include_system`).
-- `root_event_id` / `run_id` are promoted from `provenance.runner` so a
-- lineage ("everything under root event E1") and a run's own events are
-- ONE indexed equality each, not a JSON scan. `capture_event` fills them
-- server-side; a user event is its own root.
--
-- Nullable on purpose: an `ADD COLUMN … NOT NULL` needs a rewrite, and
-- pre-migration rows read as `kind = 'user'` through `COALESCE` anyway.
-- Applied ONLY by `Migrator::ensure_events_lineage` — presence-checked and
-- CHECKPOINTed, because `events.created_at` carries a function-valued
-- DEFAULT and an ALTER left in the WAL is unreplayable
-- (docs/notes/discovered/2026-09-16-alter-on-a-defaulted-table-poisons-the-wal.md).
-- A fresh database gets the same columns from `0004_events.sql` directly.
ALTER TABLE events ADD COLUMN IF NOT EXISTS kind          VARCHAR DEFAULT 'user';
ALTER TABLE events ADD COLUMN IF NOT EXISTS root_event_id VARCHAR;
ALTER TABLE events ADD COLUMN IF NOT EXISTS run_id        VARCHAR;

CREATE INDEX IF NOT EXISTS events_root_at ON events (root_event_id, at_ts);
CREATE INDEX IF NOT EXISTS events_run_at  ON events (run_id, at_ts);
