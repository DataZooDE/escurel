-- Scenario overlays (A/B/C "what-if" branches).
--
-- A nullable `scenario` column on pages + blocks. NULL means the page
-- belongs to the shared base timeline; a non-null value (e.g. 'B')
-- marks a what-if overlay that adds or overrides base pages without
-- tombstoning them. Reads pass an optional scenario and resolve
-- slug collisions with `QUALIFY ROW_NUMBER() OVER (PARTITION BY slug
-- ORDER BY scenario NULLS LAST) = 1` — the overlay wins over base.
--
-- Lives in its own staged batch so the scenario bump is isolated from
-- the core tables and the chat-messages batch.
ALTER TABLE pages  ADD COLUMN scenario VARCHAR;
ALTER TABLE blocks ADD COLUMN scenario VARCHAR;

-- Scenario-scoped event-log scan: (scenario, skill, at_ts).
CREATE INDEX pages_scenario ON pages (scenario, skill, at_ts);
