-- Contextual Retrieval, Variant A (GH #216): the structural situating
-- prefix ("[<title> › <heading path> › p.<page>]") for a document chunk.
-- `blocks.body` stays the VERBATIM chunk text (display + provenance);
-- `context` is concatenated with it only at embed / FTS-index / rerank
-- time. NULL for ordinary page blocks and for `contextualize = off`.
--
-- Idempotent (ADD COLUMN IF NOT EXISTS) and run on EVERY connection via
-- `Migrator::ensure_block_context`, so a tenant DB provisioned before
-- this column existed gains it on the next boot.
ALTER TABLE blocks ADD COLUMN IF NOT EXISTS context VARCHAR;
