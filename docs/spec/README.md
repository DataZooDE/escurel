# Escurel v1 — implementation spec

**Status:** v1 specification. The contract is locked in
[`../contract/agent-interface.md`](../contract/agent-interface.md);
this directory specifies the implementation.

## What this is

The agent ↔ KB contract (see
[`../contract/agent-interface.md`](../contract/agent-interface.md))
defines the surface: skill-instance markdown, typed `[[skill::id]]`
wikilinks, twelve MCP tools, a mandatory `escurel` meta-skill, live CRDT
write path plus whole-page fallback. The storage shape behind the
dispatcher is per-tenant: a single DuckDB file using the `vss` and
`fts` extensions, with the canonical `pages/` markdown directory on
disk as the source of truth. The architectural decision behind this
single-store layout is in
[`../adr/0001-duckdb-only-storage.md`](../adr/0001-duckdb-only-storage.md);
its pre-deployment empirical gate is the load-bearing item before any
production rollout.

The contract treats the **time axis** as two sub-axes — an immutable
*event log* (instances of event-typed skills like `meeting`, `email`,
`call`, `message`, `incident`, each with an `at:` field) and *state
at time T* (append-only chains via `prev_*`, supersession via
`supersedes:`, snapshot pinning via `[[skill::id@version]]`). Events
are ordinary instances; no new storage or wire shape is required, but
`list_instances` carries a sort key (`order_by`) and a date-range
filter syntax so the typical event-log call (`list_instances('meeting',
filter={at: '>= 2026-04-01'}, order_by='at desc')`) is expressible
cheaply. See [`protocol.md`](protocol.md) for the exact signatures and
[`storage.md`](storage.md) for the event-index sketch.

Conceptually escurel models memory as a **triad — Events · Skills ·
Instances** — bound by skills: an instance's current state **is the
projection of its event sequence, mediated by the skills** that describe
how to process each event (the durable "how"). v1 realises the Skills and
Instances legs and the event log *via the existing instance primitives
above*; the dedicated **Event** leg (a real `events`/inbox store, each
event's `label_skill` pointing at its processing skill, historical
`expand(as_of=T)`, an outbound webhook, and the external-agent fold) lands
in **M7 — Event-sourcing surface** (see
[`roadmap.md § M7`](roadmap.md)), which deliberately extends this v1
contract. The `escurel-explore` workspace renders the triad as **two
views of one memory**: the *event view* (the events that produced a
memory, plus the general inbox) on the left and the *instance view* (the
materialized state, its skill connections, and its state over time) on the
right.

This directory specifies the *implementation*: a single Rust binary
(`escurel-server`) and a thin CLI client (`escurel`), exposing the
agent surface over two transports (MCP-over-HTTP and WebSocket). HTTP
is the sole transport; the operator surface is exposed as admin-role-
gated MCP tools on the same `/mcp` endpoint. It captures every
decision that needed to be locked before code can be written.

## Document map

| Doc | What it covers |
|---|---|
| [`README.md`](README.md) (this file) | TL;DR, locked decisions, architecture, crate layout |
| [`protocol.md`](protocol.md) | Wire protocols (MCP/HTTP, WS); full tool schemas and admin tools |
| [`storage.md`](storage.md) | Per-tenant FS layout, single DuckDB store (relational + `vss` + `fts` + CRDT op log), markdown source of truth, S3 driver, audit/rebuild |
| [`platform.md`](platform.md) | OIDC auth, tenancy resolution, admin & lifecycle API, quotas, observability |
| [`roadmap.md`](roadmap.md) | Milestones, v1 cut-line, deferred items, license audit recap |
| [`dx.md`](dx.md) | Downstream-app integration contract: `EscurelProcess`, `escurel-client`, `AuthMode`, fixture seeding, the `escurel → app → triton → frontend` chaining recipe |

Read this README first for shape; the four siblings are the
detail one layer down. Cross-references from this file to
`protocol.md §X.Y` etc. point at section anchors in those files.

## Locked design decisions

Each row links back to the question that produced it. **Bold** rows
are the ones with the largest blast radius if changed later.

| # | Topic | Decision | Notes |
|---|---|---|---|
| **1** | **Language** | **Rust** (stable, edition 2024) | Per-tenant single-writer fits a Tokio runtime cleanly |
| **2** | **Deployment** | **Single binary `escurel-server`, self-hosted; same binary scales to multi-tenant SaaS** | One process per node; multi-tenancy is in-process (see decision 8) |
| 3 | Auth | Generic OIDC discovery; tenant id is one OIDC claim (`tenant` by default, configurable) | See [`platform.md`](platform.md#auth) |
| 4 | Embedding model — default | **EmbeddingGemma** (`google/embeddinggemma-300m`), 768d, Matryoshka-trained, multilingual | Open weights; loads on first start, cached under `${ESCUREL_DATA_DIR}/cache/models/` |
| 4a | Embedding model — optional | Gemini embeddings (`gemini-embedding-001`) over HTTPS, gated by `embedding.provider = gemini` | Bypasses candle; breaks air-gapped use; only enabled if explicitly configured |
| 5 | Embed/rerank runtime | **candle** (pure Rust) | No external runtime; CUDA/Metal feature flags; sidecar adapter exists as a trait impl for future use |
| **6** | **Transports** | **MCP-over-HTTP** + **WebSocket** (live mode); HTTP is the sole transport | See [`protocol.md`](protocol.md) for each |
| 7 | Storage backend | **Local FS for dev; S3 LaneStore is the production backend.** S3-compatible stores supported via `object_store::aws` (verified: AWS S3, MinIO, Hetzner Object Storage); FS retained as a dev-only convenience | DuckDB supports object-store URLs via `httpfs`, DuckLake natively; markdown ships through the same trait |
| **8** | **CRDT state** | **Lives in the server**. Single source of truth for each open page. Web client and agent both connect to the same server. | See [`storage.md`](storage.md#crdt-persistence) |
| 9 | CLI shape | **Thin MCP client over HTTP**, same auth as agents | Operator-only commands call the admin-role-gated MCP tools; the CLI carries both |
| 10 | Admin/operator surface | **Admin-role-gated MCP tools (no separate service)** | The operator capabilities are MCP tools on `/mcp`; they require an `admin` role on the OIDC token (missing role → JSON-RPC `-32001`) |
| 11 | Execution model | **Single-process Tokio + per-tenant async write lock**; background jobs in a Tokio task pool | One DuckDB writer per tenant at a time (single-file lock); reads concurrent; jobs share the runtime |
| 12 | Tenant lifecycle | **Explicit admin API + export/import**; no auto-provision | Admin creates tenants before first agent call; export = tarball (markdown + lane snapshot + manifest) |
| 13 | Observability | **OpenTelemetry traces + metrics (OTLP)**, **JSON logs to stdout**, **`/metrics` Prometheus fallback** | See [`platform.md`](platform.md#observability) |
| 14 | Quotas (v1) | **Query rate limit**, **write + embed rate limit**, **concurrent sessions cap** | All three are token-bucket per tenant; defaults configurable, overridable per tenant |
| 15 | Page IDs | **ULID** is canonical; **mutable slug** stored as metadata for human-friendly URLs | Wikilinks reference ULID at storage layer; slug → ULID resolved at parse time |
| 16 | Deployment target binding | The core spec is target-agnostic; concrete bindings for any specific runtime live in [`../deploy/`](../deploy/). v1 ships [`../deploy/substrate.md`](../deploy/substrate.md) for the `DataZooDE/hetzner-agent-substrate` target | Names the OIDC issuer source, S3 LaneStore config, audit/backup shippers, placement-group sizing, golden-image content, Tailscale tags, and ingress proxy (Fabio) for that target. New deployment targets get their own sibling doc; the core spec stays unchanged. |

**Two intentional extensions beyond a strict tool-surface-only contract.** Decisions 6 (transports) and 10 (admin/operator surface) broaden the agent contract: live mode benefits from WebSocket, while the admin/operator capabilities are exposed as additional MCP tools on the same `/mcp` endpoint, gated by an OIDC `admin` role claim. The CLI becomes a thin client over the same surface rather than a separate channel. The contract — twelve agent tools, no direct SQL, no raw vector access, no cross-tenant operations — is preserved.

Two decisions remain *deliberately* deferred to implementation,
both about transport efficiency:

- Whether `apply_op` over MCP/HTTP is request/response per op or
  bidirectional streaming on a long-lived call. v1 ships
  request/response over HTTP and bidirectional streaming over
  WS (WS is the recommended path for live mode anyway).
- Whether viewer-awareness shares the WS channel with the CRDT op
  stream or uses a separate WS path. v1 puts both on `/ws` with a
  message-type discriminator; can split later if needed.

## Architecture in one diagram

```
                       ┌─────────────────────────────┐
                       │   Identity Provider (OIDC)  │
                       │   any standard issuer       │
                       └──────────────┬──────────────┘
                                      │ id-token w/ tenant claim
                                      ▼
  Agent (Claude/etc) ──┐                                   ┌── Web client
                       │       streamable HTTP +           │
                       │          WebSocket               │
                       ▼                                   ▼
                ┌──────────────────────────────────────────────────────┐
                │                escurel-server (Rust)                 │
                │  ┌──────────────────────────────────────────────┐    │
                │  │   gateway: axum (HTTP/WS)                    │    │
                │  └─────────────────────┬────────────────────────┘    │
                │                        │                             │
                │  ┌─────────────────────▼────────────────────────┐    │
                │  │   tool dispatcher (one handler per tool)      │    │
                │  │   auth → tenant resolution → quota → handler  │    │
                │  └──┬───────┬────────────┬──────────┬───────────┘    │
                │     │       │            │          │                │
                │  ┌──▼──┐ ┌──▼──┐  ┌──────▼──┐  ┌────▼────┐           │
                │  │read │ │write│  │  crdt   │  │  admin  │           │
                │  │path │ │path │  │ sessions│  │ surface │           │
                │  └──┬──┘ └──┬──┘  └────┬────┘  └────┬────┘           │
                │     │       │          │            │                │
                │  ┌──▼───────▼──────────▼────────────▼─────┐          │
                │  │     tenant manager (per-tenant lock)     │          │
                │  │     loads/closes TenantHandle on demand  │          │
                │  └──────────────────┬───────────────────────┘         │
                │                     │                                 │
                │  ┌──────────────────▼──────────────────┐              │
                │  │            TenantHandle             │              │
                │  │     ┌──────────────┐ ┌──────────┐   │              │
                │  │     │escurel.duckdb│ │ markdown │   │              │
                │  │     │ vss+fts+rel+ │ │  store   │   │              │
                │  │     │   crdt_ops   │ │ (pages/) │   │              │
                │  │     └──────┬───────┘ └────┬─────┘   │              │
                │  │            │              │         │              │
                │  │  ┌─────────▼──────────────▼──────┐  │              │
                │  │  │       LaneStore trait         │  │              │
                │  │  │   (local FS │ S3 backend)     │  │              │
                │  │  └───────────────┬───────────────┘  │              │
                │  └──────────────────┼──────────────────┘              │
                │                     │                                 │
                │  ┌──────────────────▼──────────────────┐              │
                │  │   embed/rerank workers (candle)     │              │
                │  │   one queue per tenant, shared pool │              │
                │  └─────────────────────────────────────┘              │
                │                                                       │
                │  observability: OTLP exporter, /metrics, JSON logs    │
                └──────────────────────────────────────────────────────┘
                                      │
                                      ▼
                            Local filesystem
                               OR S3 bucket
                            (LaneStore trait)
```

### Process model

One OS process, one Tokio runtime, one global state struct
(`Server`). Multi-tenancy is in-process: `TenantManager` keeps
a bounded LRU of `TenantHandle`s (default 64), each holding
an open DuckDB connection pool plus an in-memory `LiveDoc`
map for that tenant's currently-open CRDT sessions.

Concurrency rules per [`platform.md`](platform.md#concurrency):

- **Reads** (`search`, `resolve`, `expand`, `neighbours`,
  `list_skills`, `list_instances`, `run_stored_query`): no
  lock, separate DuckDB read connection. Fully concurrent
  within DuckDB's MVCC.
- **Writes** (`apply_op`, `close_session(commit=true)`,
  `update_page`): acquire the per-tenant `RwLock` write side.
  Serial within a tenant; parallel across tenants.
- **Background jobs** (audit, rebuild, embedding swap, vacuum):
  also under the write lock — they touch the same store the
  foreground write path does.

The per-tenant lock is `tokio::sync::RwLock`. Hold times are
bounded because every write is a single DuckDB transaction
(`Appender.flush()` plus the markdown file write that follows
a successful commit). Background jobs run in shorter slices
when possible (chunked audit; per-page rebuild) to keep tail
latency on writes acceptable.

### Failure model

- **Crash mid-write.** The DuckDB transaction is the atomicity
  primitive: a mid-write SIGKILL rolls the transaction back so
  pages, links, blocks (with `vss`/`fts` index updates) and
  `crdt_ops` all commit together or not at all. Markdown writes
  are write-then-rename and run only after a successful DuckDB
  commit. On restart, `audit` detects any drift; `rebuild`
  recovers from canonical markdown.
- **Embedding model load fail.** Server starts in degraded
  mode: read path works (existing vectors served), `update_page`
  and `apply_op` reject with `embedding_unavailable` until the
  model loads. Manual `escurel-server reload-embedding` retries.
- **S3 backend unavailable.** Server keeps a local write-ahead
  copy in `${ESCUREL_DATA_DIR}/spool/<tenant>/`. Writes queue;
  reads fall back to the last cached lane snapshot. Quotas
  apply normally. The spool dir is **host-local** and never
  synced to the LaneStore. On a Nomad reschedule to a new host,
  queued spool entries are lost; the markdown source-of-truth
  on the LaneStore is preserved (writes only enter the spool
  after a successful DuckDB commit per the crash-recovery
  matrix), so recovery is a client re-submit. Substrate
  deployments must size embed throughput and S3 connectivity
  so steady-state spool depth stays bounded.
- **Per-tenant DuckDB corruption.** Tenant is auto-suspended
  (`status: suspended_corrupt`); admin must run `escurel-admin
  rebuild --tenant <id>` to recover from canonical markdown.
  Other tenants are unaffected.

## Crate layout

Workspace at the repo's `escurel/` directory (not yet created — this
dir is the *spec* for that). Proposed layout:

```
escurel/
├── Cargo.toml                 # workspace
├── crates/
│   ├── escurel-server/        # binary, gateway, dispatcher, tenant manager
│   ├── escurel-cli/           # binary, thin MCP client (agent + admin tools)
│   ├── escurel-storage/       # LaneStore trait, FS impl, S3 impl
│   ├── escurel-index/         # DuckDB lane logic (relational + vss + fts); indexer; audit/rebuild
│   ├── escurel-md/            # markdown parser, wikilink parser, frontmatter
│   ├── escurel-crdt/          # Loro adapter, LiveDoc, session manager (persistence via escurel-index crdt_ops table)
│   ├── escurel-embed/         # candle EmbeddingGemma; Gemini API adapter;
│   │                          # trait abstraction (Embedder, Reranker)
│   ├── escurel-auth/          # OIDC verification, tenant resolution
│   ├── escurel-quota/         # token-bucket limiter
│   └── escurel-obs/           # OTel + JSON log layer + /metrics
├── docs/                      # operator docs (separate from this spec)
└── examples/
```

Why a workspace: the CLI links the same MCP client used by
integration tests; the indexer is testable in isolation. Plus,
separate crates make it easier to swap backends behind their traits
without touching the gateway code.

Crates that have non-trivial external deps:

- `escurel-storage`: `object_store` (Apache-2.0)
- `escurel-index`: `duckdb` (MIT) with the `vss` and `fts` extensions enabled
- `escurel-crdt`: `loro` (MIT)
- `escurel-embed`: `candle-core` + `candle-nn` + `candle-transformers`
  (MIT/Apache-2.0); `reqwest` for Gemini adapter
- `escurel-server`: `axum` (MIT), `tokio` (MIT)
- `escurel-auth`: `jsonwebtoken` (MIT), `reqwest`, `serde_json`
- `escurel-obs`: `opentelemetry-otlp`, `tracing-subscriber`,
  `tracing-opentelemetry`, `prometheus`
- `escurel-cli`: `clap`, the MCP client from `escurel-client`

The full license audit is in [`roadmap.md`](roadmap.md#licenses).
All deps are MIT or Apache-2.0; no GPL surface.

## Configuration

One TOML file, default location `${ESCUREL_CONFIG:-/etc/escurel/server.toml}`.
Environment variable overrides are `ESCUREL_<UPPER_SNAKE>` for any
field. Example:

```toml
[server]
data_dir = "/var/lib/escurel"
listen_http = "0.0.0.0:8080"

[auth]
oidc_issuer = "https://auth.example.com/realms/main"
oidc_audience = "escurel"
tenant_claim = "tenant"
admin_role_claim = "roles"
admin_role_value = "escurel:admin"

[storage]
backend = "fs"                # "fs" or "s3"
# [storage.s3]
# bucket = "escurel-data"
# region = "eu-central-1"
# prefix = "tenants/"

[embedding]
provider = "embeddinggemma"   # or "gemini" or "sidecar"
model = "google/embeddinggemma-300m"
device = "cpu"                # "cpu", "cuda:0", "metal"
dim = 768                     # Matryoshka truncation; 768|512|256|128
# [embedding.gemini]
# api_key_env = "GEMINI_API_KEY"
# model = "gemini-embedding-001"

[quota.defaults]
queries_per_minute = 600
writes_per_minute = 120
embeds_per_minute = 300
concurrent_sessions = 32

[observability]
otlp_endpoint = "http://otel-collector:4317"
metrics_listen = "0.0.0.0:9090"   # /metrics for Prometheus
log_format = "json"               # "json" or "text"
```

Environment variable overrides follow `ESCUREL_<UPPER_SNAKE>`
derived from the TOML key path (e.g. `[server] data_dir` →
`ESCUREL_SERVER_DATA_DIR`). Substrate Nomad jobspecs pin the
sizing knobs explicitly so capacity planning is one place:

| env var | TOML | default | what it bounds |
|---|---|---|---|
| `ESCUREL_TENANT_LRU_CAP` | `[concurrency] tenant_lru_cap` | 64 | TenantHandle LRU; idle eviction after 5 min |
| `ESCUREL_DUCKDB_READ_POOL` | `[concurrency] duckdb_read_pool` | 16 | per-tenant DuckDB read connections |
| `ESCUREL_EMBED_POOL` | `[concurrency] embed_pool` | 32 | per-tenant in-flight embed tasks |
| `ESCUREL_WRITE_LOCK_TIMEOUT_MS` | `[concurrency] write_lock_timeout_ms` | 5000 | per-tenant write-lock acquisition timeout |
| `ESCUREL_SPOOL_FLUSH_INTERVAL_MS` | `[storage] spool_flush_interval_ms` | 1000 | S3 spool flush cadence when LaneStore is reachable |

Tenant-specific overrides live in the tenant manifest (see
[`platform.md`](platform.md#tenant-lifecycle)); they can lift or
lower any quota and override the embedding provider but cannot
change storage backend or auth.

## What v1 ships

The cut line for v1 (the binary you can run in production):

- All 12 MCP tools from
  [`../contract/agent-interface.md`](../contract/agent-interface.md)
- Live CRDT mode + whole-page fallback over MCP/HTTP and WebSocket
- **S3 LaneStore is the production default** (Hetzner Object
  Storage is the reference substrate target); local FS retained
  as a dev-only convenience
- EmbeddingGemma in candle (CPU); CUDA/Metal behind feature
  flags
- Three quota dimensions enforced
- Admin API: tenant CRUD + export/import + rebuild + audit +
  attach_external
- OTel + JSON logs + `/metrics`
- One client: `escurel` CLI (operator + agent-style usage)
- One mandatory in-corpus skill: `escurel` meta-skill, shipped with
  every new tenant
- Downstream-app integration contract ([`dx.md`](dx.md)): `escurel-client`
  + `escurel-test-support` (`EscurelProcess`, `AuthMode::TestIssuer`,
  `FixtureBuilder`, `McpTestClient`) so new applications can wire
  escurel into their integration tests without copying plumbing out of
  this repo's crate tests

What's explicitly **not** v1 (see [`roadmap.md`](roadmap.md)):

- Federation across tenants
- Live cursors; v1 has presence badges only
- Sidecar embedding adapter (Ollama/TEI/vLLM)
- Web admin UI (CLI only)
- Reranker beyond a small CE head bundled with EmbeddingGemma
- Auto-provision-on-first-request flow

## Where this design rests

The evidence that bears directly on the Rust shape:

| concern | source |
|---|---|
| Per-tenant single writer is sufficient | DuckDB single-writer evaluation |
| DuckDB transaction as atomicity primitive | DuckDB durability model (vendored) |
| Block retrieval beats page for long pages | block-vs-page retrieval evaluation |
| Retrieval quality target ≥ 0.95 nDCG | [`../adr/0001-duckdb-only-storage.md`](../adr/0001-duckdb-only-storage.md) pre-deployment gate |
| Loro meets rich-text + tree CRDT needs | Loro evaluation |
| FTS quality requires synonym-stress tuning | tracked as part of the consolidation gate |
| Markdown AST fragments on `[` — use regex | wikilink-parser evaluation |
| Tier-1 token budget invariant 10²→10⁶ | memory-budget verification |
| Real LLM finds the right policy from descriptions | real-LLM run |

## Status

This directory is `spec`, not `code`. Once accepted, the
implementation begins at `escurel-storage` + `escurel-md` + `escurel-index`
and builds outward to the gateway.
