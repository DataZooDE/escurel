# Escurel documentation

This is the v1 specification of the Escurel knowledge-base service.
Read it in the order below.

## Reading order

1. **[`contract/agent-interface.md`](contract/agent-interface.md)** —
   the agent ↔ KB contract: twelve MCP tools, the mandatory `escurel`
   meta-skill, and the behavioural promises both sides depend on. The
   spec under `spec/` is the implementation of this contract.
2. **[`spec/README.md`](spec/README.md)** — architecture overview,
   locked design decisions, crate layout, configuration, the v1
   cut-line.
3. **[`spec/protocol.md`](spec/protocol.md)** — wire protocols
   (MCP-over-HTTP, WebSocket), full tool schemas, admin
   endpoints.
4. **[`spec/storage.md`](spec/storage.md)** — per-tenant filesystem
   layout, the single DuckDB store with `vss` + `fts` + CRDT op log,
   markdown as the source of truth, S3 driver, audit / rebuild.
5. **[`spec/platform.md`](spec/platform.md)** — OIDC auth, tenancy
   resolution, admin & lifecycle API, quotas, observability.
6. **[`spec/roadmap.md`](spec/roadmap.md)** — milestones, v1 cut-line,
   deferred items, dependency license audit.
7. **[`spec/dx.md`](spec/dx.md)** — downstream-app integration contract.
   Read this if you are *consuming* escurel from another application and
   need to wire it (plus, optionally, triton) into your test harness.
8. **[`adr/0001-duckdb-only-storage.md`](adr/0001-duckdb-only-storage.md)** —
   the single architectural decision the v1 storage shape rests on,
   plus the pre-deployment empirical gate that must pass before any
   production rollout.
9. **[`deploy/substrate.md`](deploy/substrate.md)** — deployment
   binding for the DataZoo Hetzner agent substrate; paired with
   **[`deploy/escurel.nomad.hcl`](deploy/escurel.nomad.hcl)** as a
   concrete Nomad jobspec.

## What this is not

- Not the Rust implementation. The implementation will land alongside
  the spec in this repo; today there is no code.
- Not a tutorial. This is the spec a Rust implementer or a substrate
  operator works from, not an end-user manual.
