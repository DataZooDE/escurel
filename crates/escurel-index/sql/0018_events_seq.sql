-- Events: `seq`, the ingestion position (hardening H3, 2026-09-22).
--
-- The tails the runner and the workbench keep on a label, a lineage or a
-- run resumed by an `(at, event_id)` cursor. Both halves are caller-
-- controlled: an event captured AFTER a poll with an EARLIER `at` (a
-- backdated import, a client's clock) sorted before the cursor and was
-- never seen. `seq` is assigned at capture — the next number after the
-- tenant's highest, under the single writer's lock — so a listing by
-- label / root / run is append order and a cursor over it never skips.
-- A page's own history and the inbox stay in `at` order.
--
-- Nullable on purpose (an `ADD COLUMN … NOT NULL` needs a rewrite); every
-- existing row is backfilled in `(at_ts, event_id)` order, the order the
-- tails used until now, so no existing cursor position is reordered.
-- Applied ONLY by `Migrator::ensure_events_seq` — presence-checked and
-- CHECKPOINTed, because `events.created_at` carries a function-valued
-- DEFAULT and an ALTER left in the WAL is unreplayable
-- (docs/notes/discovered/2026-09-16-alter-on-a-defaulted-table-poisons-the-wal.md).
-- A fresh database gets the column from `0004_events.sql` directly.
ALTER TABLE events ADD COLUMN IF NOT EXISTS seq BIGINT;
UPDATE events SET seq = numbered.rn
  FROM (SELECT event_id, row_number() OVER (ORDER BY at_ts NULLS LAST, event_id) AS rn FROM events) AS numbered
 WHERE events.event_id = numbered.event_id AND events.seq IS NULL;
CREATE INDEX IF NOT EXISTS events_seq ON events (seq);
