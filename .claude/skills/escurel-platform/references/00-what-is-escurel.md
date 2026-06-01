# 00 — What is Escurel

Escurel is a **per-tenant knowledge-base service**. One tenant = one
isolated knowledge base, backed by a single DuckDB store (vector search +
full-text + a CRDT op log) with canonical markdown as the source of
truth. Your application is a **consumer**: it reads and writes a tenant's
pages through a small tool surface and never touches the storage engine.

Canonical reading: `docs/spec/README.md` (architecture + locked
decisions) and `docs/contract/agent-interface.md` (the agent ↔ KB
contract this skill is the consumer view of).

## The one mental model: skills and instances

Every page in a tenant is one of two kinds:

- **Skill** — a *type declaration*. It names a conceptual entity
  (`customer`, `meeting`, `decision-record`, …) and declares what its
  instances must look like (`required_frontmatter`, `optional_frontmatter`).
- **Instance** — a *memory of that type*. It cites its skill via
  `skill: <id>` in frontmatter and links to other pages with typed
  `[[skill::id]]` wikilinks.

There is no second model for external data or for time. A
`[[table::sales]]` (external structured data) and a `[[meeting::qbr]]`
(an event in time) are reached with the **same** primitives as a
`[[customer::acme]]`. The dispatcher hides which store backs which page.
This is the "one referent space" principle (`agent-interface.md`
§Design principles). `references/01` covers the model in depth.

## The published surfaces

A consuming app reaches a tenant through one of these — all carry the
**same** tool surface (`docs/spec/protocol.md` §Transport summary):

| Surface | Endpoint | Use from |
|---|---|---|
| MCP-over-HTTP | `POST /mcp` (JSON-RPC 2.0), `:8080` | any language; agent harnesses; typed clients (the Rust `escurel-client` rides this); → `references/03`, `05` |
| WebSocket | `/ws`, `:8080` | live CRDT co-editing + presence; → `references/02` |
| CLI | the `escurel` binary (a thin MCP-over-HTTP client) | shells, scripts, non-Rust apps; → `references/04` |

Operational routes also exist: `/healthz`, `/readyz`, `/version`,
`/metrics` (→ `references/09`).

## Where your app sits

Two common shapes:

- **Backend-as-consumer.** Your service holds a connection (or a CLI
  invocation, or an HTTP client) to the tenant and answers product
  requests by resolving/expanding/searching pages. The reference example
  `examples/echo-app/` is exactly this: `GET /pages/{slug}` →
  `resolve([[customer::{slug}]])` → `expand(page_id)` → markdown body.
- **Agent-as-consumer.** An LLM agent in your app uses the tool surface
  directly (over MCP). The tenant ships a mandatory `escurel` meta-skill
  page that teaches the agent the disclosure model at runtime — distinct
  from *this* skill, which teaches *you* (the developer) how to build the
  app. See `references/01` §The mandatory meta-skill.

Either way, the contract is the tool surface in `references/02`. Start
there if you already know the data model; start at `references/01` if you
are designing a tenant from scratch.
