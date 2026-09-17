-- The branch registry (escurel#512 §1) + overlay tombstones (§3).
--
-- escurel already had the READ half of a branch: a nullable `scenario` on
-- pages/blocks, `base ∪ overlay` with a deterministic per-slug override
-- (`sql/0003_scenarios.sql`). What it had nowhere was the branch as a THING:
-- no author, no base, no status, no lifecycle. Scenarios were discovered by
-- grepping frontmatter, which makes "whose workspace is this and what did it
-- fork from?" unanswerable — and a merge impossible, because there is nothing
-- to compare against.
--
-- `base_version` is the corpus state the branch forked from. A merge needs it:
-- "did the base twin move since this branch started?" is the whole question,
-- and answering it from the pages themselves would mean trusting whatever the
-- branch happens to contain.
--
-- `status` moves `'open' → 'merged' | 'abandoned'` exactly once, and the row
-- is KEPT after that, like drafts and events: "did we already decide that
-- branch?" must stay answerable.
CREATE TABLE IF NOT EXISTS branches (
    name           VARCHAR PRIMARY KEY,               -- 'agent/inbox-scan'
    base_version   VARCHAR NOT NULL,                  -- corpus state forked from
    author         VARCHAR NOT NULL DEFAULT '',       -- the creating subject
    status         VARCHAR NOT NULL DEFAULT 'open',   -- open | merged | abandoned
    reason         VARCHAR NOT NULL DEFAULT '',       -- why it was abandoned
    decided_by     VARCHAR NOT NULL DEFAULT '',
    created_at     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    decided_at     TIMESTAMP
);

CREATE INDEX IF NOT EXISTS branches_status ON branches (status, created_at);

-- Tombstones (§3). An overlay could add or override but never DELETE, and a
-- branch that cannot express "this instance was wrong, remove it" is not a
-- branch.
--
-- The read seam already picks the overlay row first per slug
-- (`ORDER BY scenario NULLS LAST`), so a winning overlay marked deleted must
-- resolve to "not present" rather than falling back to its base twin. The
-- trap is named in docs/notes/discovered/2026-05-29-scenario-overlay-qualify.md:
-- flip the NULLS ordering and a delete silently shows the base value, with no
-- type error to catch it.
--
-- Nullable BOOLEAN, defaulting to NULL rather than FALSE: every page that
-- predates tombstones is "not a tombstone", and NULL says that without
-- rewriting history.
ALTER TABLE pages ADD COLUMN IF NOT EXISTS deleted BOOLEAN;
