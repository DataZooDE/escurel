# Storage — single DuckDB store, CRDT persistence, audit/rebuild

The storage layer carries three concerns:

1. **Canonical markdown** — the source of truth; everything
   else is derivable from it.
2. **The single per-tenant DuckDB store** —
   relational metadata + the typed link graph + block-level
   retrieval (HNSW via the `vss` extension, BM25 via the
   `fts` extension on the same `blocks` table) + the Loro
   CRDT op log + the frontmatter index.
3. **External attached data** — read-only DuckLake catalogs
   mounted as `external.ducklake` for the origin axis, via
   DuckDB's `ATTACH` mechanism.

All three sit behind one trait, `LaneStore`, so the
filesystem-backed default and the S3-backed variant share the
upper layers verbatim. The Rust port preserves the 28 end-to-end
assertions and the spike outcomes T1–T6 from the Python prototype
as the acceptance baseline for indexer behaviour.

## Per-tenant directory layout

```
${ESCUREL_DATA_DIR}/tenants/<tenant_id>/
├── manifest.toml              # tenant metadata, quotas, embedding provider
├── markdown/                  # canonical source
│   ├── skills/
│   │   ├── customer.md
│   │   ├── meeting.md         # event-typed skill
│   │   └── escurel.md         # the mandatory meta-skill
│   └── instances/
│       ├── customer/
│       │   └── acme-corp.md
│       └── meeting/           # event instances live here, not in a separate "events/"
│           └── 2026-04-12-acme-qbr.md
├── escurel.duckdb             # single DuckDB file: pages, links, blocks
│                              # (with vss + fts indexes), crdt_ops,
│                              # crdt_snapshots, frontmatter_index
├── external/                  # DuckLake / Iceberg / Delta catalog mount-points
│   └── ducklake.config        # ATTACH parameters per attached catalog
└── cache/
    ├── embeddings/            # warm cache for re-embed parallel
    └── compacted/             # staging for `compact_db`
```

This layout is *the export format*: `tenant_export` produces a
deterministic tarball of the above directory minus `cache/`.
`tenant_import` is the inverse.

Under the S3 LaneStore the same tree maps to S3 keys via:

```
s3://<bucket>/<prefix>/tenants/<tenant_id>/
├── manifest.toml
├── markdown/skills/customer.md
├── markdown/instances/customer/acme-corp.md
├── escurel.duckdb
└── external/ducklake.config
```

The `cache/` subtree is per-node and never lives on S3 (model
weights cache and embedding-recompute staging are local).
Spool (`spool/<tenant_id>/`) is similarly local and never
synced — see the S3-backend-unavailable row in the
crash-recovery summary.

## The `LaneStore` trait

```rust
#[async_trait]
pub trait LaneStore: Send + Sync + 'static {
    /// Plain byte read; used for markdown files and small artefacts.
    async fn read(&self, key: &Key) -> Result<Bytes>;

    /// Atomic write-then-publish. Returns the new content version.
    async fn write(&self, key: &Key, body: Bytes) -> Result<Version>;

    /// Open a long-lived handle for streaming workloads.
    /// For FS: returns a `tokio::fs::File`. For S3: returns a multipart
    /// writer that flushes on drop.
    async fn open_writer(&self, key: &Key) -> Result<Box<dyn AsyncWrite + Unpin + Send>>;

    /// Enumerate keys under a prefix. Used by audit + tenant_export.
    async fn list(&self, prefix: &Key) -> Result<Vec<Key>>;

    /// Used by `compact_db` and tenant_delete.
    async fn delete(&self, key: &Key) -> Result<()>;

    /// Object-store URL form, suitable for handing to DuckDB
    /// (`httpfs`) without copying through this process.
    fn url(&self, key: &Key) -> Url;
}
```

`Key` is a tenant-scoped relative path
(`Key::new(tenant_id, "markdown/skills/customer.md")`); the
implementation maps it to a filesystem path or an S3 object
key.

Two implementations ship in v1:

- **`FsStore`**. `${ESCUREL_DATA_DIR}/tenants/<tenant>/<rest>`.
  Writes go to `<rest>.tmp` and `rename(2)` to publish (atomic
  on POSIX same-filesystem). `url()` returns `file://...`.
- **`S3Store`**. Backed by `object_store::aws`. Keys are
  S3 paths under a configured prefix. `url()` returns `s3://...`,
  consumed by DuckDB's `httpfs` extension when reading or by
  DuckLake when an external catalog is attached. Writes use
  multipart upload; on drop with no `commit()` the multipart
  is aborted. The S3 backend assumes only basic S3 semantics
  (GET/PUT/DELETE/list/multipart-upload, path-style addressing,
  no STS, no presigned URLs, no AWS-S3-Tables-or-Vectors
  specific APIs). Verified backends: AWS S3, MinIO, Hetzner
  Object Storage. **Critical**: the S3 endpoint hostname
  returned by `S3Store::url()` must equal the hostname the
  LaneStore is configured against — DuckDB `httpfs` honours
  the secret's `ENDPOINT` field literally, so a hostname
  mismatch between the LaneStore config and the DuckDB
  ATTACH/secret produces silent unsigned PUTs and 403s on
  writes. On cattle nodes, `/etc/hosts` rewrites are not an
  acceptable workaround; configure the LaneStore against the
  object-store hostname directly.

DuckDB happily operates against an object-store URL via
`httpfs`; we do not need to round-trip data through the
server process. The S3 backend is gated behind the `s3` Cargo
feature.

## Indexer

The indexer is the Rust port of the Python prototype's
~430-LOC script. It has two entry points — `update_page`
(steady state) and `rebuild` (recovery).

### `update_page(tenant, page_id, content)`

Per-tenant write lock held throughout. Steps:

1. **Parse markdown.** Frontmatter, body, blocks (`^blk-...`
   anchors auto-synthesised if absent).
2. **Parse wikilinks** using the regex-plus-code-region-stripping
   parser (do **not** use a markdown AST library — they
   fragment text on `[`). Wikilinks are extracted from the **body**
   *and* from **frontmatter field values** (e.g. `about:`,
   `derived_from:`, `primary_sponsor:`); a frontmatter value that
   YAML parses as a nested flow sequence (`about: [[skill::id]]`) is
   rendered back to its raw `[[…]]` markup before parsing. Frontmatter
   links carry their originating field in `links.src_field`
   (`frontmatter.<key>`); body links leave it `NULL`. This makes a
   relationship an instance declares only in frontmatter (e.g. an
   event whose `about:` points at its entity) reachable via
   `neighbours`.
3. **Validate** (the four index-time checks plus
   required-frontmatter check; events get the extra check that
   `at:` parses as RFC 3339).
4. **Embed** new/changed blocks (candle EmbeddingGemma).
5. **One DuckDB transaction.** Upsert into `pages`; delete +
   insert into `links` (with `link_skill` and `link_version`
   populated); delete + insert into `blocks` for changed
   blocks (populating `dense_vec`, `body`, denormalised
   `skill`/`page_type`/`at_ts`); insert any new `crdt_ops`
   rows from a live session that closed in this call;
   refresh the `vss` HNSW index for the affected rows
   (`PRAGMA hnsw_compact_index` or per-row update — see
   "VSS index maintenance" below); refresh the `fts` index
   (`PRAGMA refresh_fts_index('blocks')` per the FTS
   extension semantics). Commit.
6. **Publish** the markdown file via `LaneStore::write` —
   write-then-rename, so the markdown file appears only after
   the DuckDB commit succeeds. On commit failure the markdown
   stays at the previous version.
7. **Emit issues** (warnings) and return.

### `rebuild(tenant, scope=None)`

Per-tenant write lock held throughout. Drops the affected
rows for `scope` (one page, one skill's instances or the
whole tenant) inside a DuckDB transaction and runs
`update_page` for every markdown file in canonical order.
Cost on the prototype was approximately 32 ms per page; the
single-store path is at least as fast because the write is
one DuckDB transaction. Streams progress events to the admin
client.

### `audit(tenant, scope=None)`

Read-only. Per-tenant *read* lock; concurrent reads are fine.
Compares two sets: markdown files on disk and page rows in
DuckDB. Returns the two asymmetric differences as the `audit`
admin response.

## DuckDB schema

```sql
-- Pages: one row per markdown file.
CREATE TABLE pages (
  page_id        VARCHAR PRIMARY KEY,    -- ULID
  slug           VARCHAR,                 -- mutable, indexed but not unique
  skill          VARCHAR NOT NULL,
  page_type      VARCHAR NOT NULL,       -- 'skill' | 'instance'
  frontmatter    JSON NOT NULL,
  body_hash      VARCHAR NOT NULL,       -- for audit
  at_ts          TIMESTAMP,              -- mirrored from frontmatter.at (NULL for non-events)
  created_at     TIMESTAMP NOT NULL,
  updated_at     TIMESTAMP NOT NULL
);
CREATE INDEX pages_slug      ON pages(slug);
CREATE INDEX pages_skill     ON pages(skill);
CREATE INDEX pages_skill_at  ON pages(skill, at_ts);   -- event-log scan support

-- Links: one row per wikilink occurrence.
CREATE TABLE links (
  src_page     VARCHAR NOT NULL,
  src_anchor   VARCHAR,
  src_field    VARCHAR,                  -- `frontmatter.<key>` if the link came from a frontmatter value; NULL for body links
  dst_page     VARCHAR NOT NULL,
  dst_anchor   VARCHAR,
  link_skill   VARCHAR NOT NULL,         -- the skill segment of the typed link
  link_version VARCHAR,                  -- the @version segment (NULL if unpinned)
  PRIMARY KEY (src_page, src_anchor, dst_page, link_skill)
);
CREATE INDEX links_dst_skill ON links(dst_page, link_skill);   -- backlinks
CREATE INDEX links_src_skill ON links(src_page, link_skill);   -- forward links

-- Blocks: one row per markdown block.
CREATE TABLE blocks (
  block_id    VARCHAR PRIMARY KEY,        -- "<page_id>:<anchor>"
  page_id     VARCHAR NOT NULL,
  anchor      VARCHAR,
  ordinal     INT,
  body        VARCHAR NOT NULL,           -- the block's markdown text
  dense_vec   FLOAT[768],                 -- EmbeddingGemma default; vss HNSW-indexed
  -- denormalised for filtered retrieval (single-SQL push-down):
  skill       VARCHAR,
  page_type   VARCHAR,
  at_ts       TIMESTAMP                   -- mirrored from frontmatter.at if present
);
CREATE INDEX blocks_page    ON blocks(page_id);
CREATE INDEX blocks_skill   ON blocks(skill);
CREATE INDEX blocks_at      ON blocks(at_ts);

-- vss + fts indexes are created via extension DDL after the table:
INSTALL vss; LOAD vss;
CREATE INDEX hnsw_blocks_vec ON blocks USING HNSW (dense_vec)
  WITH (metric = 'cosine', ef_construction = 128, ef_search = 64, M = 16);
INSTALL fts; LOAD fts;
PRAGMA create_fts_index('blocks', 'block_id', 'body',
                        stemmer = 'porter', stopwords = 'english',
                        ignore = '\.|[^a-z]', lower = 1);

-- Frontmatter index: flattened key/value for filtering.
CREATE TABLE frontmatter_index (
  page_id  VARCHAR NOT NULL,
  key      VARCHAR NOT NULL,
  value    JSON NOT NULL,                 -- typed value (string/number/bool/array)
  value_ts TIMESTAMP,                     -- populated when the key looks like a date and value parses
  PRIMARY KEY (page_id, key)
);
CREATE INDEX fm_key_value ON frontmatter_index(key, value_ts);
```

The `at_ts` column on `pages` plus the `pages_skill_at`
composite index is the event-log scan support:
`list_instances('meeting', filter={at: {">=": "2026-04-01"}},
order_by='at desc')` becomes `SELECT * FROM pages WHERE
skill='meeting' AND at_ts >= '2026-04-01' ORDER BY at_ts DESC
LIMIT 50` — index-served, sub-millisecond even at 100 k event
instances per skill.

The denormalised `skill`, `page_type` and `at_ts` columns on
`blocks` make filtered vector search a single SQL statement:
a query of the form "return the top-10 closest blocks to vector V
whose skill is `meeting` and whose `at_ts` is at least 2026-04-01"
expresses as one SQL with a `vss_search()` call against `dense_vec`
joined with the relational predicates.

The `frontmatter_index` table catches every other filterable
frontmatter key (`status`, `tier`, `risk`, etc.) without
requiring a schema migration when a new skill adds a new
field.

## CRDT persistence

The Loro engine — the in-memory adapter and `LiveDoc` actor —
persists into two DuckDB tables that share the per-tenant store
and the per-write transaction:

```sql
CREATE TABLE crdt_ops (
  page_id       VARCHAR NOT NULL,
  op_id         VARCHAR NOT NULL,        -- Loro op id (HLC-ordered)
  hlc           BIGINT NOT NULL,         -- the HLC value for monotonic sort
  parent_op_id  VARCHAR,                 -- chain parent (NULL for genesis)
  op_bytes      BLOB NOT NULL,           -- raw Loro op
  applied_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (page_id, op_id)
);
CREATE INDEX crdt_ops_page_hlc ON crdt_ops(page_id, hlc);

CREATE TABLE crdt_snapshots (
  page_id        VARCHAR NOT NULL,
  snapshot_hlc   BIGINT NOT NULL,        -- the HLC at which the snapshot was taken
  snapshot_bytes BLOB NOT NULL,          -- raw Loro export_snapshot
  taken_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (page_id, snapshot_hlc)
);

-- events: the global inbox / event store (M7 — Event-sourcing surface).
-- An event is the dynamic input; `label_skill` links to the SKILL that
-- knows how to process it, `instance_page_id` to the INSTANCE it belongs
-- to once an (external) agent has processed it. `status='inbox'` until
-- assigned. Events are NOT pages and are not in the `links` graph; their
-- surface is capture_event / list_inbox / list_events / assign_event.
CREATE TABLE events (
  event_id          VARCHAR PRIMARY KEY,
  at_ts             TIMESTAMP,                        -- event time (`at` is a DuckDB keyword)
  source            VARCHAR NOT NULL DEFAULT '',      -- ingest source, e.g. gmail / meet
  mime              VARCHAR NOT NULL DEFAULT '',      -- content type, e.g. message/rfc822
  label_skill       VARCHAR NOT NULL DEFAULT '',      -- skill id: how to process this event type
  instance_page_id  VARCHAR,                          -- assigned instance (NULL = inbox)
  status            VARCHAR NOT NULL DEFAULT 'inbox', -- 'inbox' | 'processed'
  title             VARCHAR NOT NULL DEFAULT '',
  body              VARCHAR NOT NULL DEFAULT '',
  provenance        JSON,
  created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX events_status_at   ON events(status, at_ts);          -- the inbox view
CREATE INDEX events_instance_at ON events(instance_page_id, at_ts);-- an instance's event history
```

The `LiveDoc` actor for a page is one Tokio task that accepts
ops from the open session(s), feeds them to the in-memory
Loro engine and on each accepted op runs a write transaction
that inserts the op row. The transaction is small (one row,
no index rebuild) and commits in under a millisecond on the
local FS backend.

Snapshots are taken on session close (`commit=true`) and
periodically during long-lived sessions (default every 256
ops or 60 s). After a snapshot inserts into `crdt_snapshots`,
the ops with `hlc <= snapshot.snapshot_hlc` for the same page
are eligible for compaction (see "Compaction" below).

**Query-time historical state (M7).** Beyond crash recovery,
`crdt_snapshots` is also a read-time API: `expand(page_id,
as_of = T)` loads the snapshot with the greatest `taken_at <=
T`, materializes its `"body"` text container back to markdown
(`escurel_crdt::body_from_snapshot`), and re-parses it —
returning the instance's frontmatter+body **as it was at T**
(the projection of its events up to T). A page with no snapshot
at-or-before T falls through to the current-state path (the
`at_ts` birth filter), so this is additive. Snapshot histories
can be authored server-side via `Indexer::seed_snapshot_history`
(deterministic Loro export, `escurel_crdt::snapshot_bytes_from_markdown`)
— how the demo gives an instance a real state-over-time history.

On server restart, opening a page that has CRDT rows replays
the latest `crdt_snapshots` row for the page, then any
`crdt_ops` rows with `hlc > snapshot_hlc`, yielding the exact
state at crash. If the markdown file on disk is *newer* than
the snapshot's reflected state (external edit happened), the
two-stage reconciler runs: snapshot wins for cited instances;
external edit wins for new pages and uncited pages.

CRDT state is *not* part of `tenant_export` — the canonical
markdown is. On import into a different server, all live
sessions reset to the markdown head. This is by design: CRDT
state is a runtime concern, not a corpus artefact.

## Event volume — scaling beyond ~1 M events

The single DuckDB store clears 100 k instances at typed-backlink
5.21 ms p95 and extrapolates to 1 M at ~13 ms p95 for the
backlink path. The vector + skill-filter path is targeted at
≤ 50 ms p95 at 100 k, pending the pre-deployment gate in
[`../adr/0001-duckdb-only-storage.md`](../adr/0001-duckdb-only-storage.md).
Above ~1 M events — multi-million-event corpora such as a
chat-message firehose or a CDC stream of CRM activity — the
recommended path is:

1. Author **one** `[[table::messages]]` skill instance whose
   body describes the schema and the `catalog`/`schema`/`name`
   fields.
2. Land the events in an external DuckLake (or Iceberg) table.
3. Attach the catalog read-only via `attach_external`.
4. Author one or more `[[query::*]]` instances that wrap
   typical timeline queries (`messages-for-customer`,
   `recent-incidents-affecting-project`).
5. Agents reach individual events through `run_stored_query`
   rather than through `list_instances`.

The origin-axis path is the volume escape hatch for events.
Markdown is reserved for the events that have curation or
authoring value (meetings, key decisions, postmortems); bulk
event streams stay where they came from. Both look identical
to the agent — the dispatcher hides the lane.

## VSS index maintenance

The HNSW index on `blocks.dense_vec` is built with `INSTALL
vss; LOAD vss;` plus `CREATE INDEX … USING HNSW` after the
table is populated. The extension's current update semantics
expect the writer to refresh the index after batched
modifications; the indexer issues `PRAGMA hnsw_compact_index`
at the end of every write transaction that touched `blocks`.
Cost characterisation is the load-bearing item of
[`../adr/0001-duckdb-only-storage.md`](../adr/0001-duckdb-only-storage.md)'s
pre-deployment gate.

### HNSW persistence model

The HNSW index lives inside the DuckDB file alongside the
`blocks` table. Opening an intact DuckDB file via
`DuckDB.Open()` loads the existing index as-is; the index is
**not** reconstructed per open. After a crash mid-write the
DuckDB transaction is rolled back (no partial index state),
so on the next open the index is consistent with the
committed `blocks` rows without further work.

If the DuckDB file is missing but the canonical markdown is
intact on the LaneStore — the cattle-node-loss case, where
a Nomad client was destroyed and the next allocation lands on
a fresh host with no local state — the server detects the
missing file on first tenant access and runs `rebuild(tenant)`
automatically before serving the first request. Cost ~32 ms/page:
a 1000-page tenant rebuilds in ~32 s, a 10 000-page
tenant in ~5 min. Transparent to agent callers except for the
first-request latency on that tenant.

This recovery property is what makes the per-tenant
`escurel.duckdb` file *cattle* rather than *pet*: canonical
markdown on the LaneStore is the source of truth, the DuckDB
file (including the HNSW and FTS indexes) is a rebuildable
derivative.

## Compaction

DuckDB compaction is implicit (`CHECKPOINT` runs after the
write transaction completes); file rewrites happen during the
regular write path. A `compact_db` admin endpoint forces a
`CHECKPOINT` plus a `VACUUM` plus a `PRAGMA
hnsw_compact_index` for any tenant whose store size grows
above a configurable watermark. The `crdt_ops` table also
benefits from periodic truncation: ops older than the most
recent `crdt_snapshots` row for the same page are eligible
for deletion, controlled by `crdt.ops.retain_post_snapshot`
(default keep the most recent 1024 ops per page for one-step
replay margin).

For S3 backends, compaction also coalesces many small
DuckDB checkpoint files into fewer larger ones (vendored
behaviour).

## Crash recovery summary

| failure | recovery |
|---|---|
| Process killed mid-write | DuckDB rolls back the transaction; pages, links, blocks (with vss/fts updates), crdt_ops all revert together; markdown file is left at the previous version because the rename happens only after commit |
| Process killed after DuckDB commit, before markdown rename | audit reports `markdown_not_in_duckdb` for the new page (DuckDB has it, markdown does not yet); rebuild reconciles by re-writing the markdown from the DuckDB row, or admin can re-run `update_page` from a re-submitted source |
| Markdown rename(2) crashed mid-write | The pre-existing markdown is intact (`.tmp` orphans cleaned on startup) |
| External edit mid-session (live mode) | Two-stage reconciler: for cited pages the CRDT snapshot wins; for new or uncited pages the external edit wins |
| DuckDB file corruption (rare) | Auto-suspend tenant (`status: suspended_corrupt`); admin runs `rebuild --tenant <id>` to recreate from canonical markdown |
| `vss` or `fts` index corruption | `PRAGMA drop_index` plus rebuild — the index is derivable from `blocks.dense_vec` and `blocks.body` without re-embedding |
| S3 backend timeout | Local spool under `${ESCUREL_DATA_DIR}/spool/<tenant>/` — **host-local, not synced to the LaneStore**; queue flushes on reconnect. On Nomad reschedule to a new host the previous host's spool is lost; the markdown source-of-truth is preserved (writes only enter the spool after a successful DuckDB commit per the row above), so recovery is a client re-submit |
| Cattle node destroyed; `escurel.duckdb` gone; markdown intact on LaneStore | First request to the tenant triggers automatic `rebuild` from canonical markdown on the LaneStore (~32 ms/page; ~32 s for 1000 pages); transparent to agent except for one-time first-request latency |

The two recovery primitives (`audit`, `rebuild`) are the full
playbook. Operators do not need to know the internal storage
mechanics to recover, and the playbook is bounded because there
is one index to reconcile.
