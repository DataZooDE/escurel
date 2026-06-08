# `/mcp` wasn't a spec-complete MCP server — real agents couldn't attach

**Symptom.** The agent-runner's `claude`/`codex`/`adk` harnesses could not
actually drive escurel. Two distinct failures, found only when a real `claude`
CLI was first pointed at a real gateway:

1. Claude Code **could not connect** to the escurel MCP server at all (it got
   zero `mcp__escurel__*` tools), so — spawned in a repo cwd — it wandered the
   local filesystem looking for the event and refused to fold.
2. After connecting, Claude **read every tool result as empty** — `list_skills`
   returned 14 skills but the model reported "0 skills."

**Cause.** `crates/escurel-server/src/mcp.rs` implemented only `tools/list` and
`tools/call`, returning the raw tool payload as the JSON-RPC `result`. That is
enough for escurel's *own* typed client (`escurel-client`, which only ever calls
those two methods), but it is **not** the MCP Streamable-HTTP contract a real
MCP client speaks:

- A client first POSTs `initialize` and expects an `InitializeResult`
  (`protocolVersion`/`capabilities`/`serverInfo`), then POSTs a
  `notifications/initialized` notification (no `id`). escurel answered
  `initialize` with `-32601 method not found` and **failed to even deserialize**
  a notification (the `id` field was required) → the client marks the server
  failed → no tools.
- A `tools/call` result must be a `CallToolResult`:
  `{content:[{type:"text",text:…}], structuredContent?:…, isError?:bool}`.
  escurel returned the bare payload, so the client saw no `content` → empty.

**Fix** (PR #160). On `/mcp`: add the `initialize` handshake (echo the client's
`protocolVersion`, advertise `tools`), `#[serde(default)]` on the request `id`
so notifications parse, 202-and-empty-body any `notifications/*`, a `ping` →
`{}`, and wrap **`tools/call` success** results as
`{content:[{type:"text",text:<payload-as-json>}], structuredContent:<payload>, isError:false}`
(only `tools/call`; `initialize`/`ping`/`tools/list` stay raw per spec).
`escurel-client` now reads `result.structuredContent` (falling back to `result`),
so the runner/CLI/TUI are unaffected. Verified with a real `claude` fold
end-to-end and the `claude_live` test (finally run, green).

**How to recognise it next time.** If an MCP client (Claude Code, the MCP
Inspector, Codex) reports the escurel server as failed/empty, probe the
lifecycle directly:

```bash
curl -s $GW/mcp -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}'
# must return result.serverInfo + result.capabilities.tools — NOT -32601
curl -s $GW/mcp -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_skills","arguments":{}}}'
# result must have a `content` array, not a bare {skills:[…]}
```

`crates/escurel-server/tests/mcp_lifecycle.rs` is the always-on regression
guard (full lifecycle over real HTTP).

**Why it shipped undetected (the process lesson).** The agent-harness epic
(#144) closed the adapter sub-issues with deterministic tests that used **stub
scripts** in place of the real `claude`/`codex` CLI — a test double at exactly
the boundary the adapter exists to cover — and a real live-LLM test that was
`#[ignore]`'d and **never actually run**. So the only test that would have
exercised a real MCP client against the gateway never executed. Lesson, now
binding: `#[ignore]` ≠ done; a stub at the boundary under test ≠ no-mock; a
cross-component contract ("`/mcp` is an MCP server") needs a real-client
conformance test that actually runs. See the runbook
[`examples/agent-runner/README.md`](../../../examples/agent-runner/README.md).
