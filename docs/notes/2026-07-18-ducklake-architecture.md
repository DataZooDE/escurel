# The DuckLake storage architecture, as shipped

A short summary of the final system for anyone landing here without having
read all ten PRs. See [ADR-0009](../adr/0009-ducklake-postgres-catalog.md)
for the decision record and rationale; this note is the "how it fits
together" companion.

## The two backends

`ESCUREL_INDEX_BACKEND` selects between:

- **`single-file`** (default): today's behaviour, byte-identical to before
  this program. One DuckDB file per tenant, one process, no HA.
- **`ducklake`**: the derived index (pages/links/blocks + a few registry
  tables) lives in a DuckLake catalog backed by a live Postgres database
  (Cloud SQL), with Parquet data files on GCS. `ESCUREL_ROLE` then picks:
  - **`writer`** (default): keeps its own local single-file DuckDB as the
    serving index — the ingest inner loop (parse → embed → write) is
    completely unchanged — and additionally attaches the lake read-write so
    it can *publish* snapshots.
  - **`reader`**: has **no local single-file DuckDB at all**. It boots by
    adopting the latest published snapshot into an **in-memory** DuckDB,
    then polls for newer snapshots and hot-swaps them in.

## The seam: `IndexStore`, `IndexerHandle`

`crates/escurel-index/src/snapshot/` defines the trait every backend
implements:

```rust
trait IndexStore {
    async fn open(&self) -> Result<OpenedIndex, SnapshotError>;
    async fn publish(&self, ix: &Indexer) -> Result<PublishReport, SnapshotError>;
    async fn adopt_latest(&self, current: Option<i64>) -> Result<Option<AdoptedIndex>, SnapshotError>;
}
```

`SingleFileStore` implements only `open` (PR 2 — a pure refactor extracting
this seam with zero behavior change, so the single-file path never
regressed across the whole program). The `ducklake` backend calls
`publish_lake`/`adopt_lake` directly (PRs 3–4) rather than through a
`DuckLakeStore` wrapper type — the trait exists as the conceptual boundary
and for `SingleFileStore`'s zero-diff guarantee, not as a second concrete
implementation.

Every server process holds `Option<IndexerHandle>` where
`IndexerHandle = Arc<ArcSwap<Indexer>>` (PR 2). Request-handling code
captures `handle.current()` **once per request** — so a background snapshot
swap never tears an in-flight request; it finishes against the `Arc` it
captured, and the old indexer is dropped once nothing references it.

## Publish → adopt → refresh

**Publish** (PR 3, `escurel_index::snapshot::publish_lake`): the writer,
under its existing per-tenant `write_lock`, mirrors its local
`pages`/`links`/`blocks` (+ `group_members`/`external_endpoints`/
`pack_subscriptions`) into `CREATE OR REPLACE TABLE lake.<t> AS FROM <t>`
statements inside **one transaction** — which is exactly one DuckLake
snapshot. A `lake.escurel_manifest` row (schema_version/model_id/dim/epoch)
goes in the same transaction as the readers' fail-closed compatibility
check. `external_credentials` never leaves the writer. `blocks.dense_vec`
casts to `FLOAT[]` on the way out — DuckLake rejects fixed-size arrays
(`FLOAT[768]`), a spike-anticipated, PR-3-confirmed constraint.

**Adopt** (PR 4, `adopt_lake`): a reader opens a **fresh in-memory** DuckDB
(`Connection::open_in_memory()` — file-backed DBs refuse
`CREATE INDEX … USING HNSW` without the experimental persistence flag,
which readers deliberately never set), attaches the lake `READ_ONLY`, checks
the manifest, bulk-loads with the `dense_vec::FLOAT[768]` cast-back, builds
HNSW + FTS, detaches, and returns a fresh `Indexer`.

**Refresh** (PR 5, `escurel-server/src/snapshot_refresh.rs`): a background
`RefreshTask` polls `latest_lake_snapshot_id` on `ESCUREL_SNAPSHOT_REFRESH_SECS`
(default 30s), calls `adopt_lake` when the lake has advanced, and
`IndexerHandle::swap`s the result in. A poll or adopt failure is logged and
the loop keeps serving the last-known-good snapshot — never panics, never
exits. This is what makes a reader replica genuinely stateless cattle:
kill one, the others keep serving; bring up a new one, it catches up on its
own.

## Roles, gating, readiness (PR 6)

A `ducklake+reader` boot has no CRDT backend, no seed, no meta-skill
bootstrap — those stay writer-only concerns. Mutating page/ingest tools
return a typed `read_only_replica` error on a reader. `ReadinessReport`
gates `/readyz` on the boot-time synchronous adopt having succeeded, so
kamal-proxy-style blue/green promotion only routes traffic to a reader once
it's actually serving a snapshot.

## Publish control (PR 7)

An admin MCP tool `publish_snapshot` (writer-only) triggers a publish
on demand; an optional `ESCUREL_SNAPSHOT_PUBLISH_SECS` timer does it
periodically when dirty (default `0` = manual-only — `publish_lake`
already no-ops on a clean lake via the indexer's mutation-epoch counter).
Every successful publish runs `gc_lake_snapshots` afterward
(`ESCUREL_SNAPSHOT_KEEP`, default 5) — `ducklake_expire_snapshots` +
`ducklake_cleanup_old_files`, guarded against a real extension crash when
called with `older_than => NULL` on a lake with fewer snapshots than the
retention target (see the PR 7 discovered-note).

## Phase B: the three shared-Postgres tables (PRs 8–10)

Everything above covers the **shared corpus** — read-mostly, eventually
consistent, fine to duplicate into an in-memory reader copy. Three other
data classes are per-user/per-session and need strong consistency
(read-your-writes) instead: chat history, the event bus, and CRDT
op-logs/snapshots. Rather than the per-user object-storage-prefix design
originally sketched, all three landed as **shared Postgres tables** in the
*same* Cloud SQL database the lake catalog already uses — `chat_pg`,
`events_pg`, `crdt_pg`, each reusing `LakeConfig.catalog_dsn` (zero new
config surface). Every replica, writer **and every reader**, attaches to
these **read-write** at boot — the one deliberate per-user write surface on
a reader; the shared corpus itself stays read-only there.

Each of the three followed the same shape (`attach_<x>_pg_sql` +
`create_<x>_pg_table_sql` + `attach_<x>_pg`, idempotent `ATTACH IF NOT
EXISTS` + `CREATE TABLE IF NOT EXISTS`) and the same reader-gating pattern
(`has_shared_<x>()` lifts the `unsupported_on_replica` gate PR 6 put in
place). PR 8 (chat) discovered that DuckDB's Postgres connector rejects
`INSERT … RETURNING` on any attached-Postgres write; PRs 9 and 10 reused
that fix with zero rediscovery cost. PR 10 (CRDT) needed one architectural
deviation: `DuckdbCrdtBackend` lives in `escurel-crdt`, which doesn't depend
on `escurel-index`, so its attach lives in `escurel-crdt/src/pg.rs` rather
than `escurel-index/src/snapshot/`; and a *second*, independent attach
exists on `Indexer` itself because `list_snapshots` reads `crdt_snapshots`
directly off the indexer's connection, bypassing the `CrdtBackend` trait.

**What Phase B does not do:** live cross-replica CRDT session failover.
`SessionManager` still runs one in-process `LiveDoc` actor per page; nothing
routes a page's `apply_op` calls to whichever replica opened its session if
a client's requests land on a different one mid-session. The durable
storage is shared and correct; making live failover a *supported* flow
needs ingress affinity, which is an explicit, documented follow-up.

## Config summary

| Var | Default | Meaning |
|---|---|---|
| `ESCUREL_INDEX_BACKEND` | `single-file` | or `ducklake` |
| `ESCUREL_ROLE` | `writer` | or `reader` (requires `ducklake`) |
| `ESCUREL_DUCKLAKE_CATALOG_DSN` | — | Postgres DSN or DuckDB-file path (offline/dev) |
| `ESCUREL_DUCKLAKE_DATA_PATH` | — | `gs://…`, `s3://…`, or local dir |
| `ESCUREL_DUCKLAKE_GCS_KEY_ID` / `_GCS_SECRET` | — | HMAC for `gs://` |
| `ESCUREL_DUCKLAKE_S3_*` | — | `TYPE s3` secret (MinIO tests / Hetzner) |
| `ESCUREL_SNAPSHOT_REFRESH_SECS` | `30` | reader poll interval |
| `ESCUREL_SNAPSHOT_PUBLISH_SECS` | `0` | writer periodic publish when dirty |
| `ESCUREL_SNAPSHOT_KEEP` | `5` | GC retention |

## Further reading

- [ADR-0009](../adr/0009-ducklake-postgres-catalog.md) — the decision record.
- `docs/notes/discovered/2026-07-17-ducklake-spike-results.md` — pre-implementation validation.
- `docs/notes/discovered/2026-07-17-ducklake-data-inlining.md` — the writer's `DATA_INLINING_ROW_LIMIT 0` requirement.
- `docs/notes/discovered/2026-07-18-duckdb-postgres-attach-no-returning.md` — the `RETURNING` gotcha.
- `docs/notes/discovered/2026-07-18-ducklake-snapshot-gc.md` — the GC `NULL`-crash gotcha.
- `docs/notes/discovered/2026-07-18-duckdb-blob-bytea-round-trip.md` — CRDT bytes verification.
