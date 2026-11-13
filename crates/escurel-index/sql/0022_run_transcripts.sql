-- `run_transcripts`: which blob holds which run's transcript (#822), so the
-- retention sweep deletes transcript blobs and never a document's. The blob is
-- content-addressed (`put_blob`); the run-finished event names it for readers.
-- `stored_at` is when the runner uploaded it — right after the harness exited,
-- so it is the run's end for the retention cutoff. Lives in the tenant's DuckDB
-- only, like `run_tool_calls`: an index rebuild forgets the rows (the blobs then
-- outlive the retention window until removed by hand — the accepted trade).
CREATE TABLE IF NOT EXISTS run_transcripts (
    run_id     VARCHAR PRIMARY KEY,
    blob_id    VARCHAR NOT NULL,
    records    BIGINT NOT NULL,
    bytes      BIGINT NOT NULL,
    stored_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS run_transcripts_blob ON run_transcripts (blob_id);
