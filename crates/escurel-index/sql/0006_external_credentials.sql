-- External-source credential registry (SQL-view backend, REQ-SQL-05 / D10).
--
-- A row maps an admin-registered `name` (the value a skill's
-- `backend.source.attach:` references) to the secret material needed to
-- reach an external source (a DSN, an S3 secret spec, ...). Secrets are
-- recorded here — server-side, in `kb.duckdb` — and NEVER in the `pages/`
-- markdown corpus, so the canonical corpus stays secret-free and
-- git-diffable, and `tenant_export` (which tars only `markdown/`) never
-- carries a secret.
--
-- This table is a SEPARATE canonical input (REQ-NF-01 lists "registered
-- creds" alongside `pages/` + `blobs/`): it is NOT derivable from the
-- markdown corpus, so `rebuild` must NOT drop it. Like `group_members` it
-- is created with IF NOT EXISTS on EVERY connection (the v1 schema has no
-- version framework), so an already-provisioned tenant gains the table on
-- the next boot.
CREATE TABLE IF NOT EXISTS external_credentials (
    name       VARCHAR PRIMARY KEY,   -- the `attach` name skills reference
    connector  VARCHAR NOT NULL,      -- postgres|mysql|sqlite|erpl|s3|...
    secret     VARCHAR NOT NULL,      -- DSN / secret material (server-side only)
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    created_by VARCHAR                -- admin sub who registered it (audit)
);
