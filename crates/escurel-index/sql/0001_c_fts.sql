-- Stage 3 of the v1 schema migration: the BM25 FTS index on
-- blocks.body. Runs in its own batch so the fts extension can see
-- the `blocks` table from stage 2 in its catalog lookup.
--
-- Index is built on the empty `blocks` table; subsequent inserts
-- need `PRAGMA refresh_fts_index('blocks')` from the indexer.
PRAGMA create_fts_index(
    'blocks',
    'block_id',
    'body',
    stemmer   = 'porter',
    stopwords = 'english',
    ignore    = '(\.|[^a-z])+',
    lower     = 1
);
