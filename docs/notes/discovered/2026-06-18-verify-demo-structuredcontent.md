# verify-demo.sh in-page probes must read `result.structuredContent`

**Date:** 2026-06-18
**Area:** `scripts/verify-demo.sh` (in-page `/mcp` behaviour probes)

## Symptom

`scripts/verify-demo.sh` failed at the **first** behaviour probe —
`FAIL: no engagement instances seeded (got '')` — even though the server
was healthy and `list_instances` returned the seeded instances correctly
via a direct `curl` to `/mcp`. Every presence assertion (the rodney
`flt-semantics[aria-label=…]` checks) passed; only the `mcp_int`
behaviour probes broke, and they returned an **empty string** (not
`ERR`), which looked like a rodney/CanvasKit async-fetch flake.

## Cause

A `tools/call` reply is an MCP **`CallToolResult`** —
`{ content, isError, structuredContent }` — not the tool's bare JSON.
The tool's own payload (e.g. `{instances: […]}`) lives under
`structuredContent`. The probe helper bound `r = j.result` and evaluated
`r.instances.length`; `r.instances` was `undefined`, so `undefined.length`
**threw**, the `(async()=>…)()` IIFE rejected, and rodney surfaced the
rejected promise as an empty string. Consistent, not flaky.

`curl` "worked" only because the manual check parsed
`result.content[0].text`; the script's extract path was simply wrong for
the wrapped shape.

## Fix

Bind the probe payload to `structuredContent`, falling back to the raw
result for any non-wrapped reply:

```js
const r = j.result.structuredContent || j.result;
```

## How to recognise it next time

If a verify-demo behaviour probe returns `''` (empty) — not `ERR`, not a
number — while the same tool works over `curl`, the extract is reading the
wrong level of the `CallToolResult`. Confirm with one rodney call:
`Object.keys(j.result)` → `content,isError,structuredContent`. The tool
payload is under `structuredContent`.
