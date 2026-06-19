# Platform — auth, tenancy, quotas, observability

The platform concerns that sit above the storage and indexer
layers: how identity flows in, how a request is routed to a
tenant, how concurrent work is bounded, how the system reports
on itself.

## Auth

OIDC, generic. The server is configured with one issuer URL,
one audience, and a small set of claim names:

```toml
[auth]
oidc_issuer        = "https://auth.example.com/realms/main"
oidc_audience      = "escurel"
tenant_claim       = "tenant"          # which claim names the tenant
admin_role_claim   = "roles"           # which claim lists role memberships
admin_role_value   = "escurel:admin"   # the role that grants admin access
groups_claim       = "roles"           # which claim lists RBAC group/role names
jwks_refresh_secs  = 300
```

The OIDC issuer is an external dependency; this spec does not
ship one. Per-target deployment bindings (e.g.
[`../deploy/substrate.md`](../deploy/substrate.md)) name the
concrete issuer source — for the substrate target this is the
Vault OIDC role or a Dex/Keycloak Nomad job classified as a
pet.

On every request:

1. Extract `Bearer` token from the `Authorization` header.
2. Verify signature against the issuer's JWKS (cached, refreshed
   per `jwks_refresh_secs`). Reject on any failure with the
   transport's native error shape (HTTP 401, WS close code 4401).
3. Resolve **tenant_id** = the value of the `tenant_claim` claim.
   Reject if absent.
4. Look up the tenant in the tenant manager; reject with
   `NOT_FOUND` if it does not exist (no auto-provision — see
   below). Reject with `UNAVAILABLE` if `status != active`.
5. Resolve **role** = whether `admin_role_value` is in the
   `admin_role_claim` array. Admin endpoints require it; agent
   endpoints do not check it.
6. Resolve **groups** = the names in the `groups_claim` (a JSON array,
   or a single string split on whitespace/commas). These feed the
   data-level group ACL (see
   [protocol.md §Group ACL](protocol.md) and
   [ADR-0004](../adr/0004-rbac-groups.md)); the configured
   `admin_role_value` and the reserved names `public`/`owner`/`admin`
   are stripped so they can never act as ordinary header-grantable
   groups.
7. Stamp the resolved `(tenant_id, role, sub, groups)` onto the request
   context for downstream layers.

The same flow handles HTTP (header on every request) and WS
(token in the upgrade request).

Two operational notes:

- **No machine-to-machine shortcut.** Even the CLI uses an OIDC
  token. Operators get one by running `escurel login` which uses the
  device-code flow against the configured issuer.
- **Admin-on-multiple-tenants.** Admin endpoints take a `tenant`
  field in the request body and ignore the token's tenant claim
  (an admin operates across tenants). Agent endpoints respect
  the token's claim and reject mismatched `tenant` fields.

## Tenant lifecycle

Tenants are explicit — there is no auto-provision-on-first-request
flow in v1. An admin creates a tenant before any agent can connect
under that tenant's claim.

### Manifest

Each tenant's `manifest.toml` (in its directory root) carries:

```toml
[tenant]
id            = "acme"
display_name  = "Acme Corp"
status        = "active"          # active | suspended | suspended_corrupt | deleting
created_at    = "2026-05-18T10:00:00Z"

[tenant.embedding]
provider = "embeddinggemma"        # overrides global default

[tenant.quotas]
queries_per_minute     = 600
writes_per_minute      = 120
embeds_per_minute      = 300
concurrent_sessions    = 32
storage_soft_cap_bytes = 10_737_418_240   # 10 GiB; soft = log a warning
storage_hard_cap_bytes = 53_687_091_200   # 50 GiB; hard = reject writes
```

The manifest is the authoritative source for quotas; the server's
global config is the *default* used when no per-tenant value is set.

### Operations

| operation | what happens | preconditions |
|---|---|---|
| **Create** | `mkdir` tenant dir, write manifest, initialise empty `escurel.duckdb` with the `vss` and `fts` extensions loaded and schema applied, drop in the `escurel` meta-skill page. ~50 ms. | id is unique; admin role |
| **List** | enumerate manifests under `${ESCUREL_DATA_DIR}/tenants/` | admin role |
| **Get** | parse manifest plus runtime status (open sessions, current usage) | admin role |
| **Update** | rewrite manifest fields under per-tenant write lock; quotas take effect on next request | admin role |
| **Suspend** | set `status = suspended`; existing connections drained over 30 s | admin role |
| **Delete** | mark `status = deleting`, drain connections, wipe 15 locations (markdown, lanes, CRDT, cache, spool, logs, backups, metrics labels, …), remove manifest. Atomic: either the tenant fully exists or it is gone | admin role; explicit `confirm = "<id>"` |
| **Export** | tar the tenant dir minus `cache/`; verified atomic | admin role |
| **Import** | inverse of export; rejects if id already exists unless `overwrite=true` | admin role |

The `escurel` meta-skill is auto-shipped on create. Operators may
extend the page with tenant-specific guidance (appended after
the standard sections, in markdown) but cannot delete it or
remove the standard sections; the indexer enforces this on
write.

## Concurrency

One Tokio runtime, one `Server` global, one `TenantManager`.

### Tenant manager

```rust
pub struct TenantManager {
    handles: DashMap<TenantId, Arc<TenantHandle>>,
    lru:     Mutex<LruCache<TenantId, ()>>,
    cfg:     Arc<Config>,
    store:   Arc<dyn LaneStore>,
}

pub struct TenantHandle {
    pub manifest: ArcSwap<TenantManifest>,
    pub lock:     RwLock<TenantWriter>,   // single-writer; many readers
    pub duck:     DuckdbPool,             // read pool against escurel.duckdb; writer is in TenantWriter
    pub live:     LiveSessions,           // open CRDT sessions for this tenant
    pub quota:    QuotaState,             // token-buckets + concurrent counter
}
```

- **Read path** (search/resolve/expand/neighbours/list_skills/
  list_instances/run_stored_query): no lock; a DuckDB read
  connection is pulled from `duck` and serves all queries
  (relational, vector via `vss`, full-text via `fts`) under
  DuckDB's MVCC.
- **Write path** (update_page / apply_op / close_session(commit=true)):
  acquire `lock.write()`. Background jobs (audit/rebuild/compact)
  also take the write lock. Hold times are bounded by the
  one-page DuckDB transaction.
- **CRDT live state**: held in `live`. Op application is itself
  guarded by the per-page actor's mailbox (one task per open
  page); the actor takes the tenant write lock when it
  persists ops into the `crdt_ops` table and when it publishes
  to canonical markdown on commit.

### Resource caps

- `TenantHandle` LRU defaults to 64; idle eviction after 5 min.
- DuckDB read pool: 16 connections per tenant; reused.
- DuckDB writer: one in flight at a time per tenant (per the
  per-tenant write lock; DuckDB's own file lock enforces this
  at the storage layer too).
- Embed worker pool: a global Tokio task pool with a bounded
  per-tenant queue (default 32 in-flight tasks). The pool size
  scales with detected cores; CUDA/Metal uses one worker per
  device.

### Tail latency budget

Targets:

| operation | p50 target | p95 target |
|---|---|---|
| `search` (block, k=10) | 12 ms | 25 ms |
| `resolve` | 2 ms | 4 ms |
| `expand` | 5 ms | 10 ms |
| `neighbours` (limit=100) | 6 ms | 15 ms |
| `list_skills` | 4 ms | 8 ms |
| `list_instances` (limit=50, with `at` filter) | 6 ms | 12 ms |
| `run_stored_query` | varies | varies |
| `update_page` | 40 ms | 100 ms |
| `apply_op` | 5 ms | 20 ms |

These budgets are measured at 100 k instances per skill; they
extrapolate sub-linearly to 1 M. The CI suite carries a regression
check that compares against these numbers on a representative
corpus.

## Quotas

Three dimensions, all token-bucket per tenant:

| dimension | refill | what counts |
|---|---|---|
| `queries_per_minute` | continuous | all read tools: `search`, `resolve`, `expand`, `neighbours`, `list_skills`, `list_instances`, `run_stored_query`, `validate` |
| `writes_per_minute` | continuous | all writes: `update_page`, `apply_op`, `close_session(commit=true)`; also `embeds_per_minute` is debited from this bucket when a write triggers embedding |
| `embeds_per_minute` | continuous | counts embedding jobs (one per new/changed block); shared bucket means a bulk import triggers backpressure here |
| `concurrent_sessions` | semaphore | counts open MCP sessions and WS connections |

Defaults are in the server config; per-tenant overrides in the
manifest. On bucket exhaustion the server returns
HTTP 429 / WS error code 4429 with
a `retry_after_ms` hint. Live-mode WS connections that hit the
concurrent-sessions cap on connect are rejected at upgrade time
with HTTP 429.

Quota state is in-memory only — restarting the server zeros all
buckets. This is intentional: the buckets are a rate-shaping
device, not a billing system. SaaS deployments running a
metered-billing layer subscribe to the OTel `escurel.tool_calls{...}`
counter and roll their own accounting. Substrate deployments
treat quotas the same way: durable accounting (billing, audit)
lives in the OTel pipeline, not in the `escurel-server` process.

## Observability

Three streams, all standard.

### Tracing (OpenTelemetry)

Every tool call emits one span at the gateway, with attributes:

```
escurel.tenant       = "<id>"
escurel.tool         = "search" | "expand" | ...
escurel.transport    = "mcp_http" | "ws"
escurel.role         = "agent" | "admin"
escurel.subject      = "<sub from token>"
```

Child spans cover storage operations (`duckdb.tx`,
`duckdb.vss_search`, `duckdb.fts_match`, `embed.batch`), CRDT
operations (`crdt.apply_op`, `crdt.snapshot`), and any
external HTTP calls.

Errors are recorded with the OTel error semantic conventions.
Issues (validation warnings/errors) are recorded as span events,
not as span errors — they're an expected output.

OTLP/gRPC exporter to a configurable endpoint
(`observability.otlp_endpoint`); falls back to a no-op if the
endpoint is unset.

### Metrics

A small set of OTel-conventional metrics:

| metric | type | labels |
|---|---|---|
| `escurel.tool_calls` | counter | `tenant`, `tool`, `transport`, `status` (ok/error/quota_exhausted) |
| `escurel.tool_latency_ms` | histogram | `tenant`, `tool`, `transport` |
| `escurel.write_lock_wait_ms` | histogram | `tenant` |
| `escurel.embed_batch_size` | histogram | `tenant` |
| `escurel.embed_queue_depth` | gauge | `tenant` |
| `escurel.live_sessions_open` | gauge | `tenant` |
| `escurel.storage_bytes` | gauge | `tenant`, `lane` (`markdown` / `duckdb` / `external_ducklake`) |
| `escurel.audit_drift` | gauge | `tenant`, `category` (`mn-d` markdown-not-in-duckdb, `i-no-m` indexed-but-no-markdown) |

Scraped at `/metrics` on a dedicated listener (default `:9090`,
tailnet-only — see [`operations.md`](../operations.md)). The live
gateway renders these through a Prometheus registry, so the wire
names are `_`-separated: `escurel.tool_calls` is exposed as
`escurel_tool_calls`, etc. Trace spans are exported via OTLP;
metric OTLP export is not yet wired.

**Implemented today:** `escurel_tool_calls`,
`escurel_tool_latency_ms`, `escurel_live_sessions_open`, and
`escurel_audit_drift`, plus the gateway-level `escurel_up` and
`escurel_requests_total{route,status}`. The remaining
histograms/gauges in the table above (`write_lock_wait_ms`,
`embed_batch_size`, `embed_queue_depth`, `storage_bytes`) are
**reserved** — specified here, not yet populated.

### Logs

Structured JSON to stdout. One line per significant event;
correlation by trace id (`trace_id`, `span_id`). Schema (every
record):

```json
{
  "ts": "2026-05-18T19:30:01.234Z",
  "level": "info",
  "msg": "tool.completed",
  "tenant": "acme",
  "tool": "search",
  "transport": "mcp_http",
  "subject": "user:joachim",
  "trace_id": "...",
  "span_id": "...",
  "duration_ms": 11.2,
  "result_count": 10
}
```

Audit-relevant events (tenant create/delete/suspend, admin role
use, quota exhaustion) carry `level: notice` so log routers can
funnel them to a SIEM. Sensitive fields (request body, page
content) are never logged — only counts and ids.

The log shape is a **stable contract**. Substrate-side audit
collectors (e.g. the GCS audit-bucket shipper named in
[`../deploy/substrate.md §3`](../deploy/substrate.md#3--audit-collector-contract))
depend on these fields being present and named exactly.
Required on every record: `ts`, `level`, `msg`, `tenant`,
`tool`, `transport`, `subject`, `trace_id`, `duration_ms`.
Required additional field for production deployments: `env`
∈ `{"prod", "nonprod"}` — substrate audit routing keys off
it. Optional fields (result counts, sizes, etc.) may be added
without contract impact; renaming or removing required fields
is a breaking change.

`log_format = "text"` switches to single-line human-readable
output; useful for local development, not production.

### Health endpoints

- `GET /healthz` — liveness; returns 200 if the runtime is up
- `GET /readyz`  — readiness; 200 only when embedding is loaded
  AND storage is reachable AND OTel exporter has connected (or
  is configured no-op)
- `GET /metrics` — Prometheus scrape
- `health` MCP tool — richer JSON with version, embedding
  status, tenant count, lane stats

Substrate orchestrators (Nomad) wire `/readyz` as the
deployment readiness probe; blue/green canary promotion
respects it, so a green allocation receives public traffic only
after embedding is loaded, storage is reachable, and OTel has
connected.

## Failure modes recap

| failure | quota effect | observability signal |
|---|---|---|
| Embedding model load fail at startup | writes reject with `embedding_unavailable`; reads OK | `/readyz` returns 503; `escurel.tool_calls{status=error}` on writes; log at `level: critical` |
| Storage backend timeout (S3) | writes queue to spool; reads cached | `escurel.storage_bytes` gauge stale; log at `level: warning` per retry |
| Per-tenant lane corruption | tenant auto-suspended | `escurel.audit_drift` spikes; log at `level: critical`; other tenants unaffected |
| Server OOM (large embed batch) | the offending request fails; the runtime continues | OTel error span; `escurel.tool_calls{status=error, tool=update_page}` increments |
| Quota exhausted | request returns 429/RESOURCE_EXHAUSTED with retry hint | `escurel.tool_calls{status=quota_exhausted}` increments |
