-- Stage 4 of the v1 schema migration: the `chat_messages` table
-- introduced for per-chat-group conversation history (DataZooDE/
-- escurel#63 — M-Chat). Unlike `pages`/`blocks`, this is an
-- append-mostly log; each row is one message. Rows may carry an
-- embedding (`dense_vec` populated, `embedded = TRUE`) or skip it
-- (`dense_vec = NULL`, `embedded = FALSE`) so high-volume sources
-- can opt out of the embedding cost (see
-- docs/notes/discovered/2026-05-25-vss-hnsw-tolerates-null-rows.md).
--
-- The HNSW index covers the embedded subset; similarity queries
-- filter with `WHERE dense_vec IS NOT NULL`. The probe confirms
-- vss tolerates NULL rows in the indexed column.

CREATE TABLE chat_messages (
    chat_group_id  VARCHAR    NOT NULL,
    msg_id         VARCHAR    NOT NULL,            -- caller-supplied or server ULID
    ts             TIMESTAMP  NOT NULL,            -- event time
    role           VARCHAR    NOT NULL,            -- 'user'|'assistant'|'system'|'tool'
    author         VARCHAR,                        -- opaque handle, optional
    content        VARCHAR    NOT NULL,
    metadata       JSON,
    dense_vec      FLOAT[768],                     -- NULL when embed = false
    embedded       BOOLEAN    NOT NULL DEFAULT TRUE,
    created_at     TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (chat_group_id, ts, msg_id)
);
CREATE INDEX chat_group_ts ON chat_messages (chat_group_id, ts DESC);
CREATE INDEX chat_msg_id   ON chat_messages (msg_id);

-- HNSW on the embedded subset. The vss extension accepts NULL rows
-- (probed 2026-05-25); similarity queries must filter
-- `WHERE dense_vec IS NOT NULL` so the non-embedded rows don't leak
-- into the result.
CREATE INDEX hnsw_chat_vec
    ON chat_messages USING HNSW (dense_vec)
    WITH (metric = 'cosine', ef_construction = 128, ef_search = 64, M = 16);
