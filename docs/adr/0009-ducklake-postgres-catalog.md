# ADR-0009 — DuckLake derived-index backend with a live Postgres catalog

**Status:** Accepted, 2026-07-18.
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

## Implementation (2026-07-18)

Shipped as 10 PRs on `DataZooDE/escurel` main, in order:

| PR | GitHub | What |
|---|---|---|
| 1 | [#288](https://github.com/DataZooDE/escurel/pull/288) | Validation spikes (this ADR's findings above) + ADR draft |
| 2 | [#289](https://github.com/DataZooDE/escurel/pull/289) | `IndexStore` seam + `IndexerHandle` — zero behavior change |
| 3 | [#290](https://github.com/DataZooDE/escurel/pull/290) | `LakeConfig` + attach/secret builders + `publish_lake` + dirty counter |
| 4 | [#291](https://github.com/DataZooDE/escurel/pull/291) | `adopt_lake` — reader bulk-load into an in-memory indexer |
| 5 | [#292](https://github.com/DataZooDE/escurel/pull/292) | Reader refresh task — poll, adopt, hot-swap |
| 6 | [#293](https://github.com/DataZooDE/escurel/pull/293) | `ESCUREL_INDEX_BACKEND`/`ESCUREL_ROLE` wiring, bootable reader, replica tool gating, readiness |
| 7 | [#294](https://github.com/DataZooDE/escurel/pull/294) | `publish_snapshot` admin tool + periodic publish + snapshot GC |
| 8 | [#295](https://github.com/DataZooDE/escurel/pull/295) | Phase B: chat re-homed to an attached-Postgres shared table |
| 9 | [#296](https://github.com/DataZooDE/escurel/pull/296) | Phase B: events re-homed to an attached-Postgres shared table |
| 10 | [#297](https://github.com/DataZooDE/escurel/pull/297) | Phase B: CRDT ops/snapshots re-homed to an attached-Postgres table |

Full architecture summary: [`docs/notes/2026-07-18-ducklake-architecture.md`](../notes/2026-07-18-ducklake-architecture.md).

**Real-world discoveries beyond the pre-implementation spikes** (each has a
`docs/notes/discovered/` entry):

1. **DuckLake rejects `FLOAT[768]`** (fixed-size arrays) — lake tables store
   `FLOAT[]`; readers cast back before the HNSW build (spike-anticipated,
   confirmed in PR 3).
2. **DuckDB's Postgres connector rejects `INSERT … RETURNING`** on any
   attached-Postgres write (`ATTACH … TYPE postgres`, not just the
   `ducklake:postgres:` catalog protocol) — discovered in PR 8 (chat),
   documented, and the fix pattern (resolve server-side values via a separate
   scalar `SELECT` first) was reused verbatim in PRs 9 and 10 with zero
   rediscovery cost.
3. **`BLOB` maps cleanly to Postgres `bytea`** through the same attach and
   round-trips byte-exact via `duckdb::ToSql` — verified empirically in PR 10
   before writing the CRDT op-log tables, precisely because the `FLOAT[768]`
   and `RETURNING` surprises above made "verify column-type behavior first"
   the house habit for this program.

**Phase B landed differently than originally sketched.** The plan's
Phase-B section proposed per-user object-storage prefixes (append-only
objects in GCS/Hetzner) for chat/events/CRDT. The shipped design instead
re-homes each to a **shared Postgres table** (`chat_pg`/`events_pg`/`crdt_pg`)
in the *same* Cloud SQL database as the lake catalog, all reusing
`LakeConfig.catalog_dsn` — zero new config surface, and every replica
(writer and every reader) attaches read-write to these tables at boot. This
is simpler than the object-prefix design and still satisfies the original
requirements (strong consistency, GDPR delete-by-row, no shared corpus
mutation) because a normal SQL table already gives read-your-writes and
transactional deletes for free.

**CRDT scope boundary (explicit, not a gap):** PR 10 makes durable CRDT
*storage* reachable from every replica — a reader can `list_snapshots` for
any page, and a session opened fresh on any replica loads correct history.
It does **not** build live cross-replica session failover: `SessionManager`
still runs one in-process `LiveDoc` actor per page, with no ingress-affinity
mechanism to route a page's `apply_op` calls back to whichever replica
opened its session. Loro's CRDT convergence makes this eventually-safe if it
ever happens by accident; making it a *supported* flow is a documented
follow-up, not attempted here.

## Follow-ups

Substrate repo: ADR-0015 rev.3 amendment; IaC for bucket/SA/HMAC/Cloud SQL +
Secret Manager + deploy wiring; host-pin/STOP-FIRST removal; replica count.
Escurel: CRDT live cross-replica session affinity (ingress stickiness);
snapshot expiry/compaction cadence beyond the `ESCUREL_SNAPSHOT_KEEP` count
retention already shipped; incremental FTS; `sslmode=verify-ca`.
