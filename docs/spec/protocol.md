# Protocol — MCP/HTTP, WebSocket

HTTP is the sole transport surface. Both transports expose the same
logical surface — the agent tools and the admin endpoints. They
differ only in framing and streaming model. The tool
*semantics* are the contract in
[`../contract/agent-interface.md`](../contract/agent-interface.md);
this doc specifies the *wire shapes*.

## Transport summary

| transport | mount | what it carries | streaming | default for |
|---|---|---|---|---|
| MCP-over-HTTP | `/mcp` | JSON-RPC 2.0 framed as MCP method calls; one HTTP request per call; long-running calls block until done and return the final result | none (blocking) | agents, MCP clients, CLI/TUI, admin/operator tools |
| WebSocket | `/ws` | Bidirectional. Used for live CRDT op streams, presence pings, and search-result streaming | full-duplex | live mode, web client |

Alongside the two tool transports the server also mounts a set of
plain HTTP endpoints (no JSON-RPC framing): `GET /openapi.json`
(an OpenAPI 3.1 document generated from the same `tools/list`
payload, for non-MCP HTTP clients), the liveness/readiness probes
`GET /healthz` + `GET /readyz`, `GET /version`, `GET /metrics` (a
Prometheus scrape, optionally on a dedicated listener), and the
two document-ingest routes `POST /ingest` + `POST /ingest/upload`
(see [Instance backends](#instance-backends)).

**Execution labels (WI-8 / REQ-LABEL-01).** Every `tools/list` entry
carries an additive `execution: "deterministic" | "orchestration"`
label. `deterministic` = the result is a pure function of KB state +
arguments (reads, queries, validation, bundle builds); `orchestration`
= the call advances loop state (writes, events, sessions, lifecycle).
The default for a new tool is `orchestration` (fail-closed: nothing
masquerades as deterministic compute by omission). This makes the
interlocked-loops "deterministic-first" invariant machine-visible: a
per-phase tool surface can hand a compute step deterministic tools
only.

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
  score:   number,        // RRF-fused (or rerank score when reranking is on)
  similarity: number,     // raw vector cosine similarity of the hit
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

### `FilterClause` (used by `search`)

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

The agent tools, grouped by axis. Inputs and outputs given as JSON
Schema. (The full non-admin agent surface is ~22 tools — the read /
write / event / session tools below plus `fetch_blob`, `query_instance`,
`write_instance`, `list_snapshots`, `append_message`, `list_messages`;
admin/operator tools are in [Admin surface](#admin-surface).)

### Read tools

Several read tools share two optional overlay/time-travel params:

- **`scenario`** *(string)* — a **what-if overlay**. Absent/null reads
  the base corpus only; a named scenario reads `base ∪ overlay`, the
  overlay winning per slug. Accepted by `expand`, `resolve`,
  `neighbours`, `search`, and `list_instances`.
- **`as_of`** *(RFC 3339 string)* — a time-travel cut; state/edges/
  blocks born after it are excluded (see the M7 note under `expand`).
  Accepted by `expand`, `neighbours`, `search`, and `list_instances`.

#### `search`

```jsonc
// request
{
  "q": "Acme renewal risk",          // single query; provide this OR `queries`
  "queries": ["Acme renewal", "Acme churn risk"],  // optional; 2–8 phrasings
                                     // fused (RRF) into one ranking
  "k": 10,
  "granularity": "block",            // "block" | "page", default "block"
  "page_type": "any",                // "skill" | "instance" | "any"
  "skill": "customer",               // optional filter; pushes link_skill predicate to DuckDB
  "filter": { "at": { ">=": "2026-04-01" } },  // optional frontmatter filter (FilterClause)
                                               // (events use this to time-window)
  "page_id": null,                   // optional; restrict search to one page's blocks
  "as_of": null,                     // optional RFC 3339 time-travel cut
  "scenario": null                   // optional what-if overlay
}
// response
{ "hits": [Hit, ...], "granularity": "block" }
```

Provide exactly one of `q` / `queries` (at least one non-empty string).
`filter` is the `FilterClause` shape. It is
applied **after** vector + FTS retrieval as a metadata
post-filter, so it doesn't degrade recall; only the response is
narrowed. Useful for "find recent meetings about Acme":
`search(q='Acme', skill='meeting', filter={at: {">=":
"2026-04-01"}})`.

#### `resolve`

```jsonc
// request
{ "wikilink": "[[customer::acme-corp]]", "scenario": null }
// response
{
  "parsed": WikilinkParsed,
  "page": PageRef,         // or null if not found (or ACL-denied)
  "exists": true
}
```

Returns the parsed link plus the resolved page metadata.
`resolve` does NOT fetch the body — that's `expand`.

#### `expand`

```jsonc
// request
{ "page_id": "01HXMQ...", "as_of": null, "scenario": null, "full": false }
// response
{
  "page": PageRef,
  "frontmatter": { ... },         // full frontmatter
  "body":   "...markdown body...",
  "blocks": [ { "anchor": "blk-acme-signals", "content": "..." }, ... ],
  "wikilinks_out": [ WikilinkParsed, ... ],
  // external-backend instances only (see Instance backends):
  "backend_projection": null,      // sql_view: { view, rows, source, truncated, issue? }
  "chunks_total": null,            // document: total chunk count
  "chunks_truncated": false        // document: blocks are a bounded lead of chunks_total
}
```

`scenario` is the what-if overlay and `as_of` the time-travel cut
(shared read-tool params, above). `full` *(bool, default false)*
is a `document`-instance knob: when true `expand` returns **every**
chunk block (the single-document detail / heatmap view) instead of
the bounded lead — `chunks_truncated` is then always false.

**Not yet implemented.** An earlier draft returned a
`snapshot_version` field (the replayed CRDT snapshot marker); the
current builder never emits it. Use `list_snapshots` to enumerate an
instance's replayable `taken_at` points.

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

#### `fetch_blob`

```jsonc
// request
{ "page_id": "01HXMQ..." }
// response
{
  "blob": {                        // or null (see below)
    "page_id":      "01HXMQ...",
    "content_type": "application/pdf",   // sniffed (pdf / docx / pptx / xlsx / text)
    "size":         104857,
    "bytes_base64": "JVBERi0x..."        // the original retained file bytes
  }
}
```

Returns the **original retained file** behind a `document`-backed
instance (the blob named by `backend_ref.blob_id`), base64-encoded with
a sniffed content type, for a faithful client-side preview of the
source document. `blob` is `null` for a non-document page, a missing
page, or an instance the caller may not read (ACL-mirrored on `expand`;
existence is not leaked). The transfer is capped at 25 MiB.

#### `neighbours`

```jsonc
// request
{
  "page_id":  "01HXMQ...",
  "direction": "both",             // "in" | "out" | "both"
  "link_skill": null,              // optional single skill filter
  "as_of": null,                   // optional RFC 3339 time-travel cut
  "scenario": null                 // optional what-if overlay
}
// response
{
  "edges": [
    {
      "src_page": "01HX...",       // source page id
      "dst_page": "01HY...",       // destination page id
      "link_skill": "meeting",
      "link_version": null,
      "dst_anchor": null           // anchor on the destination, if the link targets one
    },
    ...
  ]
}
```

Edges whose other endpoint is an owner-private instance the caller
cannot read are dropped (fail-closed ACL).

**Not yet implemented.** The request keys `link_skill_in` (a
multi-skill array) and `order_by` (a `<field> <asc|desc>` sort on the
target's frontmatter), and the edge fields `anchor` /
`src_anchor` / `target_frontmatter_excerpt`, are not accepted/emitted
by the current dispatcher — a caller that sends the extra request keys
has them silently ignored.

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
  "frontmatter_key":   "source",    // optional single-field equality filter…
  "frontmatter_value": "gmail",     // …both must be present to apply
  "order_by": "at desc",            // optional; "at asc" | "at desc" only
  "limit":    50,
  "as_of":    null,                 // optional RFC 3339 time-travel cut
  "scenario": null                  // optional what-if overlay
}
// response
{
  "instances": [
    { "page_id": "01HX...", "skill": "meeting",
      "frontmatter": { "at": "...", "with": "...", ... },
      "at": "2026-04-12T10:00:00+02:00" },   // the typed `at`, or null
    ...
  ],
  "next_cursor": null               // reserved; always null today
}
```

This is the event-log primitive. Unlike `search`, `list_instances`
does **not** accept the operator-wrapped `FilterClause` object — its
only filter is the single `frontmatter_key` = `frontmatter_value`
string-equality pair. `order_by` is restricted to `at asc` / `at desc`.
Owner-private instances the caller cannot read are filtered out.

#### `run_stored_query`

```jsonc
// request
{
  "query_id": "customer-churn-trend",
  "params":   { "customer_id": "acme-corp", "from_date": "2026-01-01" }
}
// response
{
  "rows":   [ ... ],
  "schema": [ { "name": "as_of", "type": "DATE" }, ... ]
}
```

**Not yet implemented.** The `as_of_snapshot` request key (pin to a
DuckLake snapshot) and the `snapshot_version` response field are not
accepted/emitted by the current handler — it reads `{query_id, params}`
and returns `{rows, schema}`. For event-volume
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

#### `query_instance`

A parameterised, full-result-set read over **one `sql_view` instance's**
view — the agent-surface counterpart of `run_stored_query`. The query page
(a `[[query::*]]` instance) declares a `target: [[skill::id]]` naming the
`sql_view` instance and references its view via the `{{target}}` placeholder:

```yaml
# markdown/instances/query/sales-by-category.md
type: instance
skill: query
id: sales-by-category
target: "[[sales::eu-2026]]"        # the sql_view instance to read
params:
  - {name: min, type: number, required: true}
sql: "SELECT category, SUM(amount)::BIGINT AS total
      FROM {{target}} WHERE amount >= :min GROUP BY category"
```

```jsonc
// request                          // response
{                                   {
  "ref":    "sales-by-category",      "rows":      [ { "category": "hw", "total": 50 } ],
  "params": { "min": 10 }             "schema":    [ { "name": "total", "type": "BIGINT" } ],
}                                     "truncated": false
                                    }
```

`ref` is the query id or its `[[query::id]]` wikilink. Two trust boundaries
are kept separate by construction:

- **Value position** — every `:param` runtime value is bound as a positional
  DuckDB prepared-statement parameter (the `run_stored_query` pattern), so
  injection through a param value is impossible and it never flows through the
  `sql_view` filter-interpolation path.
- **Identifier position** — `{{target}}` resolves to the target's managed
  `vw_…` view name, allow-listed through the same `vw_`-prefix guard the
  projection path uses (never a bound value).

**Access:** unlike `run_stored_query`, `query_instance` is an **agent tool**
gated by the **per-instance read ACL on the target instance**
(`may_read_instance`, fail-closed): the caller must be allowed to read the
underlying data, not merely the query template. Admin bypasses; a denied
caller gets an authorisation error. The result set is capped at
`MAX_RESULT_ROWS` (10 000) with `truncated` set when the cap clipped the tail.

### Write tools

#### `validate`

```jsonc
// request
{ "content": "---\nskill: meeting\n...\n---\n# ...", "as_page_id": null }
// response
{ "ok": true, "issues": [Issue, ...] }
```

`ok` is `false` iff any `Issue` is error-severity (warnings do not fail
a draft); the full `issues` list is always returned.

#### `open_session` / `apply_op` / `close_session` (live CRDT)

```jsonc
// open_session request:  { "page_id": "01HX..." }
// open_session response: { "session": "sess_...", "head_version": "v42",
//                          "ws_url": "/ws" }
//
// apply_op request:  { "session": "sess_...", "op": "<base64 Loro op bytes>" }
// apply_op response: { "ok": true, "merged_version": "v43" }
//
// close_session request:  { "session": "sess_...", "commit": true }
// close_session response: { "ok": true, "final_version": "v50", "issues": [] }
```

`op` is base64-encoded Loro op bytes. `ws_url` is the relative `/ws`
path (the gateway does not know its public origin, so it never emits a
full `wss://` URL). **Not yet implemented:** `open_session` does not
return the page `content`, and `apply_op` returns neither `content`,
`conflicts`, nor `issues` — a client reads the merged document over the
WS channel or via `expand`.

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
  "auto_merged": true,             // true iff a stale base_version was three-way-merged
  "issues":      [Issue, ...]
}
```

If `base_version` is supplied and the head has advanced, the
server attempts a CRDT-aware three-way merge (Loro): it
reconstructs the base snapshot the client branched from, forks
it into the head and incoming edits as concurrent Loro branches,
and unions them. A clean merge is persisted and the response
carries `auto_merged: true`. The server refuses to persist a
merge that no longer parses or whose frontmatter matches neither
side (both sides changed the same key) — that is an unresolvable
conflict: `{ok: false, issues: [{code: "conflict", ...}],
head_content: "..."}`, and the client re-drafts against
`head_content`. (Auto-merge needs the base snapshot; a
`base_version` older than the first `update_page` snapshot, or a
bare session op-count with no snapshot, always conflicts.)

`update_page` (and `apply_op`) against an instance whose skill is a
**non-writable backend** (`sql_view`, `document`) is rejected with
`{ok: false, issues: [{code: "backend_read_only", ...}]}` — the external
source/blob is canonical and is never written back through the page API.
See [Instance backends](#instance-backends).

`update_page` against a **base-layer page** — one whose stored
frontmatter carries `layer: base@<pack>@<version>`, i.e. it was imported
from a subscribed skill pack — is rejected with `{ok: false, issues:
[{code: "layer_read_only", location: "frontmatter.layer", ...}]}`
(REQ-LAYER-02). The guard keys off the *stored* page's layer, so
stripping the `layer:` field from the draft is not an unlock; a draft
*declaring* `layer: base@…` is rejected the same way (base pages are
created by pack import only, never by `update_page`). `open_session` on
a base-layer page fails with a JSON-RPC `-32000` error whose message
starts `layer_read_only:` — live CRDT co-authoring must not bypass the
guard. Pages without a `layer:` field (every pre-layer page) and pages
declaring `layer: overlay` are unaffected. `list_skills` reports each
skill's `layer` (`"overlay"` default, or the `base@<pack>@<version>`
pin) so agents and operators can tell stable from editable.

**Shadowing (REQ-LAYER-03).** A tenant overlay skill page MAY declare
the same skill id as an imported base page — that is how a tenant
specialises pack content without forking it. Page-level precedence
with drift visibility: `resolve` prefers the overlay; `list_skills`
reports ONE entry per skill id (the overlay) with an additive
`shadows: "base@<pack>@<version>"` pin; `expand` of the shadowing
overlay carries an additive `shadow` object —
`{base_page_id, pack, base: {…the base page's frontmatter…}}` — so the
base values stay visible, never silently masked (the same namespacing
discipline as the sql_view `source` object). The base page itself is
untouched (INV-SHADOW): expanding it directly returns the pack's
pristine content, and a future pack upgrade rebases against it.
`import_pack` therefore lands a base skill beneath an existing tenant
skill of the same id (the overlay direction of `pack_skill_collision`
no longer refuses; two BASE pages with one id still do — no precedence
exists between packs).

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
CRDT-backed). **External backends** let an instance's data live elsewhere
while every escurel invariant holds — single referent space, markdown-canonical,
derivable index, fail-closed ACL, single-writer:

- **`sql_view`** — a read-only DuckDB `VIEW` over an external relational source
  (postgres / mysql / sqlite / erpl / json_dir / parquet_dir).
- **`document`** — an uploaded file (PDF / DOCX / PPTX / XLSX, or text)
  extracted, chunked, and embedded into one page-with-blocks.
- **`openapi`** / **`mcp`** — *live remote (proxy)* instances: the body/data is
  fetched **live on `expand`** from a REST/OpenAPI endpoint (`openapi`) or an
  upstream MCP server (`mcp`), with optional **write-back**. Nothing is
  materialised in DuckDB. See [Remote backends](#remote-backends-openapi--mcp).

The unifying idea: **every external instance keeps a markdown overlay page** —
the page *is* the instance in the referent space (identity, links, ACL, history
all reuse the existing machinery), and a `backend_ref` frontmatter block binds
it to the external data. All novelty is confined to *where the body/data comes
from*, so `resolve` / `expand` / `neighbours` / `list_*` / `search` route
through the backend transparently and no dispatcher or wire change is needed to
add one. A skill selects its backend in frontmatter:

```yaml
backend:
  kind: sql_view            # markdown (default) | sql_view | document | openapi | mcp
  # …kind-specific config (see below)…
```

`list_skills` reports each skill's `backend.kind` + a `capabilities` object
(`writable`, `granularity`, `search`, `supports_crdt`); `sql_view`,
`document`, `openapi`, and `mcp` are all `writable: false`, so `update_page` /
`apply_op` against them return `backend_read_only` (the overlay/source is not
editable through the page API — remote backends accept write-back only through
the explicit `write_instance` tool). Remote backends additionally report
`search: "none"` — their live data is never indexed, so it feeds no search
lane (the overlay page itself is still indexed and searchable like any page).

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

### Remote backends (`openapi` / `mcp`)

Unlike `sql_view` / `document` (materialised, read-only), the two **remote
(proxy)** backends keep **no local copy**: an instance is a live window onto a
remote object. Its identity, links, ACL, and history are the ordinary overlay
page, but its body/data is fetched **live on `expand`**, and — because these
backends declare a `write` op — edits are forwarded **upstream** via the
explicit `write_instance` tool (never `update_page`; the remote source is
canonical). `openapi` proxies a REST/HTTP endpoint; `mcp` proxies an upstream
MCP server (escurel is the MCP client, calling a tool or reading a resource).

Four invariants that these backends deliberately revise vs. the materialised
external backends, and how each is kept safe:

1. **Read-only → write-back.** Remote instances are `writable: false` w.r.t.
   `update_page`/CRDT (the overlay body is a live projection, not co-authored),
   but accept write-back through `write_instance`, gated by the target
   instance's `acl.update`. The remote op is value-bound (payload + id map),
   never string-spliced.
2. **No search lane.** Live data is never indexed → `capabilities.search:
   "none"`. The overlay page's own metadata/body stays indexed and searchable.
3. **SSRF / secrets-in-markdown.** A skill's `backend.endpoint` names an
   **admin-registered endpoint** (base URL + auth held server-side in the
   `external_endpoints` registry), never a raw URL — so tenant markdown can
   never make the server fetch an arbitrary host, and no secret enters the
   corpus.
4. **Live-read failure.** A read that times out / errors returns the overlay
   page + `backend_projection.issue` — never a partial or fabricated body (the
   `binding_degraded` policy).

Skill frontmatter declares the endpoint, the `read`/`write` ops, and a
`project` map (response JSON `$.a.b` path or bare key → overlay field):

```yaml
# openapi — read + write
backend:
  kind: openapi
  endpoint: crm_rest                 # admin-registered (URL + auth server-side)
  read:  { operationId: getCustomer, path: /customers/{id} }   # method defaults GET
  write:                             # omit ⇒ read-only
    method: POST
    path: /customers/{id}/orders/{order_id}   # {order_id} from the payload
    body: { sku: "{sku}", qty: "{qty}", via: "escurel" }   # optional template
  project: { display_name: $.name, tier: $.account_tier }
```
```yaml
# mcp — read-only resource
backend:
  kind: mcp
  endpoint: upstream_kb              # points at the upstream server's /mcp
  read:  { resource: "kb://article/{id}" }   # or { tool: getArticle }
  project: { title: $.title }
```

`{name}` placeholders in a `path` / `resource` / body template are filled from
the overlay instance id (`{id}`) and, on a write, the payload's **scalar**
fields — flattened to dotted keys, so `{order_id}` and `{customer.tier}` both
resolve. A placeholder that cannot be resolved fails the call closed
(`unfilled path/body placeholders`), never sending a literal `{x}`. For an
OpenAPI write, an optional `body:` template reshapes the payload: an **exact**
`"{name}"` leaf keeps its JSON type (a number stays a number, an object stays
an object), while embedded `{name}` interpolates as a string; omit `body:` to
send the payload verbatim. For an MCP write, the payload's fields are merged
into the tool-call arguments. A read/write is also refused (fail-closed) when
the skill's backend `kind` does not match the `kind` its `endpoint` was
registered under. `expand` returns the overlay merged with the live projection
under `backend_projection = { source, fields, issue? }`; `backend_ref` carries
just `{ kind, endpoint }` — the `read`/`write` ops are re-derived from the
skill's `backend:` block on each call, never persisted per-instance. In a
`read:`/`write:` op only `path` + `method` (`operationId` is accepted but
ignored) drive the OpenAPI call.

New MCP tools:

- **`write_instance`** *(write)* — `{ ref, payload }` → forwards a write to the
  target remote instance's upstream `write` op and returns the re-projected
  instance. `ref` is the instance id or `[[skill::id]]`. Gated by the target's
  `acl.update` (fail-closed; admin bypasses). A skill whose binding declares no
  `write` op is refused (`backend_read_only`).
- **`create_remote_instance`** *(admin)* — `{ skill, id, overlay_body? }`
  materialises the overlay page + `backend_ref` for an `openapi`/`mcp` skill
  (the binding comes from the skill's `backend:` block, never the caller — the
  `create_sql_instance` pattern).

New admin endpoint-registry tools (mirror `register_credential` &c.):
`register_endpoint` `{ name, kind, base_url, auth, secret? }`,
`list_endpoints` `{}` (names/URLs only, secret never echoed),
`delete_endpoint` `{ name }`, `validate_endpoints` `{}` (probe each registered
endpoint's reachability; unreachable ⇒ that skill's reads fail closed).

## Admin surface

The admin/operator capabilities are exposed as admin-role-gated MCP
tools over `POST /mcp` — there is no separate admin service. Each
requires the admin role on the OIDC token (configurable; see
[`platform.md`](platform.md#auth)); a call from a token without the
required role yields JSON-RPC error code `-32001`. This gate is at
**dispatch**, not discovery: `tools/list` is *not* role-filtered — every
admin tool is always listed, and a non-admin caller is refused only when
it *calls* one (see [MCP-over-HTTP framing](#mcp-over-http-framing)).
Tenant resolution rules are *different* on admin tools — the tenant is
named explicitly (`tenant_id`) rather than taken from the token's claim
— but because a gateway is single-tenant, a `tenant_id` that names a
tenant other than the one this gateway serves is refused (`-32002`); an
empty value means "this gateway's tenant".

| tool | inputs | outputs | purpose |
|---|---|---|---|
| `tenant_create` | `{tenant_id, display_name?}` | `{spec: {tenant_id, display_name}}` | provision a new tenant (no `quotas` input) |
| `tenant_list` | `{}` | `{tenants: [{tenant_id, display_name}]}` | enumerate tenants |
| `tenant_get` | `{tenant_id}` | `{spec: {tenant_id, display_name}}` | fetch one |
| `tenant_update` | `{tenant_id, display_name?, status?, quotas?, embedding_provider?}` | `{spec, rebuild_required}` | **partial** update: rename, suspend/resume (`status: active\|suspended` — a suspended tenant rejects non-admin calls), per-tenant `quotas`, and `embedding_provider` (`zero\|gemini\|embeddinggemma`; changing it moves the vector space → `rebuild_required: true`, run `rebuild`). Live suspend/quota apply to the served tenant; embedding takes effect on next boot/rebuild (#247) |
| `tenant_delete` | `{tenant_id, confirm}` (`confirm` must equal `tenant_id`) | `{deleted}` | hard-delete a tenant + its on-disk state |
| `tenant_export` | `{tenant_id}` | `{format_version, tarball_b64, bytes, sha256}` (tarball: canonical **markdown only**, gzip'd; `sha256` = hex of the tarball body) | export (blocking) |
| `tenant_import` | `{tenant_id, tarball_b64}` | `{bytes_imported}` | restore markdown into an existing tenant (blocking) |
| `export_pack` | `{tenant_id, id, version, vertical, publisher, skills, include_instances?}` | `{manifest, tarball_b64, bytes}` | build a **skill pack**: a deterministic tar+gz of the named skills' pages (+ instances when `include_instances`) with an HMAC-signed `manifest` (`{format_version, id, version, vertical, publisher, page_count, content_hash, signature}`). Requires `ESCUREL_PACK_SECRET` (refuses unsigned, `pack_secret_not_configured`); fails closed on credential-shaped page content (`pack_secret_detected`). See ADR-0006 |
| `import_pack` | `{tenant_id, manifest, tarball_b64, allow_vertical_mismatch?}` | `{pack, version, vertical, pages_imported, layer}` | import a signed pack as the tenant's pinned, **read-only base layer**: signature + `content_hash` verify fail-closed **before** unpacking (`pack_signature_invalid`); unsafe pack ids refuse (`pack_id_invalid`); unsafe entry paths / malformed pages refuse (`pack_malformed`) with the WHOLE pack validated before the first page lands (a bad page ⇒ zero landed pages); pages land under the reserved `markdown/base/<pack>/` namespace stamped `layer: base@<id>@v<version>`; the pin is recorded in `pack_subscriptions` (a canonical input, like the credential registry). A version change on a subscribed pack refuses (`pack_version_pinned`), a same-version re-publish with different bytes refuses (`pack_content_mismatch`) — upgrades are an explicit future `rebase`; an unrelated vertical refuses (`vertical_mismatch`) unless `allow_vertical_mismatch`; a skill id another indexed skill page already declares refuses (`pack_skill_collision` — explicit shadowing is a future feature). Transport-neutral: an air-gapped tarball and a live pull are the same call. See ADR-0007 |
| `unsubscribe_pack` | `{tenant_id, pack_id}` | `{pack, pages_removed}` | drop a subscription cleanly: every base page the pack landed is removed (so `rebuild` cannot resurrect orphaned base content), then the pin; tenant overlays survive (a shadow simply stops shadowing); a later `import_pack` starts from zero. Refuses unknown packs (`pack_not_subscribed`) |
| `list_packs` | `{}` | `{packs: [{pack_id, version, vertical, publisher, content_hash}]}` | the subscribed packs and their pins |
| `rebase_pack` | `{tenant_id, manifest, tarball_b64, acknowledge_conflicts?, dry_run?}` | `{ok, issues, pack, from_version, to_version, pages_imported, pages_removed, conflicts_acknowledged}`; with `dry_run`: `{ok, dry_run: true, issues, pack, from_version, to_version, would_import, would_remove}` | the **reviewed upgrade** of a subscribed pack (REQ-REBASE-01/02) — the only operation that moves a version pin. Validates like `import_pack` (verify before unpack, whole pack before the first write); a field the tenant's shadow overrides AND the new version changes surfaces as a `rebase_conflict` Issue (`skill <id> · <field>`, body included) and blocks until `acknowledge_conflicts=true` — never auto-resolved; orphaned base pages the new version no longer ships are removed; the pin moves last. `dry_run=true` runs the full validation + conflict scan, applies **nothing**, and reports the plan (`ok` = a real run would apply cleanly without acknowledgement). Refuses non-subscribed packs (`pack_not_subscribed`) and non-upgrades (`pack_rebase_not_an_upgrade`) |
| `submit_promotion` | `{tenant_id, candidate_id, vertical, skills}` | `{manifest, tarball_b64, bytes, event_id}` | the L2→L3 **harvest**: propose a scrubbed, signed pack candidate from this node's own skills. **Default-deny** (REQ-PROMO-01): every id must be a tenant-authored SKILL page carrying the curator-set `promotable: true` marker — instances never promote, base-layer pages are the hub's; one ineligible id refuses the whole request (`promotion_not_eligible`). The deterministic scrubber (the export deny set) fails the submission closed on credential-shaped content (`pack_secret_detected`). Setting `promotable: true` via `update_page` is itself curator-gated (`promotable_requires_curator` for non-admin callers). Every submission emits an immutable audit event (`source: "promotion"`, what/when/by-whom). Maker/checker: the candidate carries `version: 0`; a hub curator reviews and publishes deliberately. See ADR-0008 |
| `admin_audit` | `{tenant_id}` | `{markdown_not_in_duckdb: [...], indexed_but_no_markdown: [...]}` | drift detection (two-way diff) |
| `rebuild` | `{tenant_id}` | `{done, total, current_page}` | recover the index from canonical markdown (blocking) |
| `attach_external` | `{tenant_id, source_url}` | `{source_id}` (derived catalog alias) | attach an external read-only DuckDB source |
| `register_credential` | `{name, connector, secret}` | `{ok}` | register a named external-source credential (server-side; secret never echoed) — see [Instance backends](#instance-backends) |
| `list_credentials` | `{}` | `{credentials: [{name, connector, created_at, created_by}]}` | enumerate registered credentials (names only) |
| `delete_credential` | `{name}` | `{ok}` | remove a credential |
| `create_sql_instance` | `{skill, id, overlay_body?}` | `{page_id, view}` | materialise a read-only `sql_view` instance from a `sql_view` skill |
| `validate_bindings` | `{}` | `{bindings: [{page_id, view, status, detail?}]}` | re-probe every `sql_view`; `binding_degraded` ⇒ that view reads fail closed |
| `register_endpoint` / `list_endpoints` / `delete_endpoint` / `validate_endpoints` | see [Remote backends](#remote-backends-openapi--mcp) | — | remote-backend endpoint registry |
| `create_remote_instance` | `{skill, id, overlay_body?}` | `{page_id, kind, endpoint}` | materialise an `openapi`/`mcp` overlay instance |
| `run_stored_query` | `{query_id, params?}` | `{rows, schema}` | admin-gated stored SQL over the whole corpus (see [Read tools](#run_stored_query)) |
| `admin_index_query` | `{table, limit?}` | `{rows, schema}` | read up to `limit` rows from an allow-listed index table (pages/blocks/links/crdt_ops/crdt_snapshots/chat_messages) |
| `admin_list_lanes` | `{}` | `{lanes: [{name, backend, tenants_present}]}` | enumerate configured LaneStores |
| `admin_lane_keys` | `{lane?, prefix?, limit?}` | `{keys: [{key, size_bytes}]}` | list lane keys under a prefix |
| `admin_lane_blob` | `{lane?, key}` | `{bytes_base64, content_type}` | fetch one lane blob (≤ 1 MiB) |
| `admin_webhook_deliveries` | `{limit?}` | `{configured, deliveries: [...]}` | recent outbound capture-webhook delivery outcomes |
| `add_group_member` | `{group_id, subject}` | `{ok}` | add a principal to a custom RBAC group |
| `remove_group_member` | `{group_id, subject}` | `{ok}` | remove a principal from a group |
| `list_group_members` | `{group_id}` | `{members: [{group_id, subject, added_at, added_by}]}` | list a group's members (audit) |
| `embedding_reload` | `{}` | `{model_revision}` | hot-reload the embedding model after a degraded start |
| `compact_lanes` | `{tenant_id}` | `{ops_compacted, bytes_reclaimed}` | compact CRDT op lanes (`CHECKPOINT` + `VACUUM` + `PRAGMA hnsw_compact_index`, blocking) |
| `admin_quota` | `{tenant_id}` | `{queries_remaining, writes_remaining, embeds_remaining, concurrent_sessions}` | inspect the per-tenant quota snapshot |
| `admin_delete_chat_history` | `{chat_group_id?, before_ts?, author?}` | `{deleted}` | destructive purge of the conversation log |

`write_instance` (`{ref, payload}` → the re-projected instance) is an
**agent** tool, not admin-gated — it is authorised by the target
instance's `acl.update`; see [Remote backends](#remote-backends-openapi--mcp).

There is no `health` MCP tool. Liveness/version are the plain HTTP
endpoints `GET /healthz` + `GET /readyz` + `GET /version` (see the
[Transport summary](#transport-summary)).

The `admin_audit` and `rebuild` tools are the operational
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
- **Deterministic tarball**. **Canonical markdown only** — the
  gzip'd tar contains the tenant's `markdown/` tree and nothing else
  (the derivable DuckDB index, `cache/`, and `spool/` are excluded;
  `rebuild` reconstructs the index from this markdown). See
  [`storage.md`](storage.md#per-tenant-directory-layout).
- **Blocking result**. The call returns
  `{format_version, tarball_b64, bytes, sha256}` once the whole export
  is assembled: `format_version` is the export-format version (int) and
  `sha256` is the hex digest of the tarball body, so consumers verify
  before treating the tarball as durable.
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
every tool — agent **and** admin — with its JSON Schema input
definition. `tools/list` is **not** role-gated: the admin tools are
always listed, and the admin role is enforced only at `tools/call`
dispatch (`-32001` for a non-admin caller). The same
`tools/list` payload is also published as an OpenAPI 3.1 document at
`GET /openapi.json` for non-MCP HTTP clients.

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

The MCP `initialize` handshake echoes the client's requested
`protocolVersion` when present (default `2025-06-18`) and reports
`serverInfo = { name: "escurel", version }`, where `version` is the
`escurel-server` crate semver (`CARGO_PKG_VERSION`) — not a bare `1`.

There is **no** separate WebSocket protocol-version string
(`escurel-ws/1` is **not implemented** — the `/ws` upgrade performs no
version negotiation).

The MCP tool JSON Schemas are served via the `tools/list` handshake so
client implementations can pin to a version.
