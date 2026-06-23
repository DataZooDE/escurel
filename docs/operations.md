# Operating escurel (v1)

Day-2 operator guide. For first deploy (env vars, the three deploy
targets, the substrate Kamal binding) start at
[`deploy/README.md`](deploy/README.md). For the wire surface see
[`spec/protocol.md`](spec/protocol.md); for the storage / recovery
model see [`spec/storage.md`](spec/storage.md).

escurel is a **pet** stateful service: one replica per environment,
pinned to a host volume at `/data`. The per-tenant DuckDB file is
*cattle* — it is rebuilt from the canonical markdown in the LaneStore
on first boot of a fresh host (see [Node loss](#node-loss--fresh-host)).

## Health & liveness

| Endpoint | Meaning |
|---|---|
| `GET /healthz` | Liveness. Always `200 OK` while the process is up; dependency-free. Wire this to the kamal-proxy healthcheck. |
| `GET /readyz` | Readiness. `200` only when LaneStore + indexer + embedder are all up; `503` with a per-component JSON body otherwise. A degraded embedder (model failed to load) shows here as `{"components":{"embedder":false}}` — the process still serves liveness and read traffic. |
| `GET /version` | The build version (`VERSION` / `ESCUREL_VERSION`). |
| `GET /metrics` | Prometheus exposition on a **dedicated listener** (`ESCUREL_OBSERVABILITY_METRICS_LISTEN`, default `:9090`) — *not* the main HTTP port. Exposes `escurel_up`, `escurel_requests_total{route,status}`, and the per-tool families `escurel_tool_calls{tenant,tool,transport,status}`, `escurel_tool_latency_ms`, `escurel_live_sessions_open`, `escurel_audit_drift`. Scraped into Managed Prometheus via the substrate's metrics path. |

Logs are structured JSON on stdout with `ts`, `level`, `msg`, `app`,
`env`, `version`, `request_id` (per [`spec/platform.md`](spec/platform.md)).
Every `/mcp` request carries an `X-Request-Id` (inbound header honoured,
else a fresh ULID) threaded into a `mcp.request` span (which also carries
`transport` + `trace_id`). Each `tools/call` emits a `tool.completed`
record adding `tenant`, `tool`, `subject`, `status`, and `duration_ms` —
the per-call audit line.

## The admin surface

Admin operations are admin-role-gated MCP tools on `POST /mcp` (the same `:8080` HTTP surface as the agent tools).
Every method requires a bearer JWT carrying the admin role
(`ESCUREL_AUTH_ADMIN_ROLE_VALUE`, default `escurel:admin`); `Health` is
the one exception (auth-free liveness). A missing/invalid bearer is
`UNAUTHENTICATED`; an agent-role bearer on an admin method is
`PERMISSION_DENIED`.

| RPC | Use |
|---|---|
| `Health` | liveness + build version (auth-free) |
| `TenantCreate` / `TenantList` / `TenantGet` / `TenantUpdate` / `TenantDelete` | tenant lifecycle |
| `TenantExport` (server-stream) / `TenantImport` (client-stream) | backup / restore — tar+gz of the tenant's canonical markdown |
| `Audit` | two-way drift between markdown and the index |
| `Rebuild` (server-stream) | re-index from canonical markdown; one progress chunk per page |
| `CompactLanes` (server-stream) | delete `crdt_ops` rows subsumed by the latest snapshot; reports ops + bytes reclaimed |
| `AttachExternal` | wire a read-only external DuckDB/catalog into the tenant connection for `[[query::*]]` over external tables |
| `RegisterCredential` / `ListCredentials` / `DeleteCredential` | the `sql_view` backend's external-source secret registry (DuckDB `CREATE SECRET`; secrets are never echoed back) |
| `CreateSqlInstance` | materialise a read-only `sql_view` instance from a `sql_view` skill |
| `ValidateBindings` | re-probe every `sql_view` for schema drift; a `binding_degraded` view reads fail-closed |
| `EmbeddingReload` | retry the embedder model load after a degraded start |
| `QuotaGet` | inspect remaining per-tenant token budgets + occupied session slots |

`tenant_id` arguments are validated (lowercase ascii / digit / `-` /
`_`, 1–64 chars) before any filesystem path is built — a malformed id
is rejected, not used to escape the tenant root.

## Common runbooks

### Back up a tenant

`TenantExport` streams a tar+gz of the tenant's `markdown/` tree (the
canonical corpus — CRDT runtime state is deliberately *not* exported;
it resets to markdown head on import) — the *logical* per-tenant export.
Durable DR is the substrate's **Volume** backup: `backup-data.yml`
(restic → GCS) snapshots the whole `/data` Volume on a 6h cron, with a
verified `restore-dryrun` (see [`deploy/substrate.md §4`](deploy/substrate.md)).

### Restore a tenant

`TenantImport` unpacks a tar+gz into the named tenant's `markdown/`
dir, then `Rebuild` re-indexes it. Import into a fresh server resets
all live sessions to markdown head — by design.

### Drift between markdown and the index

Run `Audit`. It returns `markdown_not_in_duckdb` (pages on disk the
index hasn't seen) and `indexed_but_no_markdown` (stale index rows).
`Rebuild` reconciles by re-indexing from the canonical markdown.

### Node loss / fresh host

**Automatic.** On boot, if the per-tenant `escurel.duckdb` is absent
but the LaneStore still holds canonical markdown (fresh host, wiped
local volume, `/recreate-node`), the binary rebuilds the index from
that markdown before serving traffic. No operator action is needed for
the cattle-node-loss case. If the LaneStore *itself* is lost, restore
it from the backups bucket first, then let the next boot rebuild.

The mid-write crash case is covered by the DuckDB transaction: a
process killed mid-`update_page` rolls back pages/links/blocks/crdt_ops
together; the markdown file is left at its previous version (the
rename publishes only after commit). See
[`spec/storage.md`](spec/storage.md) and the crash-recovery tests in
`crates/escurel-index/tests/crash_recovery.rs`.

### Degraded embedder

If the model fails to load at startup (missing artefact, OOM), the
server boots degraded: `/readyz` reports `embedder: false`, read
tools that don't need vectors keep working, and `search`'s vector arm
is unavailable until recovery. Fix the underlying cause (e.g. the
baked model path `ESCUREL_EMBEDDING_MODEL`), then call
`EmbeddingReload` — no restart required.

### Quota exhaustion

A tenant hitting its rate budget gets `429` (HTTP) with a
`Retry-After-Ms` hint. Inspect remaining budget with
`QuotaGet`. The three dimensions are queries, writes+embeds, and
concurrent sessions.

### Lane compaction

Over a long-lived live-editing session the `crdt_ops` table grows.
`CompactLanes` deletes ops subsumed by the latest snapshot per page.
Safe to run any time; it never touches ops newer than the last
snapshot.

### Document ingestion (PDF/DOCX/text)

Document-backed instances are created by uploading a file to the
authenticated `POST /ingest/upload` (`{content_type, bytes_b64, title?}`) or
`POST /ingest` (for a blob already in `blobs/inbox/`). The server records an
ingest event, then extracts → chunks → embeds → materialises one page. The
default server build ships the in-process **kreuzberg** extractor
(PDF/DOCX/PPTX/XLSX); `text/*` needs no native deps. Build
`--no-default-features` for a born-digital-text-only server (kreuzberg is the
`kreuzberg` Cargo feature, on by default; it requires rustc ≥ 1.91 and bundles
pdfium). A MIME no document skill `accepts:` parks the upload with
`no_handler_skill` (the inbox blob is retained, not lost); an extractor failure
marks the instance `extraction_failed` and likewise retains the blob. Blobs are
content-addressed and counted against the tenant's blob quota; re-ingesting
identical bytes is idempotent. To re-derive chunks after an extractor upgrade,
`Rebuild` re-extracts from the retained blob.

### Bulk-loading a large corpus (offline loader + transfer)

Ingesting a large corpus (tens of thousands of PDFs) through `POST /ingest/upload`
is hopeless: each chunk is one embed, so the per-tenant Embeds quota turns it into
a multi-week trickle. Instead do the heavy work **offline** with the
`escurel-loader` binary, then **transfer** the result into the live tenant
carrying the embeddings as data — production never re-embeds.

```
# 1. Build a throwaway loader instance from a directory of documents, at full
#    speed with an offline embedder (no server, no quota).
escurel-loader build \
    --src /data/corpus --out /data/loader --skill attachment \
    --embedder hash            # use the SAME embedder model the live tenant runs

# 2. Transfer into the live data dir. --expect-model asserts the live tenant's
#    embedder identity; a manifest mismatch aborts before any rows move.
escurel-loader transfer \
    --from /data/loader --to /var/lib/escurel/tenants/acme \
    --tenant acme --expect-model hash \
    --on-collision skip        # additive + idempotent (default); replace | error
```

The transfer validates the loader manifest (`model_id` / `dim` /
`schema_version`) against the live tenant, copies blobs + overlay markdown
(files first), then merges `pages`/`links`/`blocks` DuckDB→DuckDB with the
vector index rebuilt once. `skip` is idempotent: a re-run resumes cleanly and
never duplicates a `page_id` (the per-document `instance_id` is the file's
content sha256). It is **host-side** operator work (it `ATTACH`es two DuckDB
files on disk), so it runs from the loader binary, not the gateway HTTP CLI.
The `--embedder` you build with **must** match the live tenant's model, or
retrieval silently degrades — the `--expect-model` gate is the safety net. See
[`docs/notes/2026-06-23-batch-loader.md`](notes/2026-06-23-batch-loader.md).

### SQL-view binding drift

`sql_view` instances project a read-only DuckDB view over an external source,
fingerprinted at creation. Run `ValidateBindings` to re-probe every view; one
whose source schema drifted comes back `binding_degraded` and **reads
fail-closed** (an `Issue`, never wrong rows) until reconciled. Fix the source
(or re-create the instance), then re-run `ValidateBindings`. `Rebuild`
reconstructs the views (`ATTACH` + `CREATE VIEW`) from each instance's
`backend_ref`.

### Rotate an external-source credential

`sql_view` source secrets live in the server-side registry, never in markdown.
`RegisterCredential {name, connector, secret}` upserts by name (rotation =
re-register the same name with the new secret); `ListCredentials` returns names
+ connectors only (never the secret); `DeleteCredential {name}` removes one.
Views bound to a deleted/rotated credential reconnect on the next `Rebuild` or
read.

## Dependency / license audit

`cargo deny check` against the root `deny.toml` gates licenses,
advisories, and sources. Run it at dep freezes / before a release, not
per-PR — see [`deploy/README.md`](deploy/README.md#license--advisory-audit).
