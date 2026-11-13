-- `run_transcripts`: when each run's transcript was stored (#822), for the
-- retention sweep, and its digest, for the write-once check. The bytes are
-- KEY-addressed at `transcripts/<run_id>.jsonl` in the lane store, never in
-- the content-addressed `blobs/` namespace, so retention can never delete a
-- document that happens to share a transcript's bytes. `stored_at` is when
-- the runner uploaded it, right after the harness exited, so it is the run's
-- end for the retention cutoff. Lives in the tenant's DuckDB only, like
-- `run_tool_calls`: an index rebuild forgets the rows, and the sweep adopts
-- the keys it finds without one (`sha256` empty) as stored at adoption.
CREATE TABLE IF NOT EXISTS run_transcripts (
    run_id     VARCHAR PRIMARY KEY,
    sha256     VARCHAR NOT NULL,
    records    BIGINT NOT NULL,
    bytes      BIGINT NOT NULL,
    stored_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
