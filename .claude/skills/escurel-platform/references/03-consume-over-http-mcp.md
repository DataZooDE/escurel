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
    "arguments": { "q": "acme churn", "k": 5, "page_kind": "instance" }
  }
}
```

```jsonc
// ← 200 OK
{ "jsonrpc": "2.0", "id": 1, "result": {
    "content": [ { "type": "text", "text": "10 hits for \"acme churn\" …" } ],   // short human summary
    "structuredContent": { "hits": [ … ], "granularity": "block" },               // the typed payload
    "isError": false } }
```

Read **`result.structuredContent`**: it is the tool's typed payload; `content[0].text` is only a short
summary for text-only clients. A refusal answers `isError: true` with `structuredContent.issues[]`
(`code`, `message`, `suggestion?`) — never an empty success.

- **Discovery:** `tools/list` is **role-scoped**. Every entry carries a
  `scope: "agent" | "admin"` label; an agent-role token receives only
  the `scope: "agent"` subset (44 tools — the ones it can actually
  call), while an admin token sees the whole surface (86). Calling an
  admin tool without the role is still refused at dispatch (`-32001`).
- **Errors:** JSON-RPC error envelope
  (`error: {code, message, data?}`). Branch on `error.data.code`
  (stable strings: `admin_required`, `unknown_session`,
  `event_not_found`, `already_assigned`, `read_only_replica`,
  `quota_exhausted`, …) and honour `error.data.retryable` — never parse
  `message` wording.
- **A refused tool call is `isError: true`, not a transport error — check it before you read anything.**
  A domain refusal (an ACL denial, `invalid_limit`, `field_not_filterable`, `query_not_found`,
  `endpoint_not_registered`, a write that fails validation, …) is a normal `result`:
  `{content: [<short summary>], structuredContent: {ok: false, issues: [{severity, code, location,
  message, suggestion?}]}, isError: true}`. **Never read `structuredContent` fields blindly.** On a
  refusal they are NOT the tool's answer, and a client that decodes the payload into its response type
  gets an EMPTY SUCCESS (no instances, no rows) that looks exactly like "there is nothing there": a
  silent partial read after a denial. Open every `tools/call` result in one place: if `isError` (or
  `ok === false`) → raise the `issues`; else use `structuredContent`. The write tools (`update_page`,
  `create_draft`, `promote_*`, …) and `validate` put `ok`/`issues` in their own typed answer, so for
  those `ok: false` IS the result to branch on. The Rust client raises `Error::Refused` for a refused
  read and returns the typed answer for those; the VS Code client throws `EscurelError` (`refused`,
  `forbidden`, …); the Dart client throws `EscurelToolException`; the CLI prints the issue and exits
  non-zero. The Rust helpers live in `escurel_types::call_result`.
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
`result.structuredContent`. If your runtime has an MCP SDK, point it at `/mcp` and call the
tools by name (stock SDKs' `listTools()` work since skill 0.19.0; against an older gateway the
`execution` string on each tool fails the SDK's validation — hand-roll the POST there). For an agent harness, this is the surface the in-tenant
`escurel` meta-skill (`references/01`) describes to the model.

## Which surface?

- **HTTP/MCP** — smallest dependency footprint; works from anything that
  can POST JSON; the natural choice for agent harnesses and non-Rust apps.
  It is the sole wire transport; the Rust `escurel-client`
  (`references/05`) is a typed wrapper over it.

For a Rust backend, prefer `escurel-client`. For everything else, HTTP/MCP
or the CLI (`references/04`) is the least-friction path.
