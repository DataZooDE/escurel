-- Events: the global inbox / event store (M7 — Event-sourcing surface).
--
-- An event is the *dynamic* input — what happened. It is tightly bound
-- to the page model without being a page itself:
--   - `label_skill`      → the SKILL that knows how to process this event
--                          type (the durable "how"; e.g. `gmail`, `meet`).
--   - `instance_page_id` → the INSTANCE the event belongs to once an
--                          (external) agent has processed it. NULL while
--                          the event is still in the inbox.
-- `status` is `'inbox'` (unprocessed) or `'processed'`. The inbox is just
-- the `status = 'inbox'` view; an event may be pre-flagged with a
-- candidate `instance_page_id` (Gmail-label style) and still be in the
-- inbox until an agent assigns/processes it.
--
-- Events are NOT in the page `links` graph (they are not pages); their
-- own surface is `list_inbox` / `list_events(instance)` / `assign_event`.
CREATE TABLE events (
    event_id          VARCHAR PRIMARY KEY,
    at_ts             TIMESTAMP,                       -- event time (mirrored from the RFC 3339 input; `at` is a DuckDB keyword)
    source            VARCHAR NOT NULL DEFAULT '',     -- ingest source, e.g. gmail / meet / drive
    mime              VARCHAR NOT NULL DEFAULT '',     -- content type, e.g. message/rfc822
    label_skill       VARCHAR NOT NULL DEFAULT '',     -- skill id: how to process this event type
    instance_page_id  VARCHAR,                         -- assigned instance (NULL = still in inbox)
    status            VARCHAR NOT NULL DEFAULT 'inbox',-- 'inbox' | 'processed'
    title             VARCHAR NOT NULL DEFAULT '',
    body              VARCHAR NOT NULL DEFAULT '',
    provenance        JSON,
    created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- 'user' (work) | 'system' (bookkeeping about a run; skips the inbox).
    -- See 0016_events_lineage.sql, which adds these three to an existing file.
    kind              VARCHAR DEFAULT 'user',
    root_event_id     VARCHAR,                         -- lineage root (a user event is its own)
    run_id            VARCHAR,                         -- the run this system event belongs to
    -- Ingestion position (see 0018_events_seq.sql): label / lineage / run
    -- listings page by it, so a backdated event still follows a cursor.
    seq               BIGINT
);

CREATE INDEX events_status_at   ON events (status, at_ts);
CREATE INDEX events_instance_at ON events (instance_page_id, at_ts);
CREATE INDEX events_root_at     ON events (root_event_id, at_ts);
CREATE INDEX events_run_at      ON events (run_id, at_ts);
CREATE INDEX events_seq         ON events (seq);
