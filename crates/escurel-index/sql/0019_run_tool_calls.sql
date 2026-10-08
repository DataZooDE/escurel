-- `run_tool_calls`: one row per `/mcp` call made with a run-bound bearer
-- (knowledge-workbench backend P3-1). The workbench's per-run tool-call
-- view; sizes always, plus (ESCUREL_TOOLCALL_DETAIL=summary) a bounded, redacted
-- summary of the arguments and the result. Retention is the run's (owner decision
-- 2026-09-23): rows go when a run's record goes, and nothing prunes runs
-- yet. Lives in the tenant's DuckDB only — observability data whose durable
-- record is the OTLP trace (P3-3), so an index rebuild dropping it is the
-- accepted trade.
CREATE TABLE IF NOT EXISTS run_tool_calls (
    seq             BIGINT PRIMARY KEY,                -- ingestion order, the cursor
    run_id          VARCHAR NOT NULL,
    root_event_id   VARCHAR,
    tool            VARCHAR NOT NULL,
    status          VARCHAR NOT NULL,                  -- 'ok' | 'error'
    error_code      VARCHAR,                           -- the JSON-RPC data.code, when any
    duration_ms     DOUBLE NOT NULL,
    request_bytes   BIGINT NOT NULL DEFAULT 0,
    response_bytes  BIGINT NOT NULL DEFAULT 0,
    subject         VARCHAR NOT NULL DEFAULT '',
    at_ts           TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS run_tool_calls_run ON run_tool_calls (run_id, seq);
-- A bounded, redacted summary of what was asked and what came back (NULL when the
-- gateway runs with ESCUREL_TOOLCALL_DETAIL=off, and for rows recorded before it existed).
-- Plain `ADD COLUMN IF NOT EXISTS`, no default: safe on every open.
ALTER TABLE run_tool_calls ADD COLUMN IF NOT EXISTS args_summary VARCHAR;
ALTER TABLE run_tool_calls ADD COLUMN IF NOT EXISTS result_summary VARCHAR;
