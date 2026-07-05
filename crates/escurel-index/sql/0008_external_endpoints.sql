-- Remote-backend endpoint registry (openapi/mcp backends, REQ-REMOTE-05).
--
-- A row maps an admin-registered `name` (the value a skill's
-- `backend.endpoint:` references) to the base URL + auth material needed to
-- reach an upstream REST/OpenAPI service or MCP server. The base URL and any
-- secret are recorded here — server-side, in `kb.duckdb` — and NEVER in the
-- `pages/` markdown corpus. This is the SSRF / secrets-in-markdown guard: a
-- live remote instance can only be pointed at an admin-registered endpoint,
-- never at a raw URL carried in tenant markdown, and `tenant_export` (which
-- tars only `markdown/`) never carries a secret.
--
-- Like `external_credentials` this is a SEPARATE canonical input (REQ-NF-01):
-- NOT derivable from the markdown corpus, so `rebuild` must NOT drop it, and
-- it is created with IF NOT EXISTS on EVERY connection so an already-
-- provisioned tenant gains the table on the next boot.
CREATE TABLE IF NOT EXISTS external_endpoints (
    name        VARCHAR PRIMARY KEY,   -- the `endpoint` name skills reference
    kind        VARCHAR NOT NULL,      -- openapi|mcp
    base_url    VARCHAR NOT NULL,      -- server-side only (SSRF allow-list anchor)
    auth_scheme VARCHAR NOT NULL DEFAULT 'none',  -- none|bearer|api_key
    auth_header VARCHAR,               -- header name when auth_scheme = api_key
    secret      VARCHAR,               -- bearer token / api-key material (server-side only)
    created_at  TIMESTAMP NOT NULL DEFAULT now(),
    created_by  VARCHAR                -- admin sub who registered it (audit)
);
