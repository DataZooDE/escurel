-- The embedding space the index's vectors were computed in: one row, the
-- embedder's model identity and dimension. Vectors from two models are not
-- comparable, so an index whose recorded space differs from the live
-- embedder's is re-embedded rather than mixed (`Indexer::reconcile_embedding_space`).
CREATE TABLE IF NOT EXISTS embedding_space (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    model_id  VARCHAR NOT NULL,
    dim       INTEGER NOT NULL
);

-- A re-embed's staging area: vectors for the TARGET model, committed per
-- batch and swapped into `blocks` in one transaction only when every block
-- has one. A failure half way leaves the old vectors serving and these rows
-- for the next attempt to resume from (`text_md5` drops rows whose text
-- changed since).
CREATE TABLE IF NOT EXISTS embedding_reembed (
    block_id  VARCHAR PRIMARY KEY,
    model_id  VARCHAR NOT NULL,
    text_md5  VARCHAR NOT NULL,
    dense_vec FLOAT[768] NOT NULL
);
