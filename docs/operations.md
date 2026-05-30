# Operating escurel (v1)

Day-2 operator guide. For first deploy (env vars, the three deploy
targets, Nomad/Packer/Tailscale artefacts) start at
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
| `GET /healthz` | Liveness. Always `200 OK` while the process is up; dependency-free. Wire this to the Nomad/Consul check. |
| `GET /readyz` | Readiness. `200` only when LaneStore + indexer + embedder are all up; `503` with a per-component JSON body otherwise. A degraded embedder (model failed to load) shows here as `{"components":{"embedder":false}}` — the process still serves liveness and read traffic. |
| `GET /version` | The build version (`VERSION` / `ESCUREL_VERSION`). |
| `GET /metrics` | Prometheus exposition on a **dedicated listener** (`ESCUREL_OBSERVABILITY_METRICS_LISTEN`, default `:9090`) — *not* the main HTTP port. Exposes `escurel_up`, `escurel_requests_total{route,status}`, and the per-tool families `escurel_tool_calls{tenant,tool,transport,status}`, `escurel_tool_latency_ms`, `escurel_live_sessions_open`, `escurel_audit_drift`. Scrape via the tailnet-only `escurel-metrics` Consul service. |

Logs are structured JSON on stdout with `ts`, `level`, `msg`, `app`,
`env`, `version`, `request_id` (per [`spec/platform.md`](spec/platform.md)).
Every `/mcp` request carries an `X-Request-Id` (inbound header honoured,
else a fresh ULID) threaded into a `mcp.request` span (which also carries
`transport` + `trace_id`). Each `tools/call` emits a `tool.completed`
record adding `tenant`, `tool`, `subject`, `status`, and `duration_ms` —
the per-call audit line.

## The admin surface

Admin operations are the `EscurelAdmin` gRPC service (`:8081`, tailnet-only).
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
| `EmbeddingReload` | retry the embedder model load after a degraded start |
| `QuotaGet` | inspect remaining per-tenant token budgets + occupied session slots |

`tenant_id` arguments are validated (lowercase ascii / digit / `-` /
`_`, 1–64 chars) before any filesystem path is built — a malformed id
is rejected, not used to escape the tenant root.

## Common runbooks

### Back up a tenant

`TenantExport` streams a tar+gz of the tenant's `markdown/` tree (the
canonical corpus — CRDT runtime state is deliberately *not* exported;
it resets to markdown head on import). The substrate ships these to
the backups bucket on a schedule via the periodic
`escurel-export-shipper` Nomad job
([`deploy/escurel-export-shipper.nomad.hcl`](deploy/escurel-export-shipper.nomad.hcl)).

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

A tenant hitting its rate budget gets `429` (HTTP) / `RESOURCE_EXHAUSTED`
(gRPC) with a `Retry-After-Ms` hint. Inspect remaining budget with
`QuotaGet`. The three dimensions are queries, writes+embeds, and
concurrent sessions.

### Lane compaction

Over a long-lived live-editing session the `crdt_ops` table grows.
`CompactLanes` deletes ops subsumed by the latest snapshot per page.
Safe to run any time; it never touches ops newer than the last
snapshot.

## Dependency / license audit

`cargo deny check` against the root `deny.toml` gates licenses,
advisories, and sources. Run it at dep freezes / before a release, not
per-PR — see [`deploy/README.md`](deploy/README.md#license--advisory-audit).
