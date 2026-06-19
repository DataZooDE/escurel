-- Group membership: the canonical store for custom-group RBAC (group
-- ACL v1). A `group_members` row says principal `subject` belongs to
-- the named `group_id` in this tenant. Membership is the source of
-- truth for groups escurel itself manages — a header may also name a
-- group that arrives on the JWT instead, in which case no row is needed.
--
-- Current-state only: NOT CRDT/time-travelled like page content. The
-- `added_at`/`added_by` columns are a lightweight audit trail (who
-- granted it, when), not a replayable history. Reserved group names
-- (`public`/`owner`/`admin`) must never be stored here; they are
-- resolved structurally and stripped before the ACL intersection.
--
-- Created with IF NOT EXISTS on EVERY connection (see
-- `Migrator::ensure_group_members`), not only on a fresh DB: the v1
-- schema has no version framework, so this is how an already-provisioned
-- tenant DB gains the table on the next boot.
CREATE TABLE IF NOT EXISTS group_members (
    group_id   VARCHAR NOT NULL,   -- the group name
    subject    VARCHAR NOT NULL,   -- the principal `sub`
    added_at   TIMESTAMP NOT NULL DEFAULT now(),
    added_by   VARCHAR,            -- admin sub who granted it (audit)
    PRIMARY KEY (group_id, subject)
);
CREATE INDEX IF NOT EXISTS group_members_subject ON group_members (subject);
