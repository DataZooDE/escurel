# Tracing `span.enter()` is unsound inside an async fn

**Date:** 2026-05-25
**Symptom:** A per-request `tracing::info_span!(...)` attached with
`let _g = span.enter();` followed by an `.await` looks correct but
silently leaks the span guard into whatever task the runtime
schedules next on that worker thread. The leaked span's fields
(e.g. `request_id`) then appear on completely unrelated records —
including hyper / h2 trace lines from a different connection.

The `tracing` crate's own docs flag this: span guards are
thread-local, but `.await` can move the future to another worker
between polls. The next time the worker thread picks up an
unrelated task, that task inherits the still-entered span until
the guard drops.

**Where we hit it:** `crates/escurel-server/src/mcp.rs` —
`pub async fn mcp(...)` originally wrapped the dispatcher with

```rust
let span = tracing::info_span!("mcp.request", request_id = %id, ...);
let _entered = span.enter();
// many .await calls follow…
```

Tests passed because they checked only that *at least one* line
carried the `request_id`. Under load, span fields would have
bled across requests.

**Fix:** instrument an inner async block / fn with `.instrument(span)`
from `tracing::Instrument`. The span is attached to the future
itself, not to the worker thread, so it follows the poll across
await points without leaking sideways.

```rust
use tracing::Instrument;

let span = tracing::info_span!("mcp.request", request_id = %id, ...);
mcp_inner(state, headers, req).instrument(span).await
```

**How to recognise it next time:** if a log record carries a
`request_id` that doesn't match its `target` (e.g. an h2 frame
trace stamped with an MCP request id), span guards are leaking
across `.await` points. Search the codebase for
`span.enter()` inside `async fn` bodies.
