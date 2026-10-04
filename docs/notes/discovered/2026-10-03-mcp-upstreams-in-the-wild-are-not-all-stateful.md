# MCP upstreams: be strict about what you send, lenient about what you accept

**Symptom.** An MCP client that insists on the full 2025-06-18 streamable-HTTP dance fails against
perfectly usable servers: some answer `initialize` with `-32601` (a legacy, stateless server), some reply
to a `tools/call` with `text/event-stream` and some with `application/json`, some drop the session and
answer `404` mid-conversation.

**Fix** (`remote_backend.rs`): send `initialize`, then `notifications/initialized`, then carry
`Mcp-Session-Id` and `MCP-Protocol-Version: 2025-06-18`; accept either a JSON body or an SSE stream for a
response (take the first `data:` frame that carries our request id); a `404` means the session expired,
so re-initialise ONCE and retry; `-32601` on `initialize` means a legacy server, so call it without a
session. Anything else is a failure with a sanitized message (never the upstream's body or URL).

**Verified against** two independent servers in `tests/suite/`: a spec-faithful test server
(`mcp_upstream.rs`, with expiry and an injection payload) and a second, real escurel gateway used as an
upstream. Don't test an MCP client against only a server you wrote to match it.
