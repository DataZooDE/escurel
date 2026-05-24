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
    dst_anchor   VARCHAR,
    link_skill   VARCHAR NOT NULL,
    link_version VARCHAR,
    PRIMARY KEY (src_page, src_anchor, dst_page, link_skill)
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

-- HNSW vector index (vss extension; auto-loaded on this reference).
CREATE INDEX hnsw_blocks_vec
    ON blocks USING HNSW (dense_vec)
    WITH (metric = 'cosine', ef_construction = 128, ef_search = 64, M = 16);

-- Frontmatter index: flattened key/value for skill-specific filters
-- (status, tier, risk, …) without a schema migration per skill.
CREATE TABLE frontmatter_index (
    page_id  VARCHAR NOT NULL,
    key      VARCHAR NOT NULL,
    value    JSON NOT NULL,
    value_ts TIMESTAMP,
    PRIMARY KEY (page_id, key)
);
CREATE INDEX fm_key_value ON frontmatter_index (key, value_ts);

-- CRDT op log.
CREATE TABLE crdt_ops (
    page_id      VARCHAR NOT NULL,
    op_id        VARCHAR NOT NULL,
    hlc          BIGINT  NOT NULL,
    parent_op_id VARCHAR,
    op_bytes     BLOB    NOT NULL,
    applied_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
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
