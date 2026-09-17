-- Stage 2 of the v1 schema migration: the six core tables plus
-- their B-tree indexes and the HNSW vector index on blocks.
-- Source: docs/spec/storage.md §DuckDB schema.

-- Pages: one row per markdown file.
CREATE TABLE pages (
    page_id     VARCHAR PRIMARY KEY,
    slug        VARCHAR,
    skill       VARCHAR NOT NULL,
    page_type   VARCHAR NOT NULL,           -- 'skill' | 'instance'
    frontmatter JSON NOT NULL,
    body_hash   VARCHAR NOT NULL,
    at_ts       TIMESTAMP,                  -- mirrored from frontmatter.at
    -- Server-stamped principal of the LAST write (#357/CR-6). NULLable;
    -- `sql/0011_write_attribution.sql` adds it to databases created before
    -- it existed and explains why NULL is the only honest default. Keep the
    -- two in step.
    last_written_by VARCHAR,
    created_at  TIMESTAMP NOT NULL,
    updated_at  TIMESTAMP NOT NULL
);
CREATE INDEX pages_slug     ON pages (slug);
CREATE INDEX pages_skill    ON pages (skill);
CREATE INDEX pages_skill_at ON pages (skill, at_ts);  -- event-log scan

-- Links: one row per wikilink occurrence.
CREATE TABLE links (
    src_page     VARCHAR NOT NULL,
    src_anchor   VARCHAR,
    src_field    VARCHAR,
    dst_page     VARCHAR NOT NULL,
    -- dst_anchor must NOT be NULL inside the PK (DuckDB primary keys
    -- forbid NULL columns), so we substitute the empty string for
    -- "no anchor" at write time; readers project '' back to NULL.
    -- See docs/notes/discovered/2026-05-24-links-pk-includes-anchor.md.
    dst_anchor   VARCHAR NOT NULL DEFAULT '',
    link_skill   VARCHAR NOT NULL,
    link_version VARCHAR,
    PRIMARY KEY (src_page, src_anchor, dst_page, dst_anchor, link_skill)
);
CREATE INDEX links_dst_skill ON links (dst_page, link_skill);
CREATE INDEX links_src_skill ON links (src_page, link_skill);

-- Blocks: hot path for retrieval.
CREATE TABLE blocks (
    block_id   VARCHAR PRIMARY KEY,        -- "<page_id>:<anchor>"
    page_id    VARCHAR NOT NULL,
    anchor     VARCHAR,
    ordinal    INTEGER,
    body       VARCHAR NOT NULL,
    dense_vec  FLOAT[768],                 -- EmbeddingGemma default dim
    skill      VARCHAR,
    page_type  VARCHAR,
    at_ts      TIMESTAMP
);
CREATE INDEX blocks_page  ON blocks (page_id);
CREATE INDEX blocks_skill ON blocks (skill);
CREATE INDEX blocks_at    ON blocks (at_ts);

-- No HNSW vector index here any more. `vss` corrupts itself under the
-- delete+insert cycle every page write performs: the index hangs the writer
-- after ~192 of them and segfaults if it is rebuilt (#431, and
-- docs/notes/discovered/2026-09-11-vss-hnsw-churn-hangs-then-segfaults.md).
-- It also bought nothing measurable — at 10k blocks 29.6ms vs 30.6ms, at 100k
-- ~0.29s either way, because every search carries a filter. Vector search is
-- an exact `array_cosine_distance` scan (see `search.rs`). `Migrator::
-- ensure_vector_index` rebuilds the index on boot when ESCUREL_INDEX_HNSW is
-- set, for the day vss is fixed.

-- CRDT op log.
CREATE TABLE crdt_ops (
    page_id      VARCHAR NOT NULL,
    op_id        VARCHAR NOT NULL,
    hlc          BIGINT  NOT NULL,
    parent_op_id VARCHAR,
    op_bytes     BLOB    NOT NULL,
    applied_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    -- Server-stamped author of THIS op (#357/CR-6) — the verified caller,
    -- NOT the Loro peer id inside `op_bytes` (that names a device). Declared
    -- here as well as in `sql/0011_write_attribution.sql` so a FRESH database
    -- needs no ALTER at all: `applied_at`'s function-valued DEFAULT makes an
    -- ALTER on this table unreplayable from the WAL until a CHECKPOINT (see
    -- `Migrator::ensure_write_attribution`).
    principal    VARCHAR,
    PRIMARY KEY (page_id, op_id)
);
CREATE INDEX crdt_ops_page_hlc ON crdt_ops (page_id, hlc);

-- CRDT snapshots.
CREATE TABLE crdt_snapshots (
    page_id        VARCHAR NOT NULL,
    snapshot_hlc   BIGINT  NOT NULL,
    snapshot_bytes BLOB    NOT NULL,
    taken_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (page_id, snapshot_hlc)
);
