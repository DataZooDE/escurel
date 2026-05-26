# Changelog

All notable changes to escurel are recorded here. The format is
loosely [Keep a Changelog](https://keepachangelog.com/); escurel
follows SemVer from v1.0.0 onward.

## [1.0.0] — 2026-05-26

First stable release. The v1 cut-line in
[`docs/spec/roadmap.md`](docs/spec/roadmap.md) is met.

### Agent surface (14 tools, on MCP-over-HTTP + gRPC)

- Read: `search` (hybrid vector + FTS, RRF-fused), `resolve`,
  `expand`, `neighbours`, `list_skills`, `list_instances`,
  `run_stored_query`, `validate`.
- Write: `update_page`.
- Live CRDT: `open_session` / `apply_op` / `close_session` over
  HTTP, the gRPC `LiveSession` bidi stream, and the WebSocket `/ws`
  attach path (Loro engine, per-page `LiveDoc` actor, op-log +
  snapshot persistence, two-stage external-edit reconciler).
- Chat history: `append_message` / `list_messages` (per-chat-group
  conversation log).

### Admin surface (gRPC `EscurelAdmin`, admin-role gated)

Tenant CRUD, streaming export/import, audit, streaming rebuild,
`attach_external` (read-only external catalog), `embedding_reload`
(degraded-start recovery), `compact_lanes` (subsumed op compaction),
`quota_get`, and an auth-free `health`.

### Storage & retrieval

- DuckDB-only per-tenant store (vss + fts extensions); HNSW dense
  vectors + FTS, fused with Reciprocal Rank Fusion.
- LaneStore trait with **S3 (Hetzner Object Storage) as the
  production default** and a local-FS dev backend.
- Crash-recovery: mid-write transaction rollback; cattle-node-loss →
  automatic rebuild-from-markdown on boot.

### Embeddings

EmbeddingGemma in candle (CPU default) behind a reloadable seam;
Gemini as an optional hosted provider; a zero/hash embedder for the
dev loop.

### Transports, auth, quotas

axum HTTP gateway (MCP/JSON-RPC framing + `/ws`), tonic gRPC mirror,
OIDC JWT verification with JWKS caching, token-bucket quotas across
three dimensions (queries, writes+embeds, concurrent sessions).

### Operability

- 12-factor `escurel-server` binary: `ESCUREL_*` config (over TOML),
  ports 8080/8081, graceful SIGTERM, degraded-start.
- OpenTelemetry traces + Prometheus `/metrics` + structured JSON
  logs with `request_id`.
- Substrate deployment artefacts: Nomad jobspec set, Packer
  golden-image fragment, tenant-export shipper periodic job,
  Tailscale ACL fragment, and a three-target deploy guide.
- `cargo deny` license + advisory audit; `Cargo.lock` committed.

### Developer experience

- `escurel-client` typed RPC wrapper (leaf crate — no server deps).
- `escurel-test-support`: `EscurelProcess` + `AuthMode::TestIssuer`
  + `FixtureBuilder` + `McpTestClient` — spawn escurel in a
  downstream app's tests without re-deriving the JWKS/RSA harness.
- `escurel` CLI; `examples/echo-app` demonstrating the chaining
  recipe.

### Engineering process

Built red→green TDD with no-mock integration tests as the merge
gate. GitHub Actions CI (paused during bootstrap) is **re-enabled**
at this release for every push to main and every PR.
