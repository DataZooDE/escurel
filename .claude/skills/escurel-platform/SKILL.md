---
name: escurel-platform
version: 0.4.0
description: Use when building an application that consumes the Escurel knowledge-base service as its data store — designing a tenant's skill/instance data model, calling the agent tools (search/resolve/expand/neighbours/list_skills/list_instances/query_instance/validate/open_session/apply_op/close_session/update_page/append_message/list_messages, plus the event bus capture_event/assign_event/list_events/list_inbox), or wiring escurel into an app backend and its integration tests. Covers the published surfaces (MCP-over-HTTP `/mcp`, the `escurel` CLI), the Rust `escurel-client` + `escurel-test-support` path, fixture seeding, auth/tenancy, per-chat-group conversation history (append_message/list_messages + admin DeleteChatHistory), external instance backends (read-only sql_view over an attached source; document/PDF/DOCX uploaded via /ingest), and the no-mock dev loop. Triggers on phrases like "use escurel from my app", "escurel MCP tool", "resolve a wikilink", "query_instance", "query page", "capture_event", "event bus", "escurel webhook", "seed an escurel tenant", "escurel-client", "EscurelProcess test", "author a skill page", "escurel CLI", "FixtureBuilder", "chat history", "append_message", "list_messages", "delete chat history", "instance backend", "sql_view instance", "document backend", "ingest a PDF", "/ingest". DO NOT use for escurel-internal work (the indexer, LaneStore, the markdown parser, the dispatcher, the embedder) — that is a PR against the escurel repo itself, not consumer-facing.
---

# escurel-platform — build apps that consume Escurel

You are helping someone build an **application that uses Escurel as its
data store**. Escurel is a per-tenant knowledge-base service (a single
Rust workspace in this repo: spec under `docs/`, implementation under
`crates/`). This skill is the **consumer-facing contract** for building
on top of it.

An Escurel tenant holds typed markdown pages — **skills** (type
declarations) and **instances** (memories of a type) — connected by
typed `[[wikilinks]]`. Your application reaches that content through a
small, stable tool surface. There is exactly one mental model: *find the
typed page or block I need.* Same primitives across the kind, time, and
origin axes.

Instances are native markdown by default, but a skill may declare an
**external instance backend** so its instances live elsewhere — a read-only
`sql_view` over an attached relational source, or a `document` (PDF/DOCX/text
uploaded via `/ingest`, extracted and chunked). They still read like ordinary
pages (just read-only). → `references/01` §Backend axis, `references/02`
§Instance backends.

## Two ways your app consumes Escurel

Escurel is reached through its **published surfaces** — your app does not
link Escurel's internals:

1. **Over the wire (language-agnostic).** Any runtime (Python, TS, Go, …)
   speaks **MCP-over-HTTP** at `POST /mcp` (JSON-RPC 2.0) on `:8080`.
   → `references/03`.
2. **Via the `escurel` CLI.** A thin MCP-over-HTTP client binary with one
   subcommand per agent tool, JSON on stdout — ideal for shells, scripts,
   and non-Rust apps that prefer to shell out. → `references/04`.

For **Rust** apps there is a typed path on top of (1): the
**`escurel-client`** crate (typed MCP-over-HTTP client) for the backend, and
**`escurel-test-support`** (`EscurelProcess`, `FixtureBuilder`) for
no-mock integration tests. → `references/05`, `references/06`.

This skill is **read-only documentation**. It makes no live calls, holds
no credentials, and runs no operator commands. Anything that requires
changing Escurel itself (a new tool, a proto field, indexer behaviour) is
a **PR against this repo**, not a workaround in your app — see
`references/10-out-of-bounds.md`.

## How this skill is installed

The Escurel repo is checked out locally and this skill directory is
**symlinked** into the consumer repo's `.claude/skills/`:

```sh
# in the consumer repo root, with the escurel repo checked out somewhere
ln -s ../path/to/escurel/.claude/skills/escurel-platform \
      .claude/skills/escurel-platform
```

References point at `docs/…`, `crates/…`, and `examples/…` **relative to
the Escurel repo root** — they resolve through the symlink. The Escurel
checkout's git ref is the version pin; check `VERSION` / `CHANGELOG.md`.

## Progressive-disclosure index

Read only what the task needs. Each reference is small and self-contained;
it **navigates to** the canonical spec in `docs/` and the source in
`crates/` rather than restating it.

| File | Read when… |
|---|---|
| `references/00-what-is-escurel.md` | First contact. The skill/instance model, the published surfaces, where your app sits. |
| `references/01-data-model.md` | Designing *your* tenant. Skills, instances, frontmatter, wikilinks, the kind/time/origin axes, the mandatory `escurel` meta-skill. |
| `references/02-tool-surface.md` | The agent tools at a glance: reads, writes, chat, the EVENT BUS (capture/assign/list_events/list_inbox + the HMAC webhook), and the anti-patterns. |
| `references/03-consume-over-http-mcp.md` | Consuming from any language over `POST /mcp` (JSON-RPC). Envelope, auth, per-tool shapes. |
| `references/04-consume-via-cli.md` | Driving Escurel from a shell or non-Rust app with the `escurel` CLI. |
| `references/05-consume-from-rust.md` | A Rust backend. `escurel-client`: `Client::connect`, the typed methods, request/response fields. |
| `references/06-integration-tests.md` | The no-mock dev loop. `EscurelProcess` + `FixtureBuilder` (Rust); endpoint/CLI driving (non-Rust); red→green TDD. |
| `references/07-fixtures-and-seeding.md` | Authoring seed pages, the public-write-path guarantee, the three seeding routes. |
| `references/08-auth-and-tenancy.md` | Bearer tokens, tenants, agent vs admin role, test issuer vs real OIDC. |
| `references/09-local-iteration.md` | Getting a gateway to develop against, the routes, the env vars, the iterate loop. |
| `references/10-out-of-bounds.md` | Before reaching for anything that belongs *inside* Escurel. Hard prohibitions + cross-refs. |

## Hard prohibitions

- **No Escurel internals in your app.** Don't depend on `escurel-server`,
  `Indexer`, `LaneStore`, `OidcVerifier`, the markdown parser, or DuckDB
  from your application binary. The leaf dependency is `escurel-client`
  (Rust) or the wire/CLI surface (anything else).
- **No raw SQL.** You reach the relational/external store only through
  `query_instance`, which dispatches to a `[[query::*]]` page authored
  ahead of time. Author the query page first; never interpolate SQL.
- **No side-dooring the indexer.** Seed only through the public
  `update_page` write path (or `FixtureBuilder`, which uses it). What you
  seed must be what production would seed.
- **No cross-tenant calls.** Each server instance is scoped to one tenant;
  federation is a separate, future layer.

See `references/10-out-of-bounds.md` for the full list and the escalation
path. Cross-references: `triton-platform` (the
`escurel → app-backend → triton → app-frontend` chaining recipe) and
`substrate-platform` (deploying your app + Escurel on the substrate).
