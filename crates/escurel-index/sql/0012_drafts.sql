-- Drafts: a proposed write, held for a human, that has not landed.
--
-- The gap this closes: a skill page may declare `autonomy: review`, and
-- `list_skills` publishes that declaration — but nothing could act on it,
-- because there was nowhere to put a change that is finished and not yet
-- wanted. An agent could only write or not write. Consumers therefore
-- invented private conventions (heron modelled a pending change as an
-- ordinary instance of its own `proposal` skill), which put a
-- consumer-shaped object in the knowledge base and made "what is waiting
-- for me?" a question only that consumer could answer.
--
-- A draft is NOT a page, and that is the point. It is never returned by
-- `expand`, `search`, `list_instances` or `neighbours`, so an unapproved
-- change cannot be mistaken for knowledge — by a reader, by an agent
-- reading context, or by the cascade. Keeping it out of `pages` makes that
-- true by construction rather than by an exclusion rule in every read path,
-- which is the kind of rule that is eventually forgotten in one of them.
--
-- Immutable after creation. A revised draft is a NEW row, never an edit of
-- an existing one: a human approves specific bytes, and bytes that can
-- change under an approval turn the gate into decoration. `content_sha256`
-- is what `promote_draft` compares against, and the same discipline the
-- page CAS already applies to the target (`base_sha256`, #354).
--
-- `status` moves `'open' → 'promoted' | 'discarded'` exactly once. Rows are
-- kept after that: "did I already deal with that?" must stay answerable,
-- which is the same reason the event store has no delete.
CREATE TABLE IF NOT EXISTS drafts (
    draft_id          VARCHAR PRIMARY KEY,
    target_page_id    VARCHAR NOT NULL,                 -- the page this write is FOR (may not exist yet)
    content           VARCHAR NOT NULL,                 -- the whole proposed markdown, frontmatter first
    content_sha256    VARCHAR NOT NULL,                 -- hex sha256 of `content`; the approval's byte binding
    base_sha256       VARCHAR,                          -- target's hash when drafted; NULL = drafted as a create
    author            VARCHAR NOT NULL DEFAULT '',      -- the subject that drafted it (an agent, normally)
    event_id          VARCHAR,                          -- the inbox event this answers, when it answers one
    status            VARCHAR NOT NULL DEFAULT 'open',  -- 'open' | 'promoted' | 'discarded'
    reason            VARCHAR NOT NULL DEFAULT '',      -- why it was discarded, when it was
    decided_by        VARCHAR NOT NULL DEFAULT '',      -- the subject that promoted or discarded it
    created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    decided_at        TIMESTAMP
);

CREATE INDEX IF NOT EXISTS drafts_status_created ON drafts (status, created_at);
CREATE INDEX IF NOT EXISTS drafts_target        ON drafts (target_page_id);
CREATE INDEX IF NOT EXISTS drafts_event         ON drafts (event_id);
