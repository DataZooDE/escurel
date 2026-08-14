-- Draft versions: a held write that has not landed yet (CR-2, #354).
--
-- A draft is NOT a separate object. It is a version whose status is draft —
-- escurel already has versions (`v<hlc>`), so a proposed change to an existing
-- page is simply the next version, unpublished. This table records WHICH
-- versions are pending; it deliberately stores no content, because the content
-- is already in `crdt_snapshots` at that version and `body_from_snapshot`
-- recovers it. Two copies of a document that must agree is a bug waiting for a
-- deployment to age.
--
-- One pending draft per page, by primary key on `page_id`. That is a decision:
-- a queue of competing drafts against one page is a merge problem, and the
-- reviewer would be choosing between diffs computed against different bases —
-- exactly the situation `require_exact_base` exists to refuse. A second draft
-- replaces the first, which is also what a re-drafting agent means.
--
-- `drafted_by` is the server-stamped principal, same source as
-- `pages.last_written_by` (#357) and never anything the caller sent. It is
-- what makes maker != checker enforceable: an approval by the author is not a
-- review.
--
-- Idempotent (CREATE TABLE IF NOT EXISTS) and run on EVERY connection via
-- `Migrator::ensure_page_drafts`, like the write-attribution columns (0011),
-- so a tenant DB provisioned before drafts existed gains the table on the
-- next boot without a migration step an operator has to remember.
CREATE TABLE IF NOT EXISTS page_drafts (
    page_id      VARCHAR PRIMARY KEY,
    version      VARCHAR NOT NULL,
    snapshot_hlc BIGINT  NOT NULL,
    drafted_by   VARCHAR,
    base_version VARCHAR,
    created_at   TIMESTAMP NOT NULL
);
