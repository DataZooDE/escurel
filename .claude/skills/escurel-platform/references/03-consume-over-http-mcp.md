# 03 — Consume over HTTP (MCP)

The language-agnostic path. Any runtime (Python, TS, Go, …) talks to a
tenant over **MCP-over-HTTP** — the tool surface from `references/02`.
Canonical wire spec: `docs/spec/protocol.md`
(§MCP-over-HTTP framing, §Shared types).

## MCP-over-HTTP (`POST /mcp` on `:8080`)

Standard **JSON-RPC 2.0** envelope; each tool call is `tools/call`:

```jsonc
// → POST /mcp   (Authorization: Bearer <token>)
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "search",
    "arguments": { "q": "acme churn", "k": 5, "page_type": "instance" }
  }
}
```

```jsonc
// ← 200 OK
{ "jsonrpc": "2.0", "id": 1, "result": { "hits": [ … ], "granularity": "block" } }
```

- **Discovery:** `tools/list` is **role-scoped**. Every entry carries a
  `scope: "agent" | "admin"` label; an agent-role token receives only
  the `scope: "agent"` subset (~28 tools — the ones it can actually
  call), while an admin token sees the whole surface (~69). Calling an
  admin tool without the role is still refused at dispatch (`-32001`).
- **Errors:** JSON-RPC error envelope (`error: {code, message}`). Tool-level
  validation issues come back inside `result` (the issue list in
  `references/02`), not as a transport error.
- **Streaming:** there is none — no SSE, no chunking, no `GET /mcp` event
  stream. Every response is a single JSON body; large blobs come back
  base64 in `fetch_blob`, capped at 25 MiB. Poll, or use the WS
  `event_subscribe` push (`references/11`) for event-driven wake-ups.
- **Auth:** `Authorization: Bearer <token>` on every call (`references/08`).
  Argument names match `protocol.md` exactly; note the wire field names
  differ slightly from the contract's prose (e.g. `q`/`k` not
  `query`/`top_k`) — trust `protocol.md`.

JSON-bearing fields (`frontmatter`, `rows`, `params`) are **real JSON
objects/arrays on the wire**, not encoded strings. (Early versions carried
`frontmatter_json`-style string fields; that era is over — nothing needs a
second parse.)

A minimal client is just an HTTP client that POSTs that envelope and reads
`result`. If your runtime has an MCP SDK, point it at `/mcp` and call the
tools by name. For an agent harness, this is the surface the in-tenant
`escurel` meta-skill (`references/01`) describes to the model.

## Which surface?

- **HTTP/MCP** — smallest dependency footprint; works from anything that
  can POST JSON; the natural choice for agent harnesses and non-Rust apps.
  It is the sole wire transport; the Rust `escurel-client`
  (`references/05`) is a typed wrapper over it.

For a Rust backend, prefer `escurel-client`. For everything else, HTTP/MCP
or the CLI (`references/04`) is the least-friction path.
