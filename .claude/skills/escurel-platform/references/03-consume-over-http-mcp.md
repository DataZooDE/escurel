# 03 — Consume over HTTP (MCP) or gRPC

The language-agnostic path. Any runtime (Python, TS, Go, …) talks to a
tenant over **MCP-over-HTTP** or native **gRPC** — same tool surface
(`references/02`). Canonical wire spec: `docs/spec/protocol.md`
(§MCP-over-HTTP framing, §gRPC service definition, §Shared types).

## MCP-over-HTTP (`POST /mcp`)

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

- **Discovery:** `tools/list` returns all fourteen agent tools with their
  JSON Schema input definitions (the twelve KB tools plus `append_message`
  and `list_messages` for chat history — `references/02` §Chat tools).
  Admin tools appear as a second group **only** when the token carries the
  admin role (otherwise invisible — `references/08`).
- **Errors:** JSON-RPC error envelope (`error: {code, message}`). Tool-level
  validation issues come back inside `result` (the issue list in
  `references/02`), not as a transport error.
- **Streaming:** large/streamed responses (search, rebuild progress) use
  SSE — `event: chunk` for incremental data, `event: done` to terminate.
- **Auth:** `Authorization: Bearer <token>` on every call (`references/08`).
  Argument names match `protocol.md` exactly; note the gRPC field names
  differ slightly from the contract's prose (e.g. `q`/`k` not
  `query`/`top_k`) — trust `protocol.md` and the `.proto`.

A minimal client is just an HTTP client that POSTs that envelope and reads
`result`. If your runtime has an MCP SDK, point it at `/mcp` and call the
tools by name. For an agent harness, this is the surface the in-tenant
`escurel` meta-skill (`references/01`) describes to the model.

## gRPC (`escurel.v1.Escurel` on `:8081`)

The same surface as protobuf RPCs. The service and message definitions
are the source of truth at `crates/escurel-proto/proto/escurel.proto`;
generate stubs for your language from that file. Auth is the same bearer,
carried as an `authorization: Bearer <token>` request metadata header.

Each RPC takes a `<Tool>Request` and returns a `<Tool>Response`. Selected
message fields (see the `.proto` for the complete set):

```protobuf
message SearchRequest  { string q = 1; uint32 k = 2; string granularity = 3;
                         string page_type = 4; string skill = 5; }
message SearchHit      { string page_id = 1; string slug = 2; string skill = 3;
                         string page_type = 4; string anchor = 5; string snippet = 6;
                         double score = 7; string frontmatter_excerpt_json = 8; }
message ResolveRequest { string wikilink = 1; }
message ResolveResponse{ WikilinkParsed parsed = 1; PageRef page = 2; bool exists = 3; }
message ExpandRequest  { string page_id = 1; string anchor = 2; string version = 3; }
message UpdatePageRequest { string page_id = 1; string content = 2; }
```

JSON-bearing fields (`frontmatter_json`, `rows_json`, `params_json`) carry
a JSON string you parse client-side — the wire keeps them opaque so the
schema doesn't churn per tool.

## Which transport?

- **HTTP/MCP** — smallest dependency footprint; works from anything that
  can POST JSON; the natural choice for agent harnesses and non-Rust apps.
- **gRPC** — typed stubs, streaming, lower per-call overhead; what the
  Rust `escurel-client` (`references/05`) rides on.

Both are committed and equivalent in surface. For a Rust backend, prefer
`escurel-client`. For everything else, HTTP/MCP or the CLI
(`references/04`) is the least-friction path.
