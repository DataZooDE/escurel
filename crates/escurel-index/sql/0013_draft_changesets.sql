-- Changesets: the held writes of ONE run, decided together (escurel#509 §1).
--
-- A draft is one page; a run is usually not. An inbox agent filing a call
-- transcript touches the customer instance, creates an interaction instance
-- and updates a decision record. As three independent drafts those get
-- promoted one at a time, and a reviewer who promotes two and discards the
-- third leaves the corpus in a state no agent proposed — the integrative
-- breadth `docs/contract/compile-first-wiki.md` G1 wants makes that worse,
-- not better.
--
-- One nullable column, deliberately. NULL is today's draft, byte for byte:
-- an ungrouped draft is created, listed, promoted and discarded exactly as
-- before, so nothing that predates changesets has to know they exist. A
-- `NOT NULL DEFAULT` would have invented a changeset for every historical
-- row — a group of one is not a group, and asserting otherwise would make
-- `list_changesets` a second, noisier copy of `list_drafts`.
--
-- The id is minted by the SERVER on first use and echoed back, for the same
-- reason `event_id` is: a client-chosen grouping key collides across runs,
-- and a collision here merges two agents' proposals into one decision.
--
-- Idempotent (ADD COLUMN IF NOT EXISTS) and run on EVERY connection via
-- `Migrator::ensure_drafts`, like `pages.last_written_by` (0011), so a
-- tenant DB provisioned before changesets existed gains the column on the
-- next boot rather than in a migration step an operator has to remember.
ALTER TABLE drafts ADD COLUMN IF NOT EXISTS changeset_id VARCHAR;

-- The queue read is "every open changeset, newest first", which is a scan
-- over this column plus `status`.
CREATE INDEX IF NOT EXISTS drafts_changeset ON drafts (changeset_id, status);
