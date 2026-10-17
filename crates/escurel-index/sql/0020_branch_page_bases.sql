-- Where each branch page forked from (the 2026-10-17 crew review, engine unit).
--
-- `branches.base_version` is one string for the whole branch, but a page's CRDT version is that page's own op
-- count, so no single value can say "the base twin has not moved since the fork". The branch recorded `v0` (the
-- version of the page id '', which has no ops), and a merge of any page that already had history compared its
-- head against `v0` and reported a conflict nobody had caused; a branch tombstone had no fork point at all and
-- deleted a page the base edited in the meantime.
--
-- One row per (branch, BASE page id), written by the branch's first write to that page: the base twin's version
-- at that moment. The first write wins (a later branch write must not move the fork point). A page with no row
-- (a page created on the branch, a branch from before this table, a gateway without a CRDT backend) merges the
-- way it always did, against `branches.base_version`.
--
-- Plain table, no defaults: CREATE TABLE IF NOT EXISTS is replay-safe, so it runs on every boot like the
-- `branches` registry it belongs to. A separate canonical input, not derivable from `pages/`: `rebuild` must not
-- drop it.
CREATE TABLE IF NOT EXISTS branch_page_bases (
    branch        VARCHAR NOT NULL,
    base_page_id  VARCHAR NOT NULL,
    base_version  VARCHAR NOT NULL,
    PRIMARY KEY (branch, base_page_id)
);
