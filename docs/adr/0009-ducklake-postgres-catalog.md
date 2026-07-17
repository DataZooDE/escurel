# ADR-0009 — DuckLake derived-index backend with a live Postgres catalog

**Status:** Proposed, 2026-07-17.
**Scope:** escurel's derived-index storage layer (`escurel-index` + server boot).
The markdown LaneStore stays the source of truth; the external contract
(MCP-over-HTTP + WS on `:8080`, JWT, markdown-as-truth) is unchanged.
**Relation to substrate ADR-0015:** rev.2 of the substrate ADR chose a
*published-catalog-file* design (DuckDB catalog snapshot copied to object
storage) specifically to avoid running a database server. This ADR adopts the
**Postgres-catalog variant** (rev.1 direction) instead, deliberately: a Cloud
SQL instance (`ducklake-metadata`, project `hetzner-agent-backplane`) now
exists, and a live multi-client catalog removes the publish/pointer/copy
machinery entirely. The substrate ADR needs a rev.3 amendment recording this
(follow-up, substrate repo).

## Question

The derived DuckDB index (vss/HNSW over 768-dim embeddings) is operated as a
durable single-writer pet: exclusive file lock (one container, one host, no
HA), STOP-FIRST deploys, `hnsw_enable_experimental_persistence` and its
segfault-on-reload class, and boot-time embedding before `:8080` binds. What
is the smallest storage change that lets read/query compute run as stateless
replicas sharing one corpus, while staying reversible?

## Decision

Move the derived index into **DuckLake**:

- **Catalog:** the Cloud SQL PostgreSQL database — DuckLake's native
  multi-client mode. The single writer ATTACHes read-write; N readers ATTACH
  `READ_ONLY`. Snapshot isolation and atomic publish come from Postgres
  transactions: one writer transaction = one DuckLake snapshot (spike-verified:
  a polling reader observes only pre/post states, never partials).
- **Data files:** Parquet on a GCS bucket
  (`gs://hetzner-agent-backplane-escurel-lake/`), reached via httpfs with an
  HMAC secret (`CREATE SECRET (TYPE gcs, KEY_ID …, SECRET …)`).
- **Compute:** local DuckDB per process, as today. The writer keeps its local
  serving tables (inner loop unchanged, embed-at-ingest); `publish` = one lake
  transaction `CREATE OR REPLACE TABLE lake.<t> AS FROM <t>` for
  `pages`/`links`/`blocks` + `group_members`/`external_endpoints`/
  `pack_subscriptions`, plus a `lake.escurel_manifest` row (model_id, dim,
  schema_version — the loader's fail-closed compat contract).
  `external_credentials` never leaves the writer. Readers poll
  `ducklake_snapshots('lake')`, bulk-load into a local **in-memory** DuckDB via
  the `merge_from_attached` pattern (vectors verbatim, no re-embed), build
  HNSW + FTS, and hot-swap the serving `Indexer`
  (`IndexerHandle = Arc<ArcSwap<Indexer>>`).
- **Seam:** an `IndexStore` trait (open / publish / adopt_latest) with
  `SingleFileStore` (today's behaviour, byte-identical) and `DuckLakeStore`,
  selected by `ESCUREL_INDEX_BACKEND` + `ESCUREL_ROLE` — coexisting and
  reversible.

## Spike findings that shaped the design (2026-07-17, DuckDB 1.5.4)

1. **DuckLake rejects fixed-size arrays**: `FLOAT[768]` columns fail with
   *"unsupported type FLOAT[768]"*. Lake tables store `FLOAT[]` (list); the
   reader casts back `::FLOAT[768]` during bulk-load, before the HNSW build.
2. **HNSW requires an in-memory DB (or the experimental flag)**: a file-backed
   reader DB refuses `CREATE INDEX … USING HNSW` without
   `hnsw_enable_experimental_persistence`. Readers therefore use an in-memory
   connection (with `temp_directory` for spill); the flag stays confined to
   the single-file backend. The segfault-on-reload class disappears on the
   reader path by construction.
3. **Atomicity holds**: a reader polling `count(*)` during a 100-statement
   writer transaction observed only the pre- and post-commit counts.
4. **Concurrent per-user appends via attached Postgres work**: two processes
   appending through `ATTACH … (TYPE postgres)` lost no writes (Phase B
   model for chat/events/CRDT re-homing).

## Consequences

- Readers become stateless cattle: no file lock, no Volume, no boot-time
  embedding (only query-time), readiness gates on first snapshot adopt.
- The writer stays the single mutation point (escurel's existing `write_lock`
  serialisation); DuckLake multi-writer conflict issues are avoided entirely.
- Reads are eventually consistent with publish cadence; per-user state keeps
  strong consistency (Phase B: plain Postgres tables in the same instance).
- New operational dependency: the Postgres catalog must be reachable for
  publish/adopt (readers keep serving their last-adopted snapshot when it is
  not — degraded-stale, not down).
- `CREATE OR REPLACE TABLE` publish rewrites all Parquet per publish — simple
  and atomic; incremental mirroring + `ducklake_merge_adjacent_files`
  compaction are named follow-ups.

## Follow-ups

Substrate repo: ADR-0015 rev.3 amendment; IaC for bucket/SA/HMAC/Cloud SQL +
Secret Manager + deploy wiring; host-pin/STOP-FIRST removal; replica count.
Escurel: Phase B per-user re-homing (chat/events/CRDT); snapshot
expiry/compaction cadence; incremental FTS; `sslmode=verify-ca`.
