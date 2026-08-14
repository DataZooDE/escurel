//! MCP discovery surface: the `tools/list` payload, execution labels and the
//! OpenAPI document.
//!
//! Split out of `mcp.rs` (which was 7,247 lines) because none of this touches
//! the indexer, application state or `await` -- it is pure JSON construction,
//! and mixing it with request handling made both harder to review. See
//! `docs/notes/complexity-reduction-plan.md` R1.
//!
//! NOTE: the tool names here are still maintained separately from the dispatch
//! arms in the parent module and from `DETERMINISTIC_TOOLS` below. Unifying
//! those three registries is R2 of the same plan; this split is what makes
//! that change reviewable.

use escurel_md::PageType;
use serde_json::{Value, json};

pub(super) fn tools_list_payload() -> Value {
    json!({
        "tools": [
            tool_entry(
                "list_skills",
                Execution::Deterministic,
                Scope::Agent,
                "Return the tenant's Tier-1 skill catalogue.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "list_instances",
                Execution::Deterministic,
                Scope::Agent,
                "Enumerate instances of a skill, optionally filtered by a frontmatter field.",
                json!({
                    "type": "object",
                    "required": ["skill_id"],
                    "properties": {
                        "skill_id": { "type": "string" },
                        "cursor": { "type": "string", "description": "Opaque resume cursor from a previous page's next_cursor; ONLY a null next_cursor means done (ACL filtering shortens pages)." },
                        "order_by": { "type": "string", "enum": ["at asc", "at desc"] },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 },
                        "frontmatter_key": { "type": "string", "description": "Frontmatter field to filter on (with frontmatter_value)." },
                        "frontmatter_value": { "type": "string", "description": "Required value of frontmatter_key." },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; instances born after it are excluded (untimed always remain)." },
                        "scenario": { "type": "string", "description": "What-if overlay; absent = base only, else base ∪ overlay (overlay wins per slug)." }
                    }
                }),
            ),
            tool_entry(
                "resolve",
                Execution::Deterministic,
                Scope::Agent,
                "Parse a [[wikilink]] and look up its target page.",
                json!({
                    "type": "object",
                    "required": ["wikilink"],
                    "properties": {
                        "wikilink": { "type": "string" },
                        "scenario": { "type": "string", "description": "What-if overlay to resolve against; absent = base only." }
                    }
                }),
            ),
            tool_entry(
                "expand",
                Execution::Deterministic,
                Scope::Agent,
                "Fetch a page's frontmatter + body + outbound wikilinks.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; the page is null if born after it." },
                        "scenario": { "type": "string", "description": "What-if overlay to read against; absent = base only." },
                        "full": { "type": "boolean", "description": "Return ALL chunks of a document instance instead of the bounded lead (REQ-DOC-05)." }
                    }
                }),
            ),
            tool_entry(
                "fetch_blob",
                Execution::Deterministic,
                Scope::Agent,
                "Fetch the original retained file bytes of a document-backed instance (base64 + content type) for a faithful client preview.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." }
                    }
                }),
            ),
            tool_entry(
                "neighbours",
                Execution::Deterministic,
                Scope::Agent,
                "Typed link-graph traversal.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." },
                        "direction": { "type": "string", "enum": ["in", "out", "both"] },
                        "link_skill": { "type": "string" },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; edges from sources born after it are hidden." },
                        "scenario": { "type": "string", "description": "What-if overlay; edges filtered by their source page's scenario." }
                    }
                }),
            ),
            tool_entry(
                "provenance_ancestry",
                Execution::Orchestration,
                Scope::Agent,
                "Bounded multi-hop provenance traversal (ADR-0010). \
                 `direction: up` returns everything the page rests on (its \
                 causes); `down` returns everything derived from it. Optionally \
                 restrict to `relations` (e.g. [\"derived_from\",\"motivated_by\"]); \
                 `max_hops` is clamped server-side. Returns page-ref hops with \
                 the reaching relation and depth. Pass `to_page` to ask the \
                 PATH question instead: does `page_id` reach `to_page` within \
                 `max_hops`? Then returns `{reachable, path, depth}`; a route \
                 through an ACL-private node reports `reachable: false` (no \
                 existence leak).",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." },
                        "to_page": { "type": "string", "description": "Target page: switches to reachability/shortest-path mode ({reachable, path, depth})." },
                        "direction": { "type": "string", "enum": ["up", "down"], "description": "up = what this rests on; down = what derives from it. Default up." },
                        "relations": { "type": "array", "items": { "type": "string" }, "description": "Restrict the walk to these edge kinds; absent/empty = all." },
                        "max_hops": { "type": "integer", "minimum": 1, "maximum": 12, "description": "Hop ceiling (default 5, capped at 12)." },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; edges from sources born after it are hidden." }
                    }
                }),
            ),
            tool_entry(
                "provenance_report",
                Execution::Orchestration,
                Scope::Agent,
                "Corpus-wide provenance report (ADR-0010). `kind: \"drift\"` = \
                 decisions resting on a since-superseded expectation (lost \
                 context); `kind: \"abandoned\"` = nodes retired by \
                 `supersedes`/`abandons` (dead-ended branches). Optionally \
                 scope to a `skill`. Returns `{kind, rows}`; rows touching an \
                 ACL-private page are dropped, fail-closed.",
                json!({
                    "type": "object",
                    "required": ["kind"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["drift", "abandoned"] },
                        "skill": { "type": "string", "description": "Restrict to this skill; absent/empty = all." }
                    }
                }),
            ),
            tool_entry(
                "search",
                Execution::Deterministic,
                Scope::Agent,
                "Hybrid vector + FTS search, RRF-fused. Pass `q` for a single \
                 query, or `queries` with 2-3 phrasings to fuse their results \
                 in one ranking (provide exactly one of the two).",
                json!({
                    "type": "object",
                    "properties": {
                        "q": { "type": "string", "description": "Single query string. Provide this OR `queries`." },
                        "queries": { "type": "array", "items": { "type": "string" }, "description": "Multiple query variants fused into one ranking (RRF across all variants × lanes). Provide this OR `q`." },
                        "k": { "type": "integer", "minimum": 0, "maximum": 1000 },
                        "granularity": { "type": "string", "enum": ["block", "page"], "description": "Result granularity; `page` collapses block hits to one per page. Default `block`." },
                        "page_type": { "type": "string", "enum": ["skill", "instance", "any"] },
                        "skill": { "type": "string" },
                        "filter": { "type": "object", "description": "Frontmatter post-filter; clauses are ANDed, e.g. {\"tier\": \"gold\", \"at\": {\">=\": \"2026-04-01\"}}." },
                        "as_of": { "type": "string", "description": "RFC 3339 time-travel cut; blocks born after it are excluded." },
                        "scenario": { "type": "string", "description": "What-if overlay; base-only when absent." }
                    }
                }),
            ),
            tool_entry(
                "query_instance",
                Execution::Deterministic,
                Scope::Agent,
                "Run a [[query::*]] report that declares `target: [[skill::id]]` \
                 against that sql_view instance's view. Runtime `params` are bound \
                 as prepared-statement values (never interpolated); the report's \
                 aggregation runs in the view and the full result set is returned. \
                 The per-instance ACL gates the target instance, fail-closed.",
                json!({
                    "type": "object",
                    "required": ["ref"],
                    "properties": {
                        "ref": { "type": "string", "description": "Query id or [[query::id]] wikilink; its `target` names the sql_view instance to read." },
                        "query_id": { "type": "string", "description": "Alias for `ref` (the retired run_stored_query's spelling)." },
                        "params": { "type": "object", "description": "Runtime values bound to the report's `:param` placeholders." }
                    }
                }),
            ),
            tool_entry(
                "validate",
                Execution::Deterministic,
                Scope::Agent,
                "Dry-run the indexer's checks on a draft; returns the same issue list \
                 as update_page but commits nothing.",
                json!({
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string" },
                        "as_page_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "update_page",
                Execution::Orchestration,
                Scope::Agent,
                "Upsert a markdown page (whole-body write). Optional \
                 `base_version` (from a prior read's `version`) enables \
                 optimistic concurrency with CRDT auto-merge: a stale write is \
                 three-way-merged against concurrent head edits (`ok:true, \
                 auto_merged:true`); an unmergeable one returns \
                 `{ok:false, issues:[{code:conflict}], head_content}`. Set \
                 `require_exact_base` to skip the merge and conflict on ANY \
                 stale base — what a human-in-the-loop approval needs, because a \
                 merged page is not the diff that was reviewed. Asking for either \
                 guard on a gateway that does not track versions returns \
                 `{ok:false, issues:[{code:versioning_unavailable}]}` rather than \
                 writing unguarded.",
                json!({
                    "type": "object",
                    "required": ["page_id", "content"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." },
                        "content": { "type": "string" },
                        "base_version": { "type": "string" },
                        "require_exact_base": { "type": "boolean" },
                        "base_sha256": { "type": "string", "description": "Content-hash CAS — the approval guard that works on EVERY gateway (base_version needs a CRDT backend). Hex sha256 of the stored markdown the held write was drafted against; \"\" = approve-create (expect no page). Mismatch refuses {code: conflict} + head_sha256 + head_content. (#354)" },
                        "provenance": { "type": "object" }
                    }
                }),
            ),
            tool_entry(
                "delete_page",
                Execution::Orchestration,
                Scope::Agent,
                "Soft-delete (archive) a markdown page/instance: retract it from \
                 discovery (search/resolve/neighbours/list) by dropping its index \
                 rows and link edges, while retaining the canonical markdown \
                 (stamped `archived: true`) as an audit record a rebuild skips. \
                 Optional `base_version` (from a prior read's `version`) guards \
                 against deleting a page that changed since you read it \
                 (`{ok:false, issues:[{code:conflict}]}`). Returns \
                 `{ok:false, issues:[{code:not_found}]}` for an absent page. The \
                 mandatory `escurel` meta-skill cannot be deleted.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." },
                        "base_version": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "purge_page",
                Execution::Orchestration,
                Scope::Admin,
                "ADMIN. Permanently remove an ALREADY-ARCHIVED page from the \
                 lane, finishing what `delete_page` started. `delete_page` \
                 retracts and retains the markdown as an audit record; this \
                 gives that record up — an operator act, refused for non-admin \
                 tokens. Refuses a LIVE page (`{code:not_archived}`) — purging \
                 is not a shortcut past retraction. Returns \
                 `{ok:false, issues:[{code:not_found}]}` for an absent page, so a \
                 sweep is re-runnable. The mandatory `escurel` meta-skill cannot \
                 be purged.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": { "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." } }
                }),
            ),
            tool_entry(
                "move_page",
                Execution::Orchestration,
                Scope::Agent,
                "Move a page to a new `page_id`, leaving NOTHING at the old one. \
                 Use this to restructure ids; use `delete_page` to retract \
                 knowledge. The difference matters: a delete retains the old \
                 markdown as an audit record, which is correct for a retraction \
                 and pure noise for a move — the content still exists, at the new \
                 id. Wikilinks are unaffected either way: `[[skill::id]]` \
                 addresses a page by skill and id, never by path. Returns \
                 `{ok:false, issues:[{code:not_found}]}` for an absent source, and \
                 `{code:conflict}` when a LIVE page already occupies `to` (an \
                 archived husk there is replaced). The mandatory `escurel` \
                 meta-skill cannot be moved.",
                json!({
                    "type": "object",
                    "required": ["from", "to"],
                    "properties": {
                        "from": { "type": "string" },
                        "to": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "append_message",
                Execution::Orchestration,
                Scope::Agent,
                "Append a message to a chat-group's conversation history. \
                 `chat_group_id` is opaque to escurel; consumers own the \
                 identifier scheme. `embed` defaults to true; set false to \
                 skip the embedding cost for high-volume sources.",
                json!({
                    "type": "object",
                    "required": ["chat_group_id", "role", "content"],
                    "properties": {
                        "chat_group_id": { "type": "string" },
                        "role": {
                            "type": "string",
                            "enum": ["user", "assistant", "system", "tool"]
                        },
                        "content": { "type": "string" },
                        "author": { "type": "string" },
                        "ts": {
                            "type": "string",
                            "description": "RFC-3339 UTC; server stamps CURRENT_TIMESTAMP when absent"
                        },
                        "metadata": { "type": "object" },
                        "msg_id": {
                            "type": "string",
                            "description": "Caller-supplied IDEMPOTENCY KEY: a retry with the same (chat_group_id, msg_id) echoes the stored row instead of inserting a duplicate. Server generates a ULID when absent (no dedup)."
                        },
                        "embed": { "type": "boolean", "default": true }
                    }
                }),
            ),
            tool_entry(
                "list_messages",
                Execution::Deterministic,
                Scope::Agent,
                "Read back a chat-group's conversation history time-ordered. \
                 `since` is inclusive, `until` is exclusive. `direction` \
                 defaults to `desc` (most recent first). Use `next_cursor` \
                 to page.",
                json!({
                    "type": "object",
                    "required": ["chat_group_id"],
                    "properties": {
                        "chat_group_id": { "type": "string" },
                        "since": { "type": "string" },
                        "until": { "type": "string" },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 1000,
                            "default": 100
                        },
                        "cursor": { "type": "string" },
                        "direction": {
                            "type": "string",
                            "enum": ["asc", "desc"],
                            "default": "desc"
                        }
                    }
                }),
            ),
            tool_entry(
                "capture_event",
                Execution::Orchestration,
                Scope::Agent,
                "Append an event to the global inbox (M7). `label_skill` links \
                 to the skill that knows how to process this event type; \
                 `instance_page_id` may pre-flag a candidate instance but the \
                 event stays in the inbox until `assign_event`. Returns the \
                 stored event with its id + timestamp. The gateway stamps the \
                 verified caller as `provenance.captured_by`, overwriting any \
                 value you send under that key: it is what scopes the event to \
                 you while it sits un-triaged in the inbox. Idempotent on \
                 `event_id`: a re-capture returns the stored first-writer \
                 event — or, if that event is not yours to see, your own \
                 submission back under the same id.",
                json!({
                    "type": "object",
                    "required": ["label_skill"],
                    "properties": {
                        "event_id": { "type": "string", "description": "Caller-supplied id; server generates a ULID when absent." },
                        "at": { "type": "string", "description": "RFC 3339 event time." },
                        "source": { "type": "string", "description": "Ingest source, e.g. gmail/meet/drive." },
                        "mime": { "type": "string", "description": "Content type, e.g. message/rfc822." },
                        "label_skill": { "type": "string", "description": "Skill id: how to process this event type." },
                        "instance_page_id": { "type": "string", "description": "Candidate instance (label hint); still inbox until assigned." },
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "provenance": { "type": "object" }
                    }
                }),
            ),
            tool_entry(
                "list_inbox",
                Execution::Deterministic,
                Scope::Agent,
                "List unprocessed events (the inbox), newest first. Filtered to the \
                 events you may see: an event filed into an instance follows \
                 that instance's ACL, an un-triaged one is yours only if you \
                 captured it, and admin sees all (`ESCUREL_EVENT_ACL`). A page \
                 may therefore come back shorter than `limit` — ONLY the \
                 absence of `next_cursor` means the listing is complete; pass \
                 `next_cursor` back as `cursor` to continue.",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 },
                        "cursor": { "type": "string", "description": "Opaque resume cursor from a previous page's next_cursor." }
                    }
                }),
            ),
            tool_entry(
                "list_events",
                Execution::Deterministic,
                Scope::Agent,
                "List an instance's processed event history (the event sequence \
                 whose projection is its state), oldest first. Pass `event_id` \
                 instead to look ONE event up by id — whatever its status — \
                 which is how you discover the instance an event was assigned \
                 to. Exactly one of `instance_page_id` or `event_id`. \
                 Filtered by the same per-event ACL as `list_inbox`; an event \
                 you may not see is absent, not an error. Paginated: ONLY the \
                 absence of `next_cursor` means the history is complete; pass \
                 it back as `cursor` to read past `limit`.",
                json!({
                    "type": "object",
                    "properties": {
                        "instance_page_id": { "type": "string" },
                        "event_id": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 },
                        "cursor": { "type": "string", "description": "Opaque resume cursor from a previous page's next_cursor (listing branch only)." }
                    }
                }),
            ),
            tool_entry(
                "list_snapshots",
                Execution::Deterministic,
                Scope::Agent,
                "List the taken_at timestamps of an instance's CRDT snapshot \
                 history, oldest first — the discrete state-over-time points \
                 expand(as_of=T) can replay.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." }
                    }
                }),
            ),
            tool_entry(
                "list_op_authors",
                Execution::Deterministic,
                Scope::Agent,
                "Who wrote each live-editing (CRDT) op on a page, oldest first: \
                 op_id, hlc, applied_at and the server-verified `principal` that \
                 submitted it. The read side of write attribution — the principal \
                 is the caller the gateway authenticated, NOT the Loro peer id in \
                 the op payload, which identifies a device rather than a person. \
                 `principal` is null for ops applied before the gateway recorded \
                 one. Ops already subsumed by a snapshot and compacted away are \
                 not listed. Returns no op bytes. Follows the page's own read \
                 ACL: a page you may not read reports an empty history, \
                 indistinguishable from one that has none.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." }
                    }
                }),
            ),
            tool_entry(
                "assign_event",
                Execution::Orchestration,
                Scope::Agent,
                "Assign an inbox event to an instance and mark it processed — the \
                 (external) agent folding the event into the instance. A \
                 compare-and-set: re-assigning to the SAME instance is a no-op \
                 success, a different instance for an already-processed event \
                 conflicts, and an event you may not see is refused as NOT \
                 FOUND — indistinguishable from one that does not exist.",
                json!({
                    "type": "object",
                    "required": ["event_id", "instance_page_id"],
                    "properties": {
                        "event_id": { "type": "string" },
                        "instance_page_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "open_session",
                Execution::Orchestration,
                Scope::Agent,
                "Open a live CRDT session on a page; returns a session id and the WS upgrade URL.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string", "description": "Repo-relative page path, e.g. `markdown/instances/<skill>/<slug>.md` (skills live under `markdown/skills/<id>.md`)." }
                    }
                }),
            ),
            tool_entry(
                "apply_op",
                Execution::Orchestration,
                Scope::Agent,
                "Apply a base64-encoded Loro op blob to an open session.",
                json!({
                    "type": "object",
                    "required": ["session", "op"],
                    "properties": {
                        "session": { "type": "string" },
                        "op": { "type": "string", "description": "base64-encoded Loro op bytes" }
                    }
                }),
            ),
            tool_entry(
                "close_session",
                Execution::Orchestration,
                Scope::Agent,
                "Close a session; optionally snapshot the doc (commit=true).",
                json!({
                    "type": "object",
                    "required": ["session"],
                    "properties": {
                        "session": { "type": "string" },
                        "commit": { "type": "boolean", "default": true }
                    }
                }),
            ),
            // Admin-gated ops tools. Visible in tools/list, but the
            // dispatcher rejects non-admin callers (see require_admin).
            tool_entry(
                "admin_quota",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: per-tenant quota snapshot (remaining query/write/embed \
                 budget + concurrent sessions in use).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "admin_audit",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: drift between canonical markdown and the DuckDB index \
                 (markdown_not_in_duckdb / indexed_but_no_markdown).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "admin_webhook_deliveries",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: recent outbound capture-webhook delivery outcomes \
                 (newest first) — event_id, ok, http_status, error. \
                 `configured: false` when no ESCUREL_WEBHOOK_URL is set.",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 100 }
                    }
                }),
            ),
            tool_entry(
                "admin_index_query",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: read up to `limit` rows from an allow-listed index table \
                 (pages, blocks, links, crdt_ops, crdt_snapshots, \
                 chat_messages). Not arbitrary SQL.",
                json!({
                    "type": "object",
                    "required": ["table"],
                    "properties": {
                        "table": {
                            "type": "string",
                            "enum": ["pages", "blocks", "links",
                                     "crdt_ops", "crdt_snapshots", "chat_messages"]
                        },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
                    }
                }),
            ),
            tool_entry(
                "admin_delete_chat_history",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: purge chat history. GDPR erasure of a whole group \
                 (chat_group_id set) or a single member across groups \
                 (author set), retention prune (before_ts set); filters \
                 compose with AND. MCP twin of the gRPC \
                 EscurelAdmin.DeleteChatHistory.",
                json!({
                    "type": "object",
                    "properties": {
                        "chat_group_id": { "type": "string" },
                        "before_ts": { "type": "string" },
                        "author": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "admin_list_lanes",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: enumerate the configured LaneStores (name, backend, \
                 tenants present). MCP twin of EscurelAdmin.AdminListLanes.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "admin_lane_keys",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: list keys under a prefix in a lane, with byte sizes. \
                 MCP twin of EscurelAdmin.AdminLaneKeys.",
                json!({
                    "type": "object",
                    "properties": {
                        "lane": { "type": "string", "description": "Lane name; empty = the default `markdown`." },
                        "prefix": { "type": "string", "description": "Tenant-relative key prefix." },
                        "limit": { "type": "integer", "minimum": 0, "description": "0 → server default (100)." }
                    }
                }),
            ),
            tool_entry(
                "admin_lane_blob",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: fetch one blob (base64) from a lane, subject to a \
                 1 MiB cap. MCP twin of EscurelAdmin.AdminLaneBlob.",
                json!({
                    "type": "object",
                    "required": ["key"],
                    "properties": {
                        "lane": { "type": "string" },
                        "key": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "add_group_member",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: add a principal `subject` to a custom RBAC group \
                 `group_id`. Idempotent. Membership is the source of truth \
                 for groups escurel manages; reserved names \
                 (public/owner/admin) are resolved structurally and ignored \
                 if stored.",
                json!({
                    "type": "object",
                    "required": ["group_id", "subject"],
                    "properties": {
                        "group_id": { "type": "string", "description": "The group name." },
                        "subject": { "type": "string", "description": "The principal `sub`." }
                    }
                }),
            ),
            tool_entry(
                "remove_group_member",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: remove a principal `subject` from a custom RBAC \
                 group `group_id`. No-op when the row is absent.",
                json!({
                    "type": "object",
                    "required": ["group_id", "subject"],
                    "properties": {
                        "group_id": { "type": "string" },
                        "subject": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "list_group_members",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: list the members of a custom RBAC group, with \
                 grant time + granting admin (audit).",
                json!({
                    "type": "object",
                    "required": ["group_id"],
                    "properties": {
                        "group_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "register_credential",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: register (or replace) a named external-source \
                 credential a sql_view skill references via \
                 `backend.source.attach`. The secret is stored server-side \
                 and NEVER in the markdown corpus (REQ-SQL-05).",
                json!({
                    "type": "object",
                    "required": ["name", "connector", "secret"],
                    "properties": {
                        "name": { "type": "string", "description": "The `attach` name skills reference." },
                        "connector": { "type": "string", "description": "postgres|mysql|sqlite|erpl|s3|…" },
                        "secret": { "type": "string", "description": "DSN / secret material (server-side only)." }
                    }
                }),
            ),
            tool_entry(
                "list_credentials",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: list registered external-source credentials WITHOUT \
                 their secrets (name, connector, registration audit).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "delete_credential",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: remove a registered external-source credential by \
                 name. No-op when absent.",
                json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": { "name": { "type": "string" } }
                }),
            ),
            tool_entry(
                "validate_bindings",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: re-probe every SQL-view binding and report schema \
                 drift (binding_degraded) or unreachable sources \
                 (backend_unavailable). Reconciles views ⟂ backend_refs.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "create_sql_instance",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: materialise a sql_view instance — the binding comes \
                 from the skill's backend.source block (read-only view + \
                 overlay page).",
                json!({
                    "type": "object",
                    "required": ["skill", "id"],
                    "properties": {
                        "skill": { "type": "string", "description": "A skill declaring backend.kind=sql_view." },
                        "id": { "type": "string", "description": "New instance id." },
                        "overlay_body": { "type": "string", "description": "Optional overlay markdown body." }
                    }
                }),
            ),
            tool_entry(
                "register_endpoint",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: register (or replace) a remote-backend endpoint an \
                 openapi/mcp skill references via `backend.endpoint`. The base \
                 URL + auth secret are stored server-side and NEVER in the \
                 markdown corpus (SSRF / secrets-in-markdown guard).",
                json!({
                    "type": "object",
                    "required": ["name", "kind", "base_url"],
                    "properties": {
                        "name": { "type": "string", "description": "The `endpoint` name skills reference." },
                        "kind": { "type": "string", "enum": ["openapi", "mcp"] },
                        "base_url": { "type": "string", "description": "REST base URL (openapi) or /mcp URL (mcp)." },
                        "auth": { "type": "string", "enum": ["none", "bearer", "api_key"], "description": "Default none." },
                        "auth_header": { "type": "string", "description": "Header name when auth=api_key (default X-API-Key)." },
                        "secret": { "type": "string", "description": "Bearer/api-key material (server-side only)." }
                    }
                }),
            ),
            tool_entry(
                "list_endpoints",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: list registered remote-backend endpoints WITHOUT their \
                 secrets (name, kind, base_url, auth scheme, audit).",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "delete_endpoint",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: remove a registered remote-backend endpoint by name. \
                 No-op when absent.",
                json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": { "name": { "type": "string" } }
                }),
            ),
            tool_entry(
                "validate_endpoints",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: probe every registered remote-backend endpoint for \
                 reachability; an unreachable endpoint's instances read closed.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "create_remote_instance",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: materialise a remote (openapi/mcp) instance — the \
                 binding comes from the skill's backend block (overlay page + \
                 backend_ref; data is fetched live on expand).",
                json!({
                    "type": "object",
                    "required": ["skill", "id"],
                    "properties": {
                        "skill": { "type": "string", "description": "A skill declaring backend.kind=openapi|mcp." },
                        "id": { "type": "string", "description": "New instance id." },
                        "overlay_body": { "type": "string", "description": "Optional overlay markdown body." }
                    }
                }),
            ),
            tool_entry(
                "write_instance",
                Execution::Orchestration,
                Scope::Agent,
                "Write-back to a remote (openapi/mcp) instance's upstream. \
                 Gated by the target instance's acl.update; a binding with no \
                 write op is refused.",
                json!({
                    "type": "object",
                    "required": ["ref"],
                    "properties": {
                        "ref": { "type": "string", "description": "Target instance id or [[skill::id]]." },
                        "payload": { "type": "object", "description": "Fields forwarded to the upstream write op." }
                    }
                }),
            ),
            // Admin tenant-lifecycle + operator tools. All require an
            // admin-role bearer (JSON-RPC -32001 otherwise) and a
            // `tenant_id` naming this single-tenant gateway's tenant
            // (-32002 on a mismatch).
            tool_entry(
                "tenant_create",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: provision a tenant (directory + DuckDB file).",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "display_name": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "tenant_list",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: list all tenants in the tenant store.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "tenant_get",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: fetch one tenant's spec.",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "tenant_update",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: partial-update a tenant's spec — display_name, status \
                 (active|suspended), quotas, embedding_provider. Changing \
                 embedding_provider requires a rebuild (`rebuild_required` in the \
                 response).",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "display_name": { "type": "string" },
                        "status": { "type": "string", "enum": ["active", "suspended"] },
                        "quotas": {
                            "type": "object",
                            "properties": {
                                "queries_per_minute": { "type": "integer" },
                                "writes_per_minute": { "type": "integer" },
                                "embeds_per_minute": { "type": "integer" },
                                "concurrent_sessions": { "type": "integer" },
                                "max_blob_bytes": { "type": "integer" }
                            }
                        },
                        "embedding_provider": {
                            "type": "object",
                            "required": ["provider"],
                            "properties": {
                                "provider": { "type": "string", "enum": ["zero", "gemini", "embeddinggemma"] },
                                "model": { "type": "string" },
                                "dim": { "type": "integer" }
                            }
                        }
                    }
                }),
            ),
            tool_entry(
                "tenant_delete",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: delete a tenant and its on-disk state. Destructive — \
                 requires `confirm` equal to the tenant id.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "confirm"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "confirm": {
                            "type": "string",
                            "description": "Must equal tenant_id to proceed."
                        }
                    }
                }),
            ),
            tool_entry(
                "tenant_export",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: export a tenant's canonical markdown as a base64 \
                 tar+gz blob (`tarball_b64` + `bytes`).",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "export_pack",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: build a versioned, HMAC-signed skill pack (a \
                 deterministic tar+gz of the named skills' pages + a signed \
                 manifest) — the unit of distribution between escurel nodes. \
                 Requires ESCUREL_PACK_SECRET; fails closed on \
                 credential-shaped content.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "id", "version", "vertical", "publisher", "skills"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "id": { "type": "string", "description": "Pack identity, e.g. logistics-midmarket." },
                        "version": { "type": "integer", "description": "Monotonic pack version." },
                        "vertical": { "type": "string", "description": "The vertical this pack belongs to." },
                        "publisher": { "type": "string", "description": "Publisher identity, e.g. hub.stuttgart-ai." },
                        "skills": {
                            "type": "array", "items": { "type": "string" },
                            "description": "Skill ids whose pages form the pack subtree."
                        },
                        "include_instances": {
                            "type": "boolean",
                            "description": "Also bundle each skill's instance pages (edge-case libraries). Default false."
                        }
                    }
                }),
            ),
            tool_entry(
                "import_pack",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: import a signed skill pack as this tenant's pinned, \
                 read-only base layer. Verifies signature + content hash \
                 fail-closed before unpacking; refuses silent version \
                 changes (pack_version_pinned) and cross-vertical mixing \
                 (vertical_mismatch, overridable).",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "manifest", "tarball_b64"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "manifest": { "type": "object", "description": "The signed pack.manifest.json object." },
                        "tarball_b64": { "type": "string", "description": "The pack tarball, base64." },
                        "allow_vertical_mismatch": {
                            "type": "boolean",
                            "description": "Explicitly permit subscribing across verticals. Default false."
                        }
                    }
                }),
            ),
            tool_entry(
                "list_packs",
                Execution::Deterministic,
                Scope::Admin,
                "Admin: the subscribed skill packs and their pinned versions.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "unsubscribe_pack",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: drop a pack subscription — removes every base page it \
                 landed and the version pin; tenant overlays survive.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "pack_id"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "pack_id": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "rebase_pack",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: the reviewed upgrade of a subscribed pack — the only \
                 operation that moves a version pin. Shadow-vs-upstream \
                 conflicts surface as rebase_conflict Issues and block until \
                 acknowledge_conflicts=true; orphaned base pages are removed.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "manifest", "tarball_b64"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "manifest": { "type": "object", "description": "The signed manifest of the NEW version." },
                        "tarball_b64": { "type": "string", "description": "The new version's tarball, base64." },
                        "acknowledge_conflicts": {
                            "type": "boolean",
                            "description": "Apply despite rebase_conflict Issues (the human review). Default false."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Plan only: run the full validation + conflict scan, apply nothing, and report {would_import, would_remove}. Default false."
                        }
                    }
                }),
            ),
            tool_entry(
                "submit_promotion",
                Execution::Orchestration,
                Scope::Admin,
                "Admin/curator: propose a scrubbed pack candidate from this \
                 node's own promotable skills (the L2→L3 harvest). Default-deny: \
                 skills-only, curator-marked `promotable: true`, tenant-authored; \
                 fail-closed on credential-shaped content; emits an immutable \
                 audit event. A hub curator reviews + publishes deliberately.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "candidate_id", "vertical", "skills"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "candidate_id": { "type": "string", "description": "Candidate pack identity for hub review." },
                        "vertical": { "type": "string", "description": "The vertical the candidate belongs to." },
                        "skills": {
                            "type": "array", "items": { "type": "string" },
                            "description": "Promotable skill ids to harvest."
                        }
                    }
                }),
            ),
            tool_entry(
                "tenant_import",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: import a tenant's markdown from a base64 tar+gz blob \
                 into an existing tenant; returns `bytes_imported`.",
                json!({
                    "type": "object",
                    "required": ["tenant_id", "tarball_b64"],
                    "properties": {
                        "tenant_id": { "type": "string" },
                        "tarball_b64": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "rebuild",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: rebuild the tenant's index from canonical markdown; \
                 returns the final `{done, total}` page counts.",
                json!({
                    "type": "object",
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." }
                    }
                }),
            ),
            tool_entry(
                "compact_lanes",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: compact the tenant's CRDT op lanes; returns \
                 `{ops_compacted, bytes_reclaimed}`.",
                json!({
                    "type": "object",
                    "required": ["tenant_id"],
                    "properties": { "tenant_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "publish_snapshot",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: trigger a DuckLake publish of this writer's current \
                 index state, then prune old snapshots down to \
                 `ESCUREL_SNAPSHOT_KEEP`. A no-op (`skipped: true`) when \
                 nothing changed since the last publish. Unavailable on a \
                 non-ducklake gateway or a ducklake reader replica.",
                json!({
                    "type": "object",
                    "properties": {}
                }),
            ),
            tool_entry(
                "attach_external",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: attach an external read-only DuckDB source; the \
                 catalog alias is derived from `source_url` and returned as \
                 `source_id`.",
                json!({
                    "type": "object",
                    "required": ["source_url"],
                    "properties": {
                        "tenant_id": { "type": "string", "description": "Must match this gateway's tenant." },
                        "source_url": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "embedding_reload",
                Execution::Orchestration,
                Scope::Admin,
                "Admin: hot-reload the embedding model from the captured \
                 config; returns the new `model_revision`.",
                json!({ "type": "object", "properties": {} }),
            ),
        ]
    })
}

/// Whether a tool's result is reproducible compute or a step that advances
/// loop state (REQ-LABEL-01, WI-8).
///
/// `Deterministic` = a pure function of KB state + arguments: reads, queries,
/// validation, pack/bundle builds. `Orchestration` = everything that advances
/// loop state: writes, events, sessions, lifecycle. Live network probes (the
/// binding/endpoint validators) and server runtime state (the quota snapshot,
/// the webhook delivery log) are `Orchestration` — they are not functions of
/// KB state, however read-only they look.
///
/// The label is a contract, not a comment: a per-phase tool surface can hand a
/// compute step deterministic tools only, so that "the LLM never does critical
/// arithmetic" is enforceable rather than aspirational.
///
/// This used to be a `DETERMINISTIC_TOOLS: &[&str]` list living 900 lines away
/// from the definitions it labelled, matched by string. A tool could be added
/// with no entry and silently take the default; a renamed tool left a dead
/// entry nothing detected. Now the label is a required argument at the
/// definition site, so neither is expressible. That is R2 of
/// `docs/notes/complexity-reduction-plan.md`.
///
/// Note the deliberate trade: the old list was *fail-closed* (an unlisted tool
/// defaulted to `orchestration`), and a required argument has no default at
/// all. Fail-closed protects against forgetting; a required argument makes
/// forgetting impossible, which is strictly better — but it does mean the
/// author must decide, and `tool_label_map.rs` pins the answer so a careless
/// choice shows up as a changed line rather than as nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Execution {
    Deterministic,
    Orchestration,
}

impl Execution {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Orchestration => "orchestration",
        }
    }
}

/// Who can actually call a tool: the ordinary agent surface, or the
/// `require_admin`-gated operator surface. `tools/list` filters by this
/// for agent-role callers — an agent no longer receives 41 schemas that
/// can only ever answer `-32001`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Agent,
    Admin,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Admin => "admin",
        }
    }
}

/// The `{ok, issues[]}` refusal envelope every write tool returns —
/// declared once, referenced per tool via [`output_schema_for`].
/// Dispatch-level alias → canonical tool name (API review B1): the
/// verb-first spellings of the noun-first stragglers. Returns `None`
/// for anything already canonical (or unknown). These are COURTESY
/// spellings only — `tools/list` advertises the canonical names, and
/// the registry-conformance ratchets see only canonical arms.
pub(crate) fn canonical_tool_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "create_tenant" => "tenant_create",
        "list_tenants" => "tenant_list",
        "get_tenant" => "tenant_get",
        "update_tenant" => "tenant_update",
        "delete_tenant" => "tenant_delete",
        "export_tenant" => "tenant_export",
        "import_tenant" => "tenant_import",
        "reload_embedding" => "embedding_reload",
        _ => return None,
    })
}

/// The `scope: "admin"` tool names, memoised once from the same
/// declarations `tools/list` serves — the quota exemption keys on this
/// so it can never drift from the dispatch gate (the registry ratchet
/// pins `scope` to `require_admin`).
pub(crate) fn admin_scope_tools() -> &'static std::collections::HashSet<String> {
    static SET: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        tools_list_payload()["tools"]
            .as_array()
            .map(|ts| {
                ts.iter()
                    .filter(|t| t["scope"] == "admin")
                    .filter_map(|t| t["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn write_envelope_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string" },
                        "code": { "type": "string" },
                        "location": { "type": "string" },
                        "message": { "type": "string" },
                        "suggestion": { "type": "string" }
                    }
                }
            }
        },
        "additionalProperties": true
    })
}

/// MCP `outputSchema` for the tools whose result shape is a load-bearing
/// contract (API review R2). Coverage is deliberate, not exhaustive:
/// every write tool declares the shared `{ok, issues[]}` envelope, the
/// core reads declare their top-level keys, and tools whose results are
/// still ad-hoc `json!` literals stay undeclared rather than lying.
fn output_schema_for(name: &str) -> Option<Value> {
    let obj = |props: Value| json!({ "type": "object", "properties": props, "additionalProperties": true });
    Some(match name {
        "update_page" | "delete_page" | "move_page" | "purge_page" | "write_instance"
        | "apply_op" | "close_session" | "import_pack" | "rebase_pack" => write_envelope_schema(),
        "validate" => obj(json!({ "issues": { "type": "array" } })),
        "search" => obj(json!({
            "hits": { "type": "array" },
            "granularity": { "type": "string" }
        })),
        "list_skills" => obj(json!({ "skills": { "type": "array" } })),
        "list_instances" => obj(json!({
            "instances": { "type": "array" },
            "next_cursor": { "type": ["string", "null"], "description": "string = more rows (pass back as cursor); null = done" }
        })),
        "list_inbox" | "list_events" => obj(json!({
            "events": { "type": "array" },
            "next_cursor": { "type": "string", "description": "present iff rows lie past the page; absence (only) means done" }
        })),
        "list_messages" => obj(json!({
            "messages": { "type": "array" },
            "next_cursor": { "type": "string" }
        })),
        "capture_event" | "assign_event" => obj(json!({
            "event_id": { "type": "string" }
        })),
        "fetch_blob" => obj(json!({
            "blob": { "type": ["object", "null"] }
        })),
        "expand" => obj(json!({
            "page": { "type": ["object", "null"] },
            "frontmatter": { "type": "object" },
            "body": { "type": "string" },
            "blocks": { "type": "array" },
            "wikilinks_out": { "type": "array" }
        })),
        _ => return None,
    })
}

fn tool_entry(
    name: &str,
    execution: Execution,
    scope: Scope,
    description: &str,
    input_schema: Value,
) -> Value {
    let mut entry = json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        // WI-8 (REQ-LABEL-01): additive execution label. Declared here, at the
        // tool, rather than in a remote list keyed by name — see [`Execution`].
        "execution": execution.as_str(),
        // Additive scope label (2026-08-14 API review): which role can
        // actually call this tool. Declared at the definition site like
        // `execution` — and ratcheted against the dispatch arms by
        // `tool_registry_conformance`, so it cannot lie about the gate.
        "scope": scope.as_str(),
    });
    // Additive result contract where one is declared (see
    // `output_schema_for`); absent = the shape is not yet pinned.
    if let Some(os) = output_schema_for(name) {
        entry["outputSchema"] = os;
    }
    entry
}

/// [`tools_list_payload`] filtered for the caller's role: an agent-role
/// token sees only `scope: "agent"` entries — the tools it can actually
/// call. Admin — and verifier-less dev mode (`role: None`, treated as
/// admin everywhere else) — sees the whole surface.
pub(crate) fn tools_list_payload_for(role: Option<escurel_auth::Role>) -> Value {
    let payload = tools_list_payload();
    if !matches!(role, Some(escurel_auth::Role::Agent)) {
        return payload;
    }
    let tools: Vec<Value> = payload["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t["scope"] == json!("agent"))
        .collect();
    json!({ "tools": tools })
}

/// Build an OpenAPI 3.1 document describing escurel's tool surface — the
/// outbound half of the openapi/mcp story. The real transport is JSON-RPC 2.0
/// at `POST /mcp`, so the document has one path (`/mcp`) whose request body is
/// the JSON-RPC envelope, plus every tool's input schema under
/// `components.schemas.<tool>_input` and the tool-name enum. Generated from the
/// same [`tools_list_payload`] the MCP `tools/list` handshake serves, so the
/// two never drift.
pub(crate) fn openapi_document(version: &str) -> Value {
    let tools = tools_list_payload();
    let tool_arr = tools
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut per_tool_schemas: Vec<(String, Value)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for t in &tool_arr {
        if let Some(name) = t.get("name").and_then(Value::as_str) {
            names.push(name.to_owned());
            if let Some(schema) = t.get("inputSchema") {
                per_tool_schemas.push((format!("{name}_input"), schema.clone()));
            }
        }
    }
    let mut doc = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "escurel agent surface",
            "version": version,
            "description": "escurel exposes its agent + admin tools as MCP over \
                HTTP (JSON-RPC 2.0) at POST /mcp, plus a small REST surface \
                (document intake + blob download + ops probes). Each tool's \
                input schema is under components.schemas.<tool>_input; tools \
                with a pinned result shape carry outputSchema in tools/list.",
        },
        "security": [ { "bearerAuth": [] } ],
        "paths": {
            "/mcp": {
                "post": {
                    "summary": "JSON-RPC 2.0 tools/call (or tools/list) envelope",
                    "operationId": "mcp_call",
                    "security": [ { "bearerAuth": [] } ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/JsonRpcRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "JSON-RPC result, or error object whose data carries {code, retryable}" }
                    }
                }
            },
            "/ingest": {
                "post": {
                    "summary": "Ingest a blob already deposited in the tenant's inbox area",
                    "operationId": "ingest",
                    "security": [ { "bearerAuth": [] } ],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": {
                            "type": "object",
                            "required": ["blob_id", "content_type"],
                            "properties": {
                                "blob_id": { "type": "string" },
                                "content_type": { "type": "string" },
                                "title": { "type": "string" },
                                "skill": { "type": "string", "description": "Explicit target document skill; must accept the MIME (422 otherwise, create-ACL enforced)." },
                                "event_id": { "type": "string", "description": "Idempotency key: a redelivery answers {status: duplicate} without re-running extraction." }
                            }
                        } } }
                    },
                    "responses": {
                        "200": { "description": "{status, event_id, blob_id, page_id?, handler_skill?, chunk_count?, issue?}; status ∈ materialised|extraction_failed|no_handler|duplicate" },
                        "403": { "description": "tenant_suspended (agent tokens while suspended)" },
                        "422": { "description": "invalid_target_skill" },
                        "429": { "description": "rate_limited (Writes budget)" },
                        "503": { "description": "read_only_replica — retry against the writer" }
                    }
                }
            },
            "/ingest/upload": {
                "post": {
                    "summary": "Deposit base64 bytes into the inbox AND ingest in one call",
                    "operationId": "ingest_upload",
                    "security": [ { "bearerAuth": [] } ],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": {
                            "type": "object",
                            "required": ["content_type", "bytes_b64"],
                            "properties": {
                                "content_type": { "type": "string" },
                                "bytes_b64": { "type": "string" },
                                "title": { "type": "string" },
                                "skill": { "type": "string" },
                                "event_id": { "type": "string" }
                            }
                        } } }
                    },
                    "responses": {
                        "200": { "description": "same pipeline outcome as /ingest" },
                        "413": { "description": "payload_too_large (per-upload blob quota)" }
                    }
                }
            },
            "/blob/{page_id}": {
                "get": {
                    "summary": "The retained original bytes of a document instance, verbatim",
                    "operationId": "blob_get",
                    "security": [ { "bearerAuth": [] } ],
                    "parameters": [ { "name": "page_id", "in": "path", "required": true, "schema": { "type": "string" } } ],
                    "responses": {
                        "200": { "description": "raw bytes with the declared/sniffed Content-Type" },
                        "404": { "description": "absent, hidden, or blob-less — one indistinguishable answer" }
                    }
                }
            },
            "/healthz": {
                "get": {
                    "summary": "Dependency-free liveness probe",
                    "responses": { "200": { "description": "alive" } }
                }
            },
            "/readyz": {
                "get": {
                    "summary": "Component-reporting readiness probe",
                    "responses": { "200": { "description": "ready" }, "503": { "description": "components not ready (JSON body)" } }
                }
            },
            "/version": {
                "get": {
                    "summary": "Build version (text)",
                    "responses": { "200": { "description": "version string" } }
                }
            }
        },
        "components": {
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer", "bearerFormat": "JWT" }
            },
            "schemas": {
                "JsonRpcRequest": {
                    "type": "object",
                    "required": ["jsonrpc", "method"],
                    "properties": {
                        "jsonrpc": { "const": "2.0" },
                        "id": {},
                        "method": { "type": "string", "enum": ["tools/list", "tools/call"] },
                        "params": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "enum": names },
                                "arguments": { "type": "object" }
                            }
                        }
                    }
                }
            }
        }
    });
    if let Some(schemas) = doc
        .pointer_mut("/components/schemas")
        .and_then(Value::as_object_mut)
    {
        for (k, v) in per_tool_schemas {
            schemas.insert(k, v);
        }
    }
    doc
}

// --- helpers ---------------------------------------------------

pub(super) fn page_type_str(pt: PageType) -> &'static str {
    match pt {
        PageType::Skill => "skill",
        PageType::Instance => "instance",
    }
}
