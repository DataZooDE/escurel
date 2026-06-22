# Protocol — MCP/HTTP, WebSocket

HTTP is the sole transport surface. Both transports expose the same
logical surface — the 12 agent tools and the admin endpoints. They
differ only in framing and streaming model. The tool
*semantics* are the contract in
[`../contract/agent-interface.md`](../contract/agent-interface.md);
this doc specifies the *wire shapes*.

## Transport summary

| transport | mount | what it carries | streaming | default for |
|---|---|---|---|---|
| MCP-over-HTTP | `/mcp` | JSON-RPC 2.0 framed as MCP method calls; one HTTP request per call; long-running calls block until done and return the final result | none (blocking) | agents, MCP clients, CLI/TUI, admin/operator tools |
| WebSocket | `/ws` | Bidirectional. Used for live CRDT op streams, presence pings, and search-result streaming | full-duplex | live mode, web client |

Auth is the same on both (OIDC Bearer in `Authorization`
header; see [`platform.md`](platform.md#auth)). Tenant resolution
is the same (one tenant per token claim). Quotas apply uniformly.

**Default exposure.** MCP/HTTP and WebSocket are designed for
ingress behind a reverse proxy + authentication terminator
(the proxy may also terminate TLS). The choice of which transports
a particular deployment exposes is per-target; see
[`../deploy/substrate.md §5`](../deploy/substrate.md#5--exposure)
for the substrate-target binding (typically internal/tailnet-only
via kamal-proxy, declared in `apps/registry.yml`).

## Shared types

These are referenced from every tool, expressed as JSON Schema.

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
Schema.

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
  "snapshot_version": "v14",       // populated only when version/as_of replayed a snapshot
  // external-backend instances only (see Instance backends):
  "backend_projection": null,      // sql_view: { view, rows, source, truncated, issue? }
  "chunks_total": null,            // document: total chunk count
  "chunks_truncated": false        // document: blocks are a bounded lead of chunks_total
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
      "is_event_typed": false,      // true iff `at` is in required_frontmatter
      "backend": { "kind": "markdown" },
      "capabilities": { "writable": true, "granularity": "block",
                        "search": "hybrid", "supports_crdt": true }
    },
    {
      "id": "erp_customer",
      "description": "...",
      "required_frontmatter": [...],
      "optional_frontmatter": [...],
      "is_event_typed": false,
      "backend": { "kind": "sql_view" },        // or "document"
      "capabilities": { "writable": false, "granularity": "page",
                        "search": "late_materialized", "supports_crdt": false }
    },
    ...
  ]
}
```

`is_event_typed` is a derived convenience flag (true iff `at` is
in `required_frontmatter`); the agent does not need to compute
it from the field list.

`backend.kind` (`markdown` | `sql_view` | `document`) and the
`capabilities` object tell the agent *where* a skill's instances live and
what may be done with them — see [Instance backends](#instance-backends).
A skill that declares no `backend:` block is `markdown` with the writable,
block-grain, CRDT capabilities above (the historical default, so existing
clients are unaffected).

##### Per-instance access control (`visibility` / `owner_field`)

A skill page MAY declare a read policy for its instances:

```yaml
visibility: owner        # public | owner   (default: public)
owner_field: credential  # frontmatter field naming the owning principal
```

- `visibility: public` (the default, and the only behaviour before this
  field existed) — any authenticated caller in the tenant may read an
  instance of this skill.
- `visibility: owner` — an instance is readable only by its **owning
  principal** or the **admin role**. The owner is the verified token
  `sub` that equals the instance's `owner_field` value: either a direct
  value (e.g. `credential` carries the platform `sub`) or a `[[skill::id]]`
  wikilink, resolved to the linked instance's `credential`.

Enforcement is **deterministic** and applied on every read path:
`expand` of a non-owned owner-instance returns `{"page": null}` (absence,
not an error — existence is not leaked); `list_instances` and `search`
**filter out** non-owned owner-instances; the admin role bypasses. The
decision is a pure comparison on the request path — never an LLM, agent,
or classifier. Owner-visibility is reported back on `list_skills` as
`"visibility"` + `"owner_field"`.

###### Group ACL (`acl:` block, group ACL v1)

A skill MAY instead declare a **per-CRUD group ACL** — a superset of
`visibility`/`owner_field` (see
[ADR-0004](../adr/0004-rbac-groups.md)):

```yaml
owner_field: author       # still drives the `owner` group
acl:
  read:   [public]
  create: [owner]
  update: [owner, moderator]
  delete: [admin]
```

Each verb lists **group names**. `public` / `owner` / `admin` are
**reserved** special groups (always-present / structural-owner /
verified-admin respectively, and never grantable via a token claim or a
membership row); any other name is a **custom group**, satisfied when it
is present in the caller's `groups_claim` JWT array **or** in the
DuckDB-canonical `group_members` table. An action is allowed iff the
caller's effective group set intersects the verb's list (admin always
bypasses); empty intersection → deny (fail-closed, no deny rules in v1).

A verb omitted from the block, or a skill with neither `acl:` nor
`visibility:`, falls through to the **tenant default** — the
`acl_defaults:` block on the `escurel` meta-skill page, or, when unset,
the shipped default (`read:[public]`, writes `[admin]`) that reproduces
the pre-RBAC behaviour. A legacy `visibility:` field with no `acl:` block
maps deterministically (`public` → open read + admin writes; `owner` →
owner-all), so existing pages are unchanged. **`delete` is enforced as
`update` in v1** (there is no distinct delete operation at the write
boundary). Membership is mutated by the admin-only `add_group_member` /
`remove_group_member` / `list_group_members` tools. `list_skills` reports
the resolved block as `"acl"` (additive; `visibility`/`owner_field`
retained). *(Instance-level `acl:` overrides and capability-tool RBAC are
phase 2.)*

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

**Access:** `run_stored_query` is **admin-only**. A stored query
executes pre-declared arbitrary SQL over the whole corpus
(`pages`/`blocks`/`links`) and projects arbitrary columns
(aggregates, joins), so there is no per-row owner against which to
apply the per-instance read ACL — the gate is therefore at the
capability level (like the `admin_*` inspection tools). A non-admin
caller is refused with the `admin role required` error; an
unauthenticated dev/on-host caller (no verifier) is unaffected.

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
continue calling `apply_op` over HTTP.

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

`update_page` (and `apply_op`) against an instance whose skill is a
**non-writable backend** (`sql_view`, `document`) is rejected with
`{ok: false, issues: [{code: "backend_read_only", ...}]}` — the external
source/blob is canonical and is never written back through the page API.
See [Instance backends](#instance-backends).

### Events / inbox (M7 — Event-sourcing surface)

Events are the dynamic input of the memory triad (Events · Skills ·
Instances). They live in a dedicated `events` store (not pages); each
event's `label_skill` links to the skill that knows how to process it,
and `instance_page_id` links to the instance it belongs to once
processed. The inbox is the `status = 'inbox'` view. All four tools are
MCP tools over `POST /mcp` with the usual quota debits
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

The delivered payload always carries an additional `tenant_id` field — the
gateway's authoritative tenant (single-tenant per indexer) — so the
receiver knows which tenant the event belongs to without a side channel.

When `ESCUREL_WEBHOOK_SECRET` is also set, the gateway authenticates the
POST: it serializes the body once, computes **HMAC-SHA256 over those exact
body bytes** under the secret, and sends it as the header
`X-Escurel-Webhook-Signature: sha256=<hex>` (lowercase hex of the 32-byte
digest), POSTing the same bytes with `content-type: application/json`. The
receiver recomputes the HMAC over the raw request body and rejects a
missing/mismatched signature (a constant-time compare). With no secret
configured the POST is unsigned (dev). This is the only ingress trust
anchor between the gateway and the external runner.

## Instance backends

By default an instance's data is native **markdown** (writable, block-grain,
CRDT-backed). Two **external backends** let an instance's data live elsewhere
while every escurel invariant holds — single referent space, markdown-canonical,
derivable index, fail-closed ACL, single-writer:

- **`sql_view`** — a read-only DuckDB `VIEW` over an external relational source
  (postgres / mysql / sqlite / erpl / json_dir / parquet_dir).
- **`document`** — an uploaded file (PDF / DOCX / PPTX / XLSX, or text)
  extracted, chunked, and embedded into one page-with-blocks.

The unifying idea: **every external instance keeps a markdown overlay page** —
the page *is* the instance in the referent space (identity, links, ACL, history
all reuse the existing machinery), and a `backend_ref` frontmatter block binds
it to the external data. All novelty is confined to *where the body/data comes
from*, so `resolve` / `expand` / `neighbours` / `list_*` / `search` route
through the backend transparently and no dispatcher or wire change is needed to
add one. A skill selects its backend in frontmatter:

```yaml
backend:
  kind: sql_view            # markdown (default) | sql_view | document
  # …kind-specific config (see below)…
```

`list_skills` reports each skill's `backend.kind` + a `capabilities` object
(`writable`, `granularity`, `search`, `supports_crdt`); `sql_view` and
`document` are `writable: false`, so `update_page` / `apply_op` against them
return `backend_read_only` (the overlay/source is not editable through the page
API).

### `sql_view`

Skill frontmatter declares the source, a projection, and the columns that feed
search:

```yaml
backend:
  kind: sql_view
  source: { connector: postgres, attach: crm_pg, relation: public.customers,
            filter: "region = 'EU'" }      # filter is optional, injection-guarded
  project: { customer_id: id, display_name: name }   # source col → overlay field
  search_text: [name, notes]               # columns that enter late FTS
  projection_limit: 50                     # optional; rows expand renders (default 50)
```

- **Secrets never live in markdown.** `source.attach` names a credential
  registered out-of-band via `register_credential` (admin), realised as a
  DuckDB `CREATE SECRET`. `list_credentials` returns names only.
- **`create_sql_instance`** `{skill, id, overlay_body?}` materialises the
  instance under the write lock: it `ATTACH`es the source `READ_ONLY`, creates a
  managed `vw_<…>` view, captures a `source_schema_fingerprint`, and writes the
  overlay page with `backend_ref { kind: "sql_view", view, binding_hash,
  source_schema_fingerprint }`.
- **Reads** (`expand`) merge the overlay (which wins) with a **bounded**
  projection of the view (`expand.backend_projection = {view, rows, source,
  truncated, issue?}`); a colliding source field is exposed under
  `source.<field>`. Never an unbounded dump.
- **`validate_bindings`** (admin) re-probes each view and compares the stored
  fingerprint; on drift the binding is marked `binding_degraded` and that view's
  reads **fail closed** (an `Issue`, not wrong rows).
- **Search** contributes *candidates only* (late-materialised FTS over
  `search_text`); the dispatcher applies the fail-closed ACL predicate to every
  lane **before** RRF fusion (INV-ACL-FUSION), and a view whose owner can't be
  resolved denies non-admins. See [storage.md](storage.md).

### `document`

Skill frontmatter declares accepted MIME types and chunking:

```yaml
backend:
  kind: document
  accepts: [application/pdf, text/plain]
  chunk: { max_chars: 800, overlap: 80 }
  lead_chunks: 8                           # optional; chunk lead expand returns (default 8)
```

Ingestion is **event-driven**, deposited-before-processed (an upload is never
lost), and runs the extractor off the per-tenant write lock:

1. An external client deposits a blob and notifies escurel via one of two
   authenticated HTTP endpoints (see below).
2. The MIME is routed to a `document` skill whose `accepts:` lists it; an
   unmatched MIME is parked with `Issue(no_handler_skill)` and the inbox blob is
   retained.
3. escurel records an **immutable ingest `Event`** (auditable; same event log as
   `capture_event`).
4. A deterministic worker **extracts** (kreuzberg for PDF/DOCX/PPTX/XLSX — on by
   default; plain-text for `text/*`) → **chunks** → **embeds** → **materialises**
   one instance = one page with N chunk blocks, under a brief write lock. The
   blob is canonical (content-addressed, retained); chunks are derivable
   (`rebuild` re-extracts).

`expand` returns the overlay + the **top-k relevant chunks** (`chunks_total`,
`chunks_truncated`), never the full text. `backend_ref` carries
`{ kind: "document", blob_id, extract_engine, chunk_count, status }`. Document
chunks are ordinary `blocks`, so search rides the same ACL-before-fusion path as
markdown.

#### Ingest endpoints

Two authenticated HTTP routes (not MCP tools), rate-limited per tenant as
Writes:

| route | body | purpose |
|---|---|---|
| `POST /ingest` | `{ blob_id, content_type, title? }` | ingest a blob already deposited in the tenant's `blobs/inbox/` area |
| `POST /ingest/upload` | `{ content_type, bytes_b64, title? }` | deposit (base64) **and** ingest in one call |

Both return the pipeline outcome:
`{ status, event_id, blob_id, page_id?, handler_skill?, chunk_count?, issue? }`
where `status` ∈ `materialised` | `extraction_failed` | `no_handler`. On
extraction failure the inbox blob is retained and the instance is marked
`extraction_failed` (the upload is never lost).

## Admin surface

The admin/operator capabilities are exposed as admin-role-gated MCP
tools over `POST /mcp` — there is no separate admin service. Each
requires the admin role on the OIDC token (configurable; see
[`platform.md`](platform.md#auth)); a call from a token without the
required role yields JSON-RPC error code `-32001`. Tenant resolution
rules are *different* on admin tools — the tenant is named explicitly
in each call rather than taken from the token's `tenant` claim
(an admin operates across tenants).

| tool | inputs | outputs | purpose |
|---|---|---|---|
| `tenant_create` | `{id, display_name, quotas?}` | `{ok, tenant: {...}}` | provision a new tenant |
| `tenant_list` | `{}` | `{tenants: [{id, status, created_at, ...}]}` | enumerate tenants |
| `tenant_get` | `{id}` | `{tenant: {id, status, quotas, ...}}` | fetch one |
| `tenant_update` | `{id, quotas?, status?, embedding_provider?}` | `{ok, tenant}` | suspend, resume, change quotas |
| `tenant_delete` | `{id, confirm: string}` | `{ok}` | hard-delete (15-location wipe) |
| `tenant_export` | `{id}` | `{tarball_b64, bytes}` (tarball: markdown + lane snapshot + manifest, base64-encoded) | export (blocking) |
| `tenant_import` | `{tenant_id, tarball_b64}` | `{bytes_imported}` | restore (blocking) |
| `audit` | `{tenant, scope?}` | `{drift: {markdown_not_in_duckdb: [...], indexed_but_no_markdown: [...]}}` | drift detection (two-way diff) |
| `rebuild` | `{tenant, scope?}` | `{done, total}` | recover from canonical markdown (blocking) |
| `attach_external` | `{tenant, catalog_uri, name}` | `{ok}` | wire a DuckLake catalog into `external.ducklake` |
| `register_credential` | `{name, connector, secret}` | `{ok}` | register a named external-source credential (server-side; secret never echoed) — see [Instance backends](#instance-backends) |
| `list_credentials` | `{}` | `{credentials: [{name, connector, created_at, created_by}]}` | enumerate registered credentials (names only) |
| `delete_credential` | `{name}` | `{ok}` | remove a credential |
| `create_sql_instance` | `{skill, id, overlay_body?}` | `{page_id, view}` | materialise a read-only `sql_view` instance from a `sql_view` skill |
| `validate_bindings` | `{}` | `{bindings: [{page_id, view, status, detail?}]}` | re-probe every `sql_view`; `binding_degraded` ⇒ that view reads fail closed |
| `embedding_reload` | `{tenant?}` | `{ok}` | retry model load after a degraded start |
| `compact_lanes` | `{tenant}` | `{ops_compacted, bytes_reclaimed}` | force a DuckDB `CHECKPOINT` + `VACUUM` + `PRAGMA hnsw_compact_index` (blocking) |
| `quota_get` | `{tenant}` | `{quotas: {...}, current_usage: {...}}` | inspect |
| `delete_chat_history` | `{tenant_id, chat_group_id?, before_ts?, author?}` | `{deleted}` | destructive purge of the conversation log |
| `health` | `{}` | `{ok, version, embedding_status, storage_backend, tenant_count, ...}` | liveness + summary |

The `audit` and `rebuild` tools are the operational
recovery path. The cost is ~32 ms/page; a 1000-page tenant
rebuilds in ~32 s.

### Long-running operations

`rebuild`, `compact_lanes`, `tenant_export`, and `tenant_import`
take a while. They are ordinary blocking `tools/call` requests over
`POST /mcp`: the call holds the connection open until the operation
finishes and returns the final result in the JSON-RPC result (shapes
in the table above). There is no streaming or SSE. SSE/streaming for
progress is a possible future enhancement, not current behaviour.

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
- **Blocking result**. The call returns `{tarball_b64, bytes}` once
  the whole export is assembled; the JSON result carries the
  export-format version and a SHA-256 hash of the body bytes so
  consumers verify before treating the tarball as durable.
- **Failures**. Surface as a JSON-RPC `error` object with
  `retryable: bool`. Retryable errors invite the consumer to
  re-issue the call; non-retryable errors indicate corruption and
  require operator intervention.
- **Idempotency is the consumer's responsibility**. Re-running
  an export produces a new tarball with potentially different
  bytes (if writes happened in between); consumers key
  snapshots by `{tenant_id, started_at}`, not by content hash.

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

Long-running tools (`rebuild`, `compact_lanes`, `tenant_export`,
`tenant_import`) block until done and return their final result in
the JSON-RPC result. There is no SSE; live op streams use the
WebSocket transport (`/ws`).

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

The MCP `serverInfo.version` and the WebSocket protocol version
(`escurel-ws/1`) both start at `1`. Breaking changes bump the version;
the server can serve both versions for one major-version overlap.

The MCP tool JSON Schemas are served via the `tools/list` handshake so
client implementations can pin to a version.
