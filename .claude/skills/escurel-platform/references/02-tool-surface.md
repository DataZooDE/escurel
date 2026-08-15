# 02 — The tool surface

The contract every surface carries (HTTP/MCP, CLI, WebSocket).
Canonical: `docs/contract/agent-interface.md` §The tool surface and
`docs/spec/protocol.md` §Agent surface (full JSON schemas + field types).
Wire shapes per transport: `references/03` (HTTP/MCP) and
`references/04` (CLI). Rust signatures: `references/05`.

Design rule: **read OR write, never both in one call.** All read tools
are safe to call speculatively; all writes go through validation. Every
`tools/list` entry also carries an `execution: "deterministic" |
"orchestration"` label — `deterministic` = a pure function of KB state +
arguments (reads, queries, validation); `orchestration` = the call
advances loop state (writes, events, sessions). New tools default to
`orchestration` (fail-closed), so a client can hand a compute step
deterministic tools only. The
chat-history tools (`append_message` / `list_messages`) are append-mostly
and sit **alongside** the typed KB rather than inside it — see the Chat
section below.

## Read tools

| tool | inputs (key ones) | output | what for |
|---|---|---|---|
| `search` | `q`, `k=10`, `granularity='block'\|'page'`, `page_type?`, `skill?` | ranked hits `{page_id, anchor, snippet, skill, page_type, score}` | natural-language vector + FTS hybrid; the cold-start primitive |
| `resolve` | `wikilink` | `{parsed, page (PageRef), exists}` | parse + look up a `[[wikilink]]`; reports validity without raising |
| `expand` | `page_id`, `as_of?`, `scenario?`, `full?` (all chunks of a document instance) | `{page, frontmatter, body, blocks[], wikilinks_out[], content_sha256?}` (`content_sha256` = the stored-bytes hash, i.e. the value `update_page.base_sha256` guards against; plain reads only) (+ `shadow` on an overlay that shadows a base skill: `{base_page_id, pack, base: {…base frontmatter…}}`) | the body fetch — the **most expensive** primitive; use sparingly |
| `neighbours` | `page_id`, `direction='in'\|'out'\|'both'`, `link_skill?` | list of `Edge {src_page, dst_page, link_skill, link_version?, dst_anchor?}` | typed link-graph traversal (backlinks + forward links) |
| `list_skills` | — | list of `{id, description, required_frontmatter, optional_frontmatter, is_event_typed, visibility, owner_field?, autonomy?, layer, shadows?}` | the Tier-1 catalogue, **scoped to the caller**; `layer` is `"overlay"` (default) or the `base@<pack>@v<N>` pin; a shadowing overlay is ONE entry carrying `shadows: base@<pack>@v<N>`; `autonomy` is the declared human-in-the-loop policy — see the note below |
| `list_instances` | `cursor?` (pass back the response next-cursor; ONLY a null one means done), `skill_id`, `order_by='at asc'\|'at desc'?`, `limit?`, `frontmatter_key?`+`frontmatter_value?`, `as_of?`, `scenario?` | list of `{page_id, skill, frontmatter, at}` | enumerate instances of a skill (event-log scans, chain heads); NB the filter param is `skill_id` here but `skill` on `search` |
| `fetch_blob` | `page_id` (a document instance) | `{blob: {page_id, content_type, size, bytes_base64} \| null}` | the raw bytes behind a document/RAG instance; capped at 25 MiB. For browsers/large files prefer `GET /blob/{page_id}` — same ACL, raw bytes, real `Content-Type`, no cap |
| `query_instance` | `ref` (a query-page id; `query_id` accepted as an alias), `params` (typed object) | `{rows, schema[], truncated}` | **the one query surface**: execute an authored `[[query::<id>]]` page — `{{target}}` substituted with its allow-listed managed view, `:params` bound as prepared statements, ACL checked on the TARGET per caller, rows capped server-side. (The legacy admin-gated `run_stored_query` was removed in the 2026-08-14 surface consolidation.) |

Notes:
- **`list_skills` is caller-scoped, and never carries group names.** A
  skill whose declared `acl.read` does not intersect your effective groups
  is **absent** from the catalogue — denial as absence, exactly like
  `expand`/`search`/`list_instances`, so you must not re-filter it client
  side. A skill with no `acl:` block stays visible to everyone, and a
  skill whose instances are owner-private (`visibility: owner`, or
  `acl.read: [owner]`) is still a discoverable *type*. The per-CRUD `acl`
  object itself is projected to an **admin** caller only: group names are
  the tenant's authorisation topology (they are named per engagement in a
  shared tenant), and a client cannot act on a grant it does not hold.
  `visibility` / `owner_field` are reported to every caller.
- **`autonomy` on `list_skills`** is the human-in-the-loop policy a skill
  page declares in frontmatter: `auto` (a write derived from this skill
  commits directly), `review` (held for human approval), `confirm` (as
  review, plus an out-of-band notification). Escurel reports it; it never
  enforces it — the gate lives in your app.
  **The field is absent when the key is absent AND when its value is
  unrecognised.** Treat absence as "hold for review". Never write
  `autonomy == "auto" ? ungated : gated` against a default you supply — an
  unrecognised value must not become `auto`, or a typo (`autonmy: review`)
  turns into an ungated write. `validate` returns
  `frontmatter_autonomy_unknown` (error) for the unrecognised case, which is
  how you tell it apart from an honest absence. `update_page` refuses such a
  write only when the operator has set `ESCUREL_AUTONOMY_LINT=enforce`
  (default `off`, middle rung `log`).
  **The approve recipe (atomic, race-free):** when you hold a
  `review`/`confirm` write, store the draft **and** the sha256 of the
  stored markdown it was drafted against (or the `expand` `version` on
  a CRDT gateway). Approval = `update_page` with `base_sha256` (or
  `base_version` + `require_exact_base`); a concurrent edit refuses
  `{code: conflict}` carrying `head_sha256`/`head_version` +
  `head_content`, so you re-diff and re-approve — approval can never
  silently overwrite an edit that landed in between. `base_sha256: ""`
  guards a create ("I expect no page yet").
- `search` granularity is `block` by default (pinpoints a block within a
  page); `page` collapses to one row per page. The choice is echoed in the
  response so a cache can tell them apart.
- `list_instances` frontmatter filtering beyond one
  `frontmatter_key`/`frontmatter_value` pair (`{at: '>= …'}` ranges) is
  in the contract; richer filter clauses land per
  `protocol.md` §list_instances.
- `query_instance` params are bound as **typed
  values** (prepared statements), never string-interpolated. Missing
  required param → `missing_required_param`; unknown param →
  `unknown_param`. A `query` page declares its params in frontmatter and
  names its data source as `target: [[<sql_view skill>::<instance>]]` —
  the caller never supplies SQL, and the server rejects params the page
  didn't declare (bind the SAME param vector to every query when one
  reply mixes several).
- `query_instance` is how the DataZoo agents and Peacock read structured
  data: query pages are **authored knowledge** (discoverable via
  `search`), so adding a query is a page write, not a deploy.

## Write tools

| tool | inputs | output | mode |
|---|---|---|---|
| `validate` | `content`, `as_page_id?` | `{issues[]}` | dry run — no commit |
| `update_page` | `page_id`, `content`, `base_version?`+`require_exact_base?` (CRDT gateways), `base_sha256?` (every gateway) | `{ok, issues[], new_version}` | whole-page write (the public write path); the `base_*` guards are the **atomic-approve** CAS — see the autonomy note |
| `delete_page` | `page_id`, `base_version?` | `{ok, …}` | **soft**-delete / archive |
| `open_session` | `page_id` | `{session, head_version, content}` | live CRDT |
| `apply_op` | `session`, `op` | `{ok, conflicts?}` | live CRDT |
| `close_session` | `session`, `commit=true` | `{final_version, issues}` | live CRDT |

`update_page` is the path you use for seeding and for whole-page authoring
(`references/07`). The live CRDT trio (`open_session`/`apply_op`/
`close_session`) is for co-editing with a user or another actor over
`/ws`; most apps start with `update_page` and only reach for live mode
when they need granular concurrent edits.

A session **fans out to every attached client**: an `op` from one peer is
delivered to the others as a `peer_op` frame (carrying the merged
`merged_version` + `content` as well as the raw op), and `presence` reaches
the other peers, which is what makes live cursors work. The originator gets
`op_ack` and not `peer_op` — it already knows its write landed.

Two properties to design for. **Attaching is ACL-gated and evaluated at
attach time**: a principal who may not read the page is refused with
`{"type":"error","code":"forbidden"}`, and an ACL revoked mid-session bites
when that peer next attaches rather than immediately (disconnect the peer if
you need it sooner). **There is no replay**: a peer that reconnects, or that
receives `resync_required` after falling behind the broadcast buffer, must
`expand` the page and re-attach. The CRDT state is lossless; the *delivery*
history is not retained.

`close_session(commit: true)` **writes the merged body through to the
store**, exactly as `update_page` would: the page is re-indexed, so the
committed text is immediately visible to `expand`, to `search`, and to the
link graph (`neighbours` / backlinks). `final_version` is the head that a
subsequent write should pass as `base_version`. `commit: false` discards the
session and writes nothing.

If the write fails, the session is left **open** and the call errors —
retry `close_session` rather than assuming the edit landed.

Earlier servers persisted only CRDT history on commit, so `final_version`
advanced while the readable body did not — a client that then wrote back
with the version it had just been handed silently overwrote its own session.
If your node predates this change, do not treat a committed session as
readable; check against the git ref your deployment pins.

`delete_page` **archives rather than destroys** (#300): the page is retracted
from discovery — search, `list_instances`, the catalogue — while the
canonical markdown is retained for audit. Do not reach for it expecting the
bytes to be gone. `base_version` is an optimistic-concurrency guard (the
version you last read); omit it to delete unconditionally.

If the bytes DO have to be gone, `purge_page` hard-removes an
already-archived husk — but it destroys the audit record, so it is
**admin-role only** (an agent token gets `-32001`); it refuses a live
page (`not_archived`), so it is never a shortcut past retraction.

Writes are layer-aware (`references/01` §Layer/stability axis):

- a page whose stored `layer:` is `base@<pack>@v<N>` (anything under
  `markdown/base/`) is **read-only** — `update_page` returns the Issue
  `layer_read_only`; `open_session` fails with a JSON-RPC `-32000` error
  prefixed `layer_read_only:`. A draft *declaring* `layer: base@…` is
  rejected the same way.
- a non-admin skill draft that declares a skill id a subscribed pack
  already provides refuses `shadow_requires_curator` (shadowing a base
  skill is curator work).
- a non-admin draft carrying a truthy `promotable:` refuses
  `promotable_requires_curator` (the promotion marker is curator-set).

Note this list is **curated, not exhaustive** — the server exposes 66 tools
(the count is pinned by `skill_doc_parity.rs`; update it here when the
surface changes), most of them operator/admin surface (tenant CRUD,
credential and endpoint registries, pack import/export, lane inspection,
snapshot publishing). The
tables here cover what an application consumes. If you need one that isn't
here, check `tools/list` against a running gateway and then fix this
reference — see the repo's `CLAUDE.md` §*Keeping the consumer skill in sync*.

## Event tools (the event bus)

Escurel is also the platform's **event bus**: an event is captured into a
tenant **inbox**, optionally fires the tenant's **HMAC webhook**
(`webhook_url`/`webhook_secret` gateway config), and becomes part of an
instance's **history** once assigned. Workers build chains on this
(capture → webhook → process → assign → capture the next hop).

| tool | inputs (key ones) | output | what for |
|---|---|---|---|
| `capture_event` | `label_skill` (**required** — the label→skill routing key; an unlabelled capture is refused `-32602`), `source`, `mime`, `title`, `body`, `instance_page_id?`, `event_id?`, `provenance?` | the stored `Event` (server mints `event_id`/`at` when empty) | ingest an event; fires the webhook |
| `list_inbox` | `limit`, `cursor?` | `{events[], next_cursor?}` | the tenant's UNPROCESSED events (a worker's poll fallback); pass `next_cursor` back as `cursor` — ONLY its absence means done (ACL filtering legitimately shortens pages) |
| `assign_event` | `event_id`, `instance_page_id` | ack | mark processed + attach to a page's history |
| `list_events` | `instance_page_id`, `limit`, `cursor?` | `{events[], next_cursor?}` | a page's PROCESSED history, **oldest first** — assigned events only (an unassigned inbox event is a pending work item, not history); paginated like `list_inbox`, so history past `limit` stays reachable |

Notes:
- **Capture + assign for timeline visibility.** Consumers that render an
  instance's activity (e.g. Peacock's `timeline` view) see only assigned
  events — a worker that wants its stamp visible must `capture_event`
  AND `assign_event`.
- `event_id` is idempotency: pass a stable id (e.g. the upstream message
  id) so redelivery upserts instead of duplicating.
- Under `ESCUREL_EVENT_ACL=enforce`, `assign_event` gates BOTH sides: the
  event must be readable by the caller AND the target instance writable
  by them (`acl.update`) — assignment moves the event into the target's
  visibility domain. Either refusal comes back as the same
  `event not found` shape (no existence oracle); `log` mode warns and
  allows. A nonexistent target refuses identically under enforce.
- The webhook payload is HMAC-signed with the tenant's secret; receivers
  verify before acting (see the follow-up-worker in the agent template
  for the canonical consumer).
- A consumer that cannot host the webhook subscribes over the WebSocket
  instead: `event_subscribe` on `/ws` pushes captured events live
  (ACL-filtered) — see `11-event-driven-agents.md`.

## Admin materialisation (external backends)

`create_sql_instance` (`{skill, id}`) materialises a managed view for a
`sql_view`-backed skill — a **post-boot admin call**, not something
`ESCUREL_SEED_DIR` does (seeding only writes markdown). Dev gateways
(auth Disabled) accept it from the default client; production requires
an admin principal. `attach_external` registers the underlying source.

## Chat tools (M-Chat, issue #63)

Per-chat-group conversation history. Distinct from the typed-instance KB:
this is an **append-mostly log** keyed by an opaque `chat_group_id` (the
consumer owns the identifier scheme — room IDs, DM pair IDs, …). Use it
for raw turn-by-turn messages; do **not** route chat through `update_page`
(that would rewrite the whole page on every append and embed every block).
ADR: `docs/adr/0002-chat-message-surface.md`.

| tool | inputs | output | mode |
|---|---|---|---|
| `append_message` | `chat_group_id`, `role`, `content`, `author?`, `ts?`, `metadata?`, `msg_id?`, `embed=true` | `{msg_id, ts}` | append (Writes quota); a caller-supplied `msg_id` is an **idempotency key** — a retry echoes the stored row (original `ts`), never a duplicate |
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
commits but is reported. A rejection also sets `isError: true` on the
MCP `CallToolResult` envelope (any `ok:false` refusal does — the
exceptions are `validate` and `dry_run:true` results, which report
rather than refuse), so a client checking only the MCP error flag still
sees it; the `ok:false` + `issues[]` payload is unchanged.
Drive your authoring UX off the issue codes; the
catalogue is the `[[error-catalogue]]` page in a tenant. Layer/pack write
rejections use the same shape: `layer_read_only` (write to a base-layer
page, or a draft declaring `layer: base@…`), `shadow_requires_curator`
(non-admin overlay declaring a pack-provided skill id),
`promotable_requires_curator` (non-admin draft carrying `promotable:`),
alongside the backend codes (`backend_read_only`) and `conflict`.

## Anti-patterns (carry these into your app's agent prompts too)

- Don't `expand` every search hit — descriptions/snippets usually suffice.
- Don't enumerate the whole catalogue for a narrow task — search-first
  reaches the right skill in ~2 calls.
- Don't pass raw SQL to `query_instance` — author a `[[query::*]]` page
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
  (both accept an optional `event_id` **idempotency key** — a redelivery
  with the same key returns `{status: "duplicate"}` instead of minting a
  second inbox event and re-running extraction; escurel#382)
  HTTP routes (not an MCP tool). See `references/01` §Backend axis and the
  repo's `docs/spec/protocol.md` § Instance backends.

## Skill packs (admin)

The curated federation surface (ADR-0006/0007/0008;
`docs/spec/protocol.md` §Admin surface). A **pack** is a deterministic
tar+gz of a skill subtree plus a manifest **HMAC-SHA256-signed with a
shared secret** (`ESCUREL_PACK_SECRET` on hub and spokes — a firm-operated
trust model, not per-publisher keys). Importing lands the pages as the
tenant's pinned, read-only **base layer** (`references/01`
§Layer/stability axis). All six tools are **admin-gated** — an agent-role
token cannot call them; a consuming app reads imported base pages like
any other page and never manages packs:

| tool | what for |
|---|---|
| `export_pack` | build a signed pack from the named skills (+ instances opt-in). Refuses without a configured secret (`pack_secret_not_configured`) and fails closed on credential-shaped page content (`pack_secret_detected`) |
| `import_pack` | verify signature + content hash **before** unpacking, then land the pages under `markdown/base/<pack>/` stamped `layer: base@<id>@v<N>` and record the version pin. Transport-neutral: an air-gapped tarball and a live pull are the same call |
| `list_packs` | the subscribed packs + their pins (`{pack_id, version, vertical, publisher, content_hash}`) |
| `rebase_pack` | the **reviewed upgrade** — the only operation that moves a pin. A field the tenant's shadow overrides AND the new version changes surfaces as a `rebase_conflict` Issue and blocks until `acknowledge_conflicts=true`; never auto-resolved |
| `unsubscribe_pack` | drop a subscription cleanly: base pages removed, then the pin; tenant overlays survive |
| `submit_promotion` | the harvest direction: propose a scrubbed, signed pack **candidate** (`version: 0` — deliberately not importable) from this node's `promotable: true` skill pages. Default-deny, one ineligible id refuses the whole request, and every submission emits an immutable audit event; a hub curator reviews and publishes deliberately |

Refusal codes you may see in operator tooling:
`pack_secret_not_configured`, `pack_secret_detected`,
`pack_signature_invalid`, `pack_id_invalid`, `pack_malformed`,
`pack_version_pinned`, `pack_content_mismatch`, `vertical_mismatch`,
`pack_skill_collision`, `pack_not_subscribed`,
`pack_rebase_not_an_upgrade`, `pack_candidate_not_importable`,
`promotion_not_eligible`, `rebase_conflict`.

CLI twins: `escurel admin pack export|import|list|rebase|unsubscribe|
submit-promotion` (`references/04`).

## Not exposed (by design)

No direct SQL, no raw vector/embedding access, no cross-tenant calls.
Ops-only tools (`audit`, `rebuild`, `attach_external`, `export`/`import`)
and admin tools (`admin_*`, gated by the `escurel:admin` role) are not part
of the normal app surface — see `references/08` and `references/10`.
