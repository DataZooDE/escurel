-- Drafts: the RUN that proposed a held write (knowledge-workbench backend, P1).
--
-- `run_id` / `root_event_id` are stamped by the gateway from the caller's
-- per-run agent token (the claims the runner signed), never from an
-- argument — so a lineage tree can hang a run's drafts and changesets off
-- its root event with one indexed read, and a draft cannot be filed under a
-- run its author did not execute. NULL for a draft no run proposed (a human's).
--
-- Applied ONLY by `Migrator::ensure_draft_run_lineage` — presence-checked and
-- CHECKPOINTed, because `drafts.created_at` carries a function-valued DEFAULT
-- and an ALTER left in the WAL is unreplayable (see 0013 / the 2026-09-16 note).
ALTER TABLE drafts ADD COLUMN IF NOT EXISTS run_id        VARCHAR;
ALTER TABLE drafts ADD COLUMN IF NOT EXISTS root_event_id VARCHAR;

CREATE INDEX IF NOT EXISTS drafts_run  ON drafts (run_id);
CREATE INDEX IF NOT EXISTS drafts_root ON drafts (root_event_id);
