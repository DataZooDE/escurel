# Protocol — MCP/HTTP, WebSocket, gRPC

All three transports expose the same logical surface — the 12
agent tools and the admin endpoints. They differ only in framing,
streaming model, and which is the recommended default. The tool
*semantics* are the contract in
[`../contract/agent-interface.md`](../contract/agent-interface.md);
this doc specifies the *wire shapes*.

## Transport summary

| transport | mount | what it carries | streaming | default for |
|---|---|---|---|---|
| MCP-over-HTTP | `/mcp` | JSON-RPC 2.0 framed as MCP method calls; one HTTP request per call; long-running calls use SSE for streaming responses | server→client SSE | MCP clients (Claude.ai today; most other MCP clients) |
| WebSocket | `/ws` | Bidirectional. Used for live CRDT op streams, presence pings, and search-result streaming | full-duplex | live mode, web client |
| gRPC | `:8081` (separate listener) | Strongly-typed Protobuf; full mirror of MCP surface plus admin endpoints | client-streaming, server-streaming, bidi as appropriate | CLI, dashboards, programmatic admin |

Auth is the same on all three (OIDC Bearer in `Authorization`
header; see [`platform.md`](platform.md#auth)). Tenant resolution
is the same (one tenant per token claim). Quotas apply uniformly.

**Default exposure.** MCP/HTTP and WebSocket are designed for
ingress behind a reverse proxy + authentication terminator
(the proxy may also terminate TLS). gRPC is designed for
internal callers — it carries no rate-limit middleware in v1
and is **not** intended for direct public LB exposure;
operator/CLI traffic reaches it over the internal network.
The choice of which transports a particular deployment exposes
is per-target; see
[`../deploy/substrate.md §8`](../deploy/substrate.md#8--ingress-fabio)
for the substrate-target binding (MCP+WS via Fabio `urlprefix-`
tags; gRPC tailnet-only with no `urlprefix-` tag).

## Shared types

These are referenced from every tool. JSON Schema for MCP;
Protobuf for gRPC.

### `PageRef`

```ts
{
  page_id: string,        // ULID, canonical id
  slug:    string | null, // mutable human-friendly slug
  skill:   string,        // the skill id this page declares or is an instance of
  page_type: "skill" | "instance"
}
```

### `Hit`

```ts
{
  page_id: string,
  slug:    string | null,
  skill:   string,
  page_type: "skill" | "instance",
  anchor?: string,        // only for granularity=block
  snippet: string,
  score:   number,        // RRF-fused
  frontmatter_excerpt: { [key: string]: any }   // includes description and at if present
}
```

### `WikilinkParsed`

```ts
{
  skill:   string | null,  // null for bare [[id]]
  id:      string | null,  // null for bare [[skill]]
  anchor:  string | null,
  version: string | null,
  alias:   string | null
}
```

### `Issue` (the `validate` / `apply_op` / `update_page` shared shape)

```ts
{
  severity: "error" | "warning",
  code:     string,        // e.g. "unknown_skill", "anchor_missing", "frontmatter_required_key_missing"
  location: string,        // e.g. "line:32 col:8" or "frontmatter.at"
  message:  string,
  suggestion?: string
}
```

### `FilterClause` (used by `list_instances`)

```ts
type FilterValue =
  | string | number | boolean | null
  | { ">=" : any }   // operator-wrapped value
  | { "<=" : any }
  | { ">"  : any }
  | { "<"  : any }
  | { "in" : any[] }
  | { "not": any }   // value-level negation

type Filter = { [frontmatter_key: string]: FilterValue }
```

This filter syntax is intentionally minimal — agents express
date-range queries (`{at: {">=" : "2026-04-01"}}`),
enumerations (`{status: {in: ["open", "in_review"]}}`), and
null-checks (`{prev_review: null}`) without learning SQL. The
dispatcher translates the filter to a DuckDB prepared statement;
unknown keys, unknown operators, or type-mismatched values
return `Issue` rows rather than dispatching.

## Agent surface

Twelve tools, grouped by axis. Inputs and outputs given as JSON
Schema; gRPC translates to messages with the obvious mapping.

### Read tools

#### `search`

```jsonc
// request
{
  "q": "Acme renewal risk",
  "k": 10,
  "granularity": "block",            // "block" | "page", default "block"
  "page_type": "any",                // "skill" | "instance" | "any"
  "skill": "customer",               // optional filter; pushes link_skill predicate to DuckDB
  "filter": { "at": { ">=": "2026-04-01" } }  // optional frontmatter filter
                                              // (events use this to time-window)
}
// response
{ "hits": [Hit, ...], "granularity": "block" }
```

`filter` is the same shape `list_instances` accepts. It is
applied **after** vector + FTS retrieval as a metadata
post-filter, so it doesn't degrade recall; only the response is
narrowed. Useful for "find recent meetings about Acme":
`search(q='Acme', skill='meeting', filter={at: {">=":
"2026-04-01"}})`.

#### `resolve`

```jsonc
// request
{ "wikilink": "[[customer::acme-corp]]" }
// response
{
  "parsed": WikilinkParsed,
  "page": PageRef,         // or null if not found
  "exists": true,
  "error": null            // or { code, message } if validation failed
}
```

Returns the parsed link plus the resolved page metadata.
`resolve` does NOT fetch the body — that's `expand`.

#### `expand`

```jsonc
// request
{ "page_id": "01HXMQ...", "anchor": null, "version": null, "as_of": null }
// response
{
  "page": PageRef,
  "frontmatter": { ... },         // full frontmatter
  "body":   "...markdown body...",
  "blocks": [ { "anchor": "blk-acme-signals", "content": "..." }, ... ],
  "wikilinks_out": [ WikilinkParsed, ... ],
  "snapshot_version": "v14"        // populated only when version/as_of replayed a snapshot
}
```

**Historical state (M7).** `as_of = T` (RFC 3339) reconstructs the
instance **as it was at T**: when the page has a CRDT snapshot taken
at-or-before T, that snapshot is materialized + re-parsed (so the
returned `frontmatter`/`body` are the historical values — the
projection of its events up to T). A page with no snapshot history at-
or-before T falls through to the `at_ts` birth filter (returns the page
when born, `null` when not). This extends the v1 rule that markdown
instances ignore `@version` silently — markdown instances with a
seeded snapshot history now honour the time cut.

- **`list_snapshots`** *(read)* — `{page_id}` → `{snapshots: [taken_at,
  …]}`, the RFC-3339 timestamps of an instance's snapshot history,
  oldest first. These are the discrete points `expand(as_of=T)` can
  replay — the "state over time" version markers in the instance view.

For events: `expand` returns the full body of an event instance
including any narrative text and follow-up links. Anchor support
is the same as any other instance.

#### `neighbours`

```jsonc
// request
{
  "page_id":  "01HXMQ...",
  "direction": "both",             // "in" | "out" | "both"
  "link_skill": null,              // single skill filter
  "link_skill_in": ["meeting", "email", "call"],   // optional multi-skill filter
  "order_by": "at desc",           // optional; orders by resolved target's frontmatter field
  "limit": 100
}
// response
{
  "edges": [
    {
      "src": PageRef,
      "dst": PageRef,
      "link_skill": "meeting",
      "link_version": null,
      "anchor": null,
      "src_anchor": "blk-acme-signals",
      "target_frontmatter_excerpt": { "at": "2026-04-12T10:00:00+02:00" }
    },
    ...
  ]
}
```

`link_skill_in` is the multi-skill array form used for event
timelines (`neighbours(acme, link_skill_in=[meeting, email,
call, incident])`). `order_by` is a `<field> <asc|desc>` string;
the field must be a top-level frontmatter key of the *target*
page. The dispatcher pushes it down to DuckDB as
`ORDER BY json_extract_string(target_frontmatter, '$.at') DESC`
(materialised into a typed column at index time for the common
case of `at`).

#### `list_skills`

```jsonc
// request: {}
// response
{
  "skills": [
    {
      "id": "customer",
      "description": "...",
      "required_frontmatter": ["tier", "opened", "status"],
      "optional_frontmatter": ["mrr_band", "owner", ...],
      "is_event_typed": false       // true iff `at` is in required_frontmatter
    },
    {
      "id": "meeting",
      "description": "An in-person or remote meeting...",
      "required_frontmatter": ["at", "modality", "with", "participants"],
      "optional_frontmatter": ["location", "duration_minutes", ...],
      "is_event_typed": true
    },
    ...
  ]
}
```

`is_event_typed` is a derived convenience flag (true iff `at` is
in `required_frontmatter`); the agent does not need to compute
it from the field list. The flag is also exposed on
`list_skills` rows in the gRPC mirror.

#### `list_instances`

```jsonc
// request
{
  "skill_id": "meeting",
  "filter": {
    "at":   { ">=": "2026-04-01" },
    "with": "[[customer::acme-corp]]"
  },
  "order_by": "at desc",            // optional
  "limit":    50
}
// response
{
  "instances": [
    { "page_id": "01HX...", "slug": "2026-04-12-acme-qbr",
      "frontmatter": { "at": "...", "with": "...", ... } },
    ...
  ],
  "next_cursor": null               // present when truncated
}
```

This is the event-log primitive. The `filter` honours typed
wikilink values (matches the string form OR a parsed link target);
strings compared as strings; dates compared lexically (ISO 8601
preserves ordering); arrays support `in`.

#### `run_stored_query`

```jsonc
// request
{
  "query_id": "customer-churn-trend",
  "params":   { "customer_id": "acme-corp", "from_date": "2026-01-01" },
  "as_of_snapshot": null            // optional; pins to a specific DuckLake snapshot
}
// response
{
  "rows":   [ ... ],
  "schema": [ { "name": "as_of", "type": "DATE" }, ... ],
  "snapshot_version": "v27"
}
```

Unchanged from the agent-interface design. For event-volume
queries that exceed the markdown-friendly scale (~1 M),
operators move the event records to an external DuckLake table
and the agent reaches them through `run_stored_query` instead
of `list_instances`. See [`storage.md`](storage.md#event-volume--scaling-beyond-1-m-events).

### Write tools

#### `validate`

```jsonc
// request
{ "content": "---\nskill: meeting\n...\n---\n# ...", "as_page_id": null }
// response
{ "issues": [Issue, ...] }
```

#### `open_session` / `apply_op` / `close_session` (live CRDT)

```jsonc
// open_session request:  { "page_id": "01HX..." }
// open_session response: { "session": "sess_...", "head_version": "v42",
//                          "content": "...", "ws_url": "wss://.../ws?session=sess_..." }
//
// apply_op request:  { "session": "sess_...", "op": <Loro op blob> }
// apply_op response: { "ok": true, "merged_version": "v43",
//                      "content": "...", "conflicts": [], "issues": [] }
//
// close_session request:  { "session": "sess_...", "commit": true }
// close_session response: { "ok": true, "final_version": "v50", "issues": [] }
```

The `ws_url` returned by `open_session` is the recommended
channel for `apply_op` — the WS path delivers ops with lower
overhead than HTTP. MCP-over-HTTP clients without WS may
continue calling `apply_op` over HTTP; gRPC clients use the
bidirectional stream `LiveSession`.

#### `update_page` (whole-page fallback)

```jsonc
// request
{
  "page_id":     "01HX...",
  "content":     "---\n...\n---\n# ...",
  "base_version": "v42"            // optional; required only if the client knows it
}
// response
{
  "ok":          true,
  "new_version": "v43",
  "issues":      [Issue, ...]
}
```

If `base_version` is supplied and the head has advanced, the
server attempts a CRDT-aware three-way merge (Loro). On
unresolvable conflict, the response is `{ok: false,
issues: [{code: "conflict", ...}], head_content: "..."}` —
the client re-drafts against `head_content`.

### Events / inbox (M7 — Event-sourcing surface)

Events are the dynamic input of the memory triad (Events · Skills ·
Instances). They live in a dedicated `events` store (not pages); each
event's `label_skill` links to the skill that knows how to process it,
and `instance_page_id` links to the instance it belongs to once
processed. The inbox is the `status = 'inbox'` view. All four tools are
mirrored on native gRPC (`Escurel.CaptureEvent` → `Event`, `ListInbox`,
`ListEvents`, `AssignEvent`) with the same quota debits as MCP
(`capture_event`/`assign_event` = Writes; `list_inbox`/`list_events` =
Queries).

- **`capture_event`** *(write)* — append an event to the inbox.
  Input: `{event_id?, at?, source?, mime?, label_skill?,
  instance_page_id?, title?, body?, provenance?}` (`event_id` is a
  server ULID when absent; `instance_page_id` only *pre-flags* a
  candidate — the event stays in the inbox until `assign_event`).
  Returns the stored event (`{event_id, at, status: "inbox", …}`).
- **`list_inbox`** *(read)* — `{limit?}` → `{events: [Event, …]}`,
  unprocessed events, newest first.
- **`list_events`** *(read)* — `{instance_page_id, limit?}` →
  `{events: [Event, …]}`, that instance's processed event history,
  oldest first (the sequence whose projection is its state).
- **`assign_event`** *(write)* — `{event_id, instance_page_id}` →
  marks the event `processed` and bound to the instance. This is the
  (external) agent's act of folding the event into state.

An `Event` is `{event_id, at, source, mime, label_skill,
instance_page_id, status, title, body, provenance}`.

**Capture webhook (opt-in).** When `ESCUREL_WEBHOOK_URL` is set, each
`capture_event` fires a **fire-and-forget** HTTP `POST` of the stored
event's JSON to that URL — the notification an external processing agent
subscribes to. Delivery never blocks or fails the capture (a down sink is
logged and dropped); the agent may also poll `list_inbox`, so a missed
POST self-heals. The fold event→state remains the external agent's job
(via `assign_event` + `update_page`); the server stays automation-free.

## Admin surface

Exposed on gRPC and on MCP/HTTP under `/mcp/admin`. Requires the
admin role on the OIDC token (configurable; see
[`platform.md`](platform.md#auth)). Tenant resolution rules are
*different* on admin endpoints — the tenant is named explicitly
in each call rather than taken from the token's `tenant` claim
(an admin operates across tenants).

| endpoint | inputs | outputs | purpose |
|---|---|---|---|
| `tenant_create` | `{id, display_name, quotas?}` | `{ok, tenant: {...}}` | provision a new tenant |
| `tenant_list` | `{}` | `{tenants: [{id, status, created_at, ...}]}` | enumerate tenants |
| `tenant_get` | `{id}` | `{tenant: {id, status, quotas, ...}}` | fetch one |
| `tenant_update` | `{id, quotas?, status?, embedding_provider?}` | `{ok, tenant}` | suspend, resume, change quotas |
| `tenant_delete` | `{id, confirm: string}` | `{ok}` | hard-delete (15-location wipe) |
| `tenant_export` | `{id}` | streaming bytes (tarball: markdown + lane snapshot + manifest) | export |
| `tenant_import` | streaming bytes + `{id, overwrite: bool}` | `{ok, tenant}` | restore |
| `audit` | `{tenant, scope?}` | `{drift: {markdown_not_in_duckdb: [...], indexed_but_no_markdown: [...]}}` | drift detection (two-way diff) |
| `rebuild` | `{tenant, scope?}` | streaming progress events | recover from canonical markdown |
| `attach_external` | `{tenant, catalog_uri, name}` | `{ok}` | wire a DuckLake catalog into `external.ducklake` |
| `embedding_reload` | `{tenant?}` | `{ok}` | retry model load after a degraded start |
| `compact_db` | `{tenant}` | streaming progress | force a DuckDB `CHECKPOINT` + `VACUUM` + `PRAGMA hnsw_compact_index` |
| `quota_get` | `{tenant}` | `{quotas: {...}, current_usage: {...}}` | inspect |
| `health` | `{}` | `{ok, version, embedding_status, storage_backend, tenant_count, ...}` | liveness + summary |

The `audit` and `rebuild` endpoints are the operational
recovery path. The cost is ~32 ms/page; a 1000-page tenant
rebuilds in ~32 s.

### `tenant_export` as the backup-contract producer

`tenant_export` is the **only** backup hook exposed by
`escurel-server`. The server never writes to a backup bucket
itself; external backup orchestrators (e.g. the substrate's
tenant-export shipper named in
[`../deploy/substrate.md §4`](../deploy/substrate.md#4--backup-shipper-contract))
call this endpoint on a schedule and ship the bytes to a
durable target.

Contract relied on by orchestrators:

- **Read-only**. Does not hold the tenant write lock;
  concurrent exports are allowed and foreground writes
  proceed normally during export.
- **Read-snapshots**. Exports may include a small lag (one
  write transaction worst case) relative to the latest
  committed write.
- **Deterministic tarball**. Per the
  [`storage.md` per-tenant layout](storage.md#per-tenant-directory-layout)
  minus `cache/` and `spool/`.
- **Streaming framing**. Chunks of `TenantExportChunk` over
  gRPC; SSE `event: chunk` / `event: done` over MCP/HTTP. The
  terminator carries the export-format version and a SHA-256
  hash of the concatenated body bytes; consumers verify before
  treating the tarball as durable.
- **Failures mid-stream**. Surface as gRPC `INTERNAL` / SSE
  `event: error` with `retryable: bool`. Retryable errors
  invite the consumer to restart from offset 0; non-retryable
  errors indicate corruption and require operator intervention.
- **Idempotency is the consumer's responsibility**. Re-running
  an export produces a new tarball with potentially different
  bytes (if writes happened in between); consumers key
  snapshots by `{tenant_id, started_at}`, not by content hash.

## gRPC service definition

Sketch (full `.proto` lives in `crates/escurel-proto/`):

```proto
service Escurel {
  // agent surface (same as MCP)
  rpc Search       (SearchRequest)        returns (SearchResponse);
  rpc Resolve      (ResolveRequest)       returns (ResolveResponse);
  rpc Expand       (ExpandRequest)        returns (ExpandResponse);
  rpc Neighbours   (NeighboursRequest)    returns (NeighboursResponse);
  rpc ListSkills   (ListSkillsRequest)    returns (ListSkillsResponse);
  rpc ListInstances(ListInstancesRequest) returns (ListInstancesResponse);
  rpc RunStoredQuery(RunStoredQueryRequest) returns (RunStoredQueryResponse);
  rpc Validate     (ValidateRequest)      returns (ValidateResponse);
  rpc UpdatePage   (UpdatePageRequest)    returns (UpdatePageResponse);

  // live session (bidi stream)
  rpc LiveSession  (stream LiveOp)        returns (stream LiveAck);
}

service EscurelAdmin {
  rpc TenantCreate  (TenantCreateRequest)  returns (TenantCreateResponse);
  rpc TenantList    (TenantListRequest)    returns (TenantListResponse);
  rpc TenantGet     (TenantGetRequest)     returns (TenantGetResponse);
  rpc TenantUpdate  (TenantUpdateRequest)  returns (TenantUpdateResponse);
  rpc TenantDelete  (TenantDeleteRequest)  returns (TenantDeleteResponse);
  rpc TenantExport  (TenantExportRequest)  returns (stream TenantExportChunk);
  rpc TenantImport  (stream TenantImportChunk) returns (TenantImportResponse);
  rpc Audit         (AuditRequest)         returns (AuditResponse);
  rpc Rebuild       (RebuildRequest)       returns (stream RebuildProgress);
  rpc AttachExternal(AttachExternalRequest)returns (AttachExternalResponse);
  rpc EmbeddingReload(EmbeddingReloadRequest) returns (EmbeddingReloadResponse);
  rpc CompactLanes  (CompactLanesRequest)  returns (stream CompactProgress);
  rpc QuotaGet      (QuotaGetRequest)      returns (QuotaGetResponse);
  rpc Health        (HealthRequest)        returns (HealthResponse);
}
```

Authentication metadata: every RPC carries an
`authorization: Bearer <jwt>` header. `EscurelAdmin` methods require
the admin role claim.

## MCP-over-HTTP framing

Standard JSON-RPC 2.0 envelope wrapping each tool call.
Tool-name mapping is:

```
search           → method = "tools/call", name = "search"
resolve          → method = "tools/call", name = "resolve"
...
update_page      → method = "tools/call", name = "update_page"
```

Tool discovery is the usual MCP `tools/list` response, declaring
all 12 tools with their JSON Schema input definitions. Admin
endpoints are surfaced as a second `tools/list` group only when
the token carries the admin role; otherwise they are not even
listed.

Streaming responses (search results, rebuild progress) use SSE
with `event: chunk` for incremental data and `event: done` for
the terminator.

## WebSocket framing

Single endpoint, `/ws`. Connection auth in the upgrade request
(`Authorization: Bearer ...`). Once connected, the client sends a
hello frame:

```jsonc
{ "type": "hello", "session": "sess_xyz" }    // attaches to an open CRDT session
// or
{ "type": "hello", "presence_only": true }    // presence + search subscriptions only
```

Message types:

| `type` | direction | payload |
|---|---|---|
| `op` | C→S | `{ session, op: <Loro op> }` |
| `op_ack` | S→C | `{ session, merged_version, content, conflicts, issues }` |
| `presence` | bidi | `{ session, user, anchor }` (heartbeat every 10 s) |
| `search_subscribe` | C→S | `{ subscription_id, q, k, filter? }` — live-updated search |
| `search_event` | S→C | `{ subscription_id, hits: [...] }` |
| `close` | C→S | `{ session, commit: bool }` |
| `error` | S→C | `{ code, message }` |

Live search subscriptions are the WS-only feature where the
server pushes new hits as new pages are indexed (useful for
agents watching for new events in a stream). Not v1; placeholder
in the schema, off behind a feature flag.

## Versioning

The MCP `serverInfo.version`, gRPC service version (`escurel.v1`), and
WebSocket protocol version (`escurel-ws/1`) all start at `1`. Breaking
changes bump to `escurel.v2` / `escurel-ws/2`; the server can serve both
versions for one major-version overlap.

JSON Schema for MCP and Protobuf for gRPC live in `escurel-proto/` and
ship as crates so client implementations can pin to a version.
