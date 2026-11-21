-- The SQL search lane's local projection (#849): one row per distinct
-- `concat_ws(chr(31), <search_text columns>)` of a sql_view instance's view,
-- with the number of source rows carrying it. `sig` keys the binding hash plus
-- the searched columns, so a projection captured for other columns is never
-- read. A DERIVED cache: rebuilt from the source by
-- `Indexer::refresh_search_projections`, never published to the lake.
CREATE TABLE IF NOT EXISTS sql_view_search_text (
    view VARCHAR NOT NULL,
    sig  VARCHAR NOT NULL,
    text VARCHAR NOT NULL,
    n    BIGINT  NOT NULL
);
CREATE INDEX IF NOT EXISTS sql_view_search_text_view_sig ON sql_view_search_text (view, sig);
