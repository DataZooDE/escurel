# 02 — The tool surface (fourteen agent tools)

The contract every surface carries (HTTP/MCP, CLI, WebSocket).
Canonical: `docs/contract/agent-interface.md` §The tool surface and
`docs/spec/protocol.md` §Agent surface (full JSON schemas + field types).
Wire shapes per transport: `references/03` (HTTP/MCP) and
`references/04` (CLI). Rust signatures: `references/05`.

Design rule: **read OR write, never both in one call.** All read tools
are safe to call speculatively; all writes go through validation. The
chat-history tools (`append_message` / `list_messages`) are append-mostly
and sit **alongside** the typed KB rather than inside it — see the Chat
section below.

## Read tools

| tool | inputs (key ones) | output | what for |
|---|---|---|---|
| `search` | `q`, `k=10`, `granularity='block'\|'page'`, `page_type?`, `skill?` | ranked hits `{page_id, anchor, snippet, skill, page_type, score}` | natural-language vector + FTS hybrid; the cold-start primitive |
| `resolve` | `wikilink` | `{parsed, page (PageRef), exists}` | parse + look up a `[[wikilink]]`; reports validity without raising |
| `expand` | `page_id`, `anchor?`, `version?` | `{page, frontmatter, body, blocks[], wikilinks_out[]}` | the body fetch — the **most expensive** primitive; use sparingly |
| `neighbours` | `page_id`, `direction='in'\|'out'\|'both'`, `link_skill?` | list of `Edge {src_page, dst_page, link_skill, link_version?, dst_anchor?}` | typed link-graph traversal (backlinks + forward links) |
| `list_skills` | — | list of `{id, description, required_frontmatter, optional_frontmatter, is_event_typed}` | the Tier-1 catalogue |
| `list_instances` | `skill`, `order_by_at='asc'\|'desc'?`, `limit?` | list of `{page_id, skill, frontmatter, at}` | enumerate instances of a skill (event-log scans, chain heads) |
| `run_stored_query` | `query_id`, `params` (typed object) | `{rows, schema[], snapshot_version?}` | execute a `[[query::*]]` page with bound params (origin axis) |

Notes:
- `search` granularity is `block` by default (pinpoints a block within a
  page); `page` collapses to one row per page. The choice is echoed in the
  response so a cache can tell them apart.
- `list_instances` frontmatter filtering (`{status: open}`, `{at: '>= …'}`)
  is in the contract; the MCP/CLI surface today exposes `skill`,
  `order_by_at`, `limit` (richer filter clauses land per
  `protocol.md` §list_instances).
- `run_stored_query` params are bound as **typed values** (prepared
  statements), never string-interpolated. Missing required param →
  `missing_required_param`; unknown param → `unknown_param`.

## Write tools

| tool | inputs | output | mode |
|---|---|---|---|
| `validate` | `content`, `as_page_id?` | `{issues[]}` | dry run — no commit |
| `update_page` | `page_id`, `content` | `{ok, issues[], new_version}` | whole-page write (the public write path) |
| `open_session` | `page_id` | `{session, head_version, content}` | live CRDT |
| `apply_op` | `session`, `op` | `{ok, conflicts?}` | live CRDT |
| `close_session` | `session`, `commit=true` | `{final_version, issues}` | live CRDT |

`update_page` is the path you use for seeding and for whole-page authoring
(`references/07`). The live CRDT trio (`open_session`/`apply_op`/
`close_session`) is for co-editing with a user or another actor over
`/ws`; most apps start with `update_page` and only reach for live mode
when they need granular concurrent edits.

## Chat tools (M-Chat, issue #63)

Per-chat-group conversation history. Distinct from the typed-instance KB:
this is an **append-mostly log** keyed by an opaque `chat_group_id` (the
consumer owns the identifier scheme — room IDs, DM pair IDs, …). Use it
for raw turn-by-turn messages; do **not** route chat through `update_page`
(that would rewrite the whole page on every append and embed every block).
ADR: `docs/adr/0002-chat-message-surface.md`.

| tool | inputs | output | mode |
|---|---|---|---|
| `append_message` | `chat_group_id`, `role`, `content`, `author?`, `ts?`, `metadata?`, `msg_id?`, `embed=true` | `{msg_id, ts}` | append (Writes quota) |
| `list_messages` | `chat_group_id`, `since?`, `until?`, `limit=100`, `cursor?`, `direction='desc'` | `{messages[], next_cursor?}` | read (Queries quota) |

Field semantics:
- `chat_group_id` is opaque — escurel never parses it. Pick a scheme
  that's stable for your app (e.g. `room-<uuid>`, `dm-<a>-<b>`).
- `ts` is RFC-3339 UTC. Omit to let the server stamp `CURRENT_TIMESTAMP`;
  the response always carries the resolved value.
- `msg_id` defaults to a server-generated **ULID** (26-char Crockford
  base32). Supply your own when re-ingesting from an external source.
- `embed=false` skips the embedding cost for the row — relief valve for
  high-volume sources. Non-embedded rows still appear in `list_messages`;
  they just don't participate in vector-recall paths.
- `since` is **inclusive**, `until` is **exclusive** (half-open interval).
- `direction` defaults to `'desc'` (most recent first); pass `'asc'` for
  forward chronological reads.
- `cursor` is opaque — pass the previous response's `next_cursor` verbatim.

There is **no agent-facing delete tool by design.** Deletion is operator
territory: `EscurelAdmin.DeleteChatHistory(tenant_id, [chat_group_id,
before_ts])` covers GDPR right-to-erasure (group set), retention pruning
(`before_ts` set), per-group pruning (both set), and full-tenant wipe
(neither set). Schedule the prune from your app (a substrate periodic
job or a cron in your backend) — escurel ships the building block, not
the policy.

### Validation is a first-class output

`validate`, `update_page`, and `apply_op` return the **same** issue shape:

```jsonc
{ "severity": "error" | "warning", "code": "<str>",
  "location": "<str>", "message": "<str>", "suggestion": "<str>?" }
```

An `error`-severity issue **rejects** the write; `warning`-severity
commits but is reported. Drive your authoring UX off the issue codes; the
catalogue is the `[[error-catalogue]]` page in a tenant.

## Anti-patterns (carry these into your app's agent prompts too)

- Don't `expand` every search hit — descriptions/snippets usually suffice.
- Don't enumerate the whole catalogue for a narrow task — search-first
  reaches the right skill in ~2 calls.
- Don't pass raw SQL to `run_stored_query` — author a `[[query::*]]` page
  first; the dispatcher refuses non-query-page SQL.
- Don't trust a frontmatter `mentions:` string over a typed wikilink.

## Instance backends

`list_skills` reports each skill's `backend.kind` (`markdown` | `sql_view` |
`document`) + a `capabilities` object. Reading a backend-sourced instance uses
the ordinary read tools (`expand` returns `backend_projection` for `sql_view`,
or top-k chunks + `chunks_total` for `document`); both kinds are read-only, so
`update_page` / `apply_op` against them return `backend_read_only`. Managing
them is `escurel:admin`-gated and so not part of the normal agent surface:

- `create_sql_instance(skill, id, [overlay_body])` — materialise a read-only
  view-backed instance.
- `register_credential(name, connector, secret)` / `list_credentials()` /
  `delete_credential(name)` — the `sql_view` source-secret registry (secrets
  never echoed back).
- `validate_bindings()` — re-probe every `sql_view` for schema drift; a
  `binding_degraded` view reads fail-closed.
- Document uploads use the authenticated `POST /ingest` / `POST /ingest/upload`
  HTTP routes (not an MCP tool). See `references/01` §Backend axis and the
  repo's `docs/spec/protocol.md` § Instance backends.

## Not exposed (by design)

No direct SQL, no raw vector/embedding access, no cross-tenant calls.
Ops-only tools (`audit`, `rebuild`, `attach_external`, `export`/`import`)
and admin tools (`admin_*`, gated by the `escurel:admin` role) are not part
of the normal app surface — see `references/08` and `references/10`.
