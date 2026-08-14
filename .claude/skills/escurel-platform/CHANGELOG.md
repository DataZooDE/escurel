# Changelog — escurel-platform skill

The skill version tracks the consumer-facing contract, not the Escurel
binary version. The Escurel repo's checked-out git ref is the true version
pin (see `SKILL.md` → "How this skill is installed").

## 0.6.9 — `/ingest` gained dispatch-gate parity + an idempotency key

- `POST /ingest` and `POST /ingest/upload` accept an optional
  `event_id`: the caller's idempotency key, fed to `capture_event`'s
  dedup. A redelivered upload is acknowledged
  `{status: "duplicate", event_id, blob_id}` — one inbox event, no
  second extraction run (escurel#382). Absent = unchanged behaviour.
- The REST intake doors now enforce the same cross-cutting gates as MCP
  dispatch: a suspended tenant refuses agent tokens (403
  `tenant_suspended`), a ducklake reader refuses with 503
  `read_only_replica` (retry against the writer), and `/metrics` records
  the actual response status per route (a refused ingest is no longer
  counted as a 200).
## 0.6.8 — `append_message` `msg_id` is a real idempotency key

- `references/02-tool-surface.md`: a caller-supplied `msg_id` now
  deduplicates — a retry with the same `(chat_group_id, msg_id)` echoes
  the originally stored row (including the server-stamped `ts`) instead
  of inserting a second, microseconds-apart copy. Same contract as
  `capture_event`'s `event_id`. Offline clients can finally retry
  deliveries safely; without a `msg_id` the server still mints a ULID
  and every call appends.
## 0.6.7 — truth pass: discovery, streaming, wire-JSON claims corrected

- `references/03-consume-over-http-mcp.md`: three false claims fixed.
  `tools/list` is NOT role-filtered — every caller sees the whole
  surface (admin tools refuse at dispatch with `-32001`); there is NO
  SSE/streaming on `/mcp` (single JSON bodies only; use the WS
  `event_subscribe` push for wake-ups); JSON-bearing fields
  (`frontmatter`, `rows`, `params`) are real JSON on the wire, not
  `*_json` strings.
- `references/08-auth-and-tenancy.md`: same discovery correction;
  role lists updated (`purge_page` is admin; `admin_delete_chat_history`
  is the real tool name).
- `references/02-tool-surface.md`: `list_instances` row corrected
  (`skill_id` + `order_by='at asc'|'at desc'` — NOT `skill`/`order_by_at`);
  `expand` row corrected (`as_of`/`scenario`/`full`; `anchor`/`version`
  are long gone); `fetch_blob` row added; tool count corrected to 69.
- `expand` now advertises its `full` argument in the input schema (it was
  parsed but undocumented on the wire).
- **New mechanical guard**: `escurel-server/tests/suite/skill_doc_parity.rs`
  reconciles this skill's tool tables (names, input names, the tool count)
  against the live `tools/list` on every test run — the drift class the
  2026-08-02 audit documented can no longer land silently. Prose remains
  a write-time obligation.

## 0.6.6 — WS `event_subscribe`: the bus pushes to open sessions

- `references/11-event-driven-agents.md`: a consumer that cannot host the
  HTTP webhook subscribes over `GET /ws` — `event_subscribe` frame →
  `event_subscribe_ack` → `{type:"event", event:{…}}` pushes for every
  captured event the caller may read (`ESCUREL_EVENT_ACL`-filtered, same
  rule as `list_inbox`); `event_lagged` marks gaps (poll once to catch
  up). Subscription starts at now — no replay. Closes escurel#333.

## 0.6.5 — `assign_event` write-gates its target instance

- `references/02-tool-surface.md`: under `ESCUREL_EVENT_ACL=enforce`,
  `assign_event` now also requires the caller to be allowed to WRITE the
  target instance (`acl.update`), closing the visibility-laundering edge
  escurel#363 — previously only the event side was checked, so a caller
  could file their own capture into someone else's record and re-scope
  its audience. Refusals are `event not found`-shaped (no existence
  oracle); `log` mode warns and allows; a nonexistent target refuses the
  same way. `off` (the default) is unchanged.

## 0.6.4 — a rejected write is flagged via MCP `isError`

- `references/02-tool-surface.md`: a write refused with `ok:false` (a
  validation rejection, a stale `base_version`, a layer/ACL refusal, …)
  now also sets `isError: true` on the MCP `CallToolResult` envelope and
  logs `status: "rejected"` server-side. Previously the envelope said
  `isError: false` and only the nested payload carried the refusal, so a
  client following MCP conventions read it as success. The payload
  contract is byte-identical. `validate` results and `dry_run:true`
  results keep `isError: false` — they report, they don't refuse.

## 0.6.3 — `list_skills` is caller-scoped and no longer discloses group names

- `references/02-tool-surface.md`: the Tier-1 catalogue now filters. A skill
  whose declared `acl.read` does not intersect the caller's effective groups
  is **absent** from `list_skills`, so a consumer asserting "a skill I may
  not read is not in the palette" can now hold that against the server
  instead of re-filtering client side.
- Backward compatible for every tenant that has not opted in: a skill with
  no `acl:` block falls through to the tenant default (`read: [public]`) and
  stays visible, and a skill whose *instances* are owner-private
  (`visibility: owner`, or `acl.read: [owner]`) remains a discoverable
  *type* — `owner` is instance-grained and never hides a skill.
- **Security**: the per-CRUD `acl` object is now projected to an **admin**
  caller only. It previously went to every authenticated caller, which in a
  shared tenant handed out the group names — i.e. the customer roster and
  the authorisation topology — to anyone holding a valid token. `visibility`
  and `owner_field` are unchanged and still reported to everyone.

## 0.6.2 — Live sessions are multi-peer, and attaching is ACL-gated

- `references/02-tool-surface.md`: an `op` from one attached client now
  reaches the others as `peer_op`, and `presence` reaches other peers (live
  cursors). Previously a session was single-peer — a second device learned of
  edits only by asking again — so consumers were being pushed toward building
  their own relay.
- Records the two properties a client must design for: ACL is evaluated at
  attach (a mid-session revocation bites at the next attach), and there is no
  replay of missed frames (`resync_required` → re-`expand` and re-attach).
- **Security**: attaching to a session is now refused for a principal who may
  not read the page. It previously was not checked at all, so any
  authenticated tenant member who knew a session id could watch another
  principal's live editing on a page the ACL denies them.

## 0.6.1 — A committed live session is now readable

- `references/02-tool-surface.md`: `close_session(commit: true)` writes the
  merged body through to the store, so the committed text is immediately
  visible to `expand`, `search` and the link graph, and `final_version` is
  the head a subsequent write should pass as `base_version`.
- Records the failure mode for anyone pinned to an older node: a commit used
  to persist only CRDT history, so `final_version` advanced while the
  readable body did not, and a client writing back with the version it had
  just been handed silently overwrote its own session. The skill said
  nothing about what a commit did to the readable page, which is how a
  consumer could reasonably have assumed the safe behaviour.

## 0.6.0 — Event-driven agents: the runner was entirely undocumented

- New `references/11-event-driven-agents.md`. `escurel-runner` is a
  shipped binary that turns a captured event into an autonomous agent
  run — webhook `POST /trigger` (+ a poll backstop) → loop-control
  ledger → context packaging → a harness adapter spawning `claude -p`
  with the gateway as a scoped MCP server. The skill described only how
  an app CALLS escurel; this is escurel calling an agent, and it had no
  entry at all.
- Documents the consequence for skill authors: an event-triggered
  skill's **page body becomes the agent's system prompt**, so it must
  read as a procedure for a machine, not as reference prose for a human.
- Records the boundary honestly: the runner STARTS a run per event, but
  nothing can push into an already-open session — `/ws` carries no
  event-bus frame (#333), so polling `event inbox` is the only option
  for a live assistant.
- Names the trap that `ESCUREL_RUNNER_HARNESS` defaults to `echo`, so a
  runner started without it runs no LLM at all.

## 0.5.0 — Page layers + skill packs (curated federation)

- **Local iteration corrected (`09`).** The reference claimed the
  workspace built exactly one binary (the CLI) and that no server could
  be run locally; `escurel-server` is a `[[bin]]` and running it is the
  ordinary dev loop. Replaced with three options + a runnable recipe,
  and three traps: `ESCUREL_EMBEDDING_PROVIDER=zero` disables retrieval
  silently, `search` scores are reciprocal-rank fusion (not similarity),
  and the s3/gcs/duckvfs storage backends are cargo features a plain
  `cargo build` silently drops.
- `02`: `delete_page` in the write-tools table, documented as an
  ARCHIVE (retracted from discovery, markdown retained) rather than a
  destroy; plus a note that the tables are curated, not exhaustive.
- `04`: `page delete`, `provenance ancestry|path|drift|abandoned`,
  `workflow run|status|stop`, `ui`, and their CLI→tool map rows.
- `SKILL.md`: a banner stating the skill can be stale and that
  load-bearing details should be checked against a running system. The
  sync obligation is now a PR-cycle step in the repo's `CLAUDE.md`.

- New **Layer/stability axis** in `01`: every page is `overlay`
  (tenant-authored, editable, the default) or `base@<pack>@v<N>`
  (imported from a subscribed skill pack, read-only — `layer_read_only`
  on `update_page`, `-32000 layer_read_only:` on `open_session`; the
  `markdown/base/` page-id namespace is reserved). Overlay **shadowing**
  of a base skill (curator-gated, `shadow_requires_curator`; `resolve`
  prefers the overlay, `list_skills` shows one entry with
  `layer` + `shadows`, `expand` carries a `shadow` object) and the
  curator-set `promotable: true` marker
  (`promotable_requires_curator`).
- New **Skill packs (admin)** section in `02`: `export_pack` /
  `import_pack` / `list_packs` / `rebase_pack` (`rebase_conflict` +
  `acknowledge_conflicts`) / `unsubscribe_pack` / `submit_promotion`
  (default-deny, scrubbed, audit-evented, version-0 candidate), the
  shared-secret HMAC trust model, and the full refusal-code inventory.
- `02` also documents the `execution: "deterministic" | "orchestration"`
  label on every `tools/list` entry (orchestration is the fail-closed
  default).
- `04`: the `escurel admin pack
  export|import|list|rebase|unsubscribe|submit-promotion` subcommands +
  map rows. `06`: the hub↔spoke two-process pack-test recipe
  (`ConfigOverrides.pack_secret`; worked version in
  `crates/escurel-server/tests/pack_import.rs`). `09`: the
  `escurel_writes_total{tenant,origin}` absorption metric.
- `SKILL.md` + `10`: the cross-tenant prohibition re-scoped — runtime
  calls never span tenants; curated pack publish/subscribe is the
  shipped federation layer.

## 0.4.1 — CLI docs realigned to the noun-grouped command surface

- `references/04` rewritten: the CLI is grouped gh/aws-style (`escurel
  skill list`, `escurel page expand`, `escurel query run`, …), not the old
  flat `escurel list-skills` style. Added the four commands that had no
  docs — `page blob` (`fetch_blob`), `page snapshots` (`list_snapshots`),
  `session open|apply|close`, and `ingest` (POST `/ingest/upload`) — plus a
  CLI→tool map, the `--format` flag, stdin-body list, and the create-ACL
  gotcha on `ingest --skill`.
- Noted the **parity guard** (`crates/escurel-cli/tests/cli_parity.rs`):
  every agent-role tool must have a CLI command, so the map can't drift;
  the admin/ops provisioning MCP-twins are deliberately CLI-less.
- Fixed the same stale flat-command style in `references/06`, `07`, `09`.
- `SKILL.md` tool enumeration gains `fetch_blob` + `list_snapshots`.

## 0.4.0 — query pages + the event bus

- `query_instance` documented as THE structured-data read (an authored
  `[[query::<id>]]` page: `{{target}}` allow-list substitution, `:param`
  prepared-statement binding, per-caller ACL on the target, server row
  cap). `run_stored_query` marked legacy.
- New **Event tools** section in `02`: `capture_event` / `assign_event` /
  `list_events` / `list_inbox`, the tenant HMAC webhook, the
  capture+assign-for-timeline rule, and event-id idempotency.
- `create_sql_instance` / `attach_external` documented as post-boot admin
  materialisation (seeding never does it).

## 0.3.0 — External instance backends (SQL views + Document/RAG)

- New **Backend axis** in the data model: a skill may declare
  `backend: { kind: markdown | sql_view | document }`, so its instances are
  sourced from outside markdown. `list_skills` now reports each skill's
  `backend.kind` + a `capabilities` object (`writable`, `granularity`,
  `search`, `supports_crdt`).
  - `sql_view` — read-only DuckDB view over an attached relational source.
    `expand` returns the overlay + a bounded row projection
    (`backend_projection`). Admin-gated lifecycle: `create_sql_instance`,
    `register_credential` / `list_credentials` / `delete_credential`,
    `validate_bindings` (schema-drift → `binding_degraded`, reads fail-closed).
  - `document` — PDF/DOCX/PPTX/XLSX/text uploaded via `POST /ingest` /
    `POST /ingest/upload`, extracted (kreuzberg, default-on) + chunked +
    embedded into a page-with-chunks. `expand` returns the overlay + top-k
    chunks (`chunks_total` / `chunks_truncated`), never the full text.
  - Both backends are read-only: `update_page` / `apply_op` → `backend_read_only`.
- Docs: `references/01` (Backend axis) + `references/02` (Instance backends).
  Full wire contract in the repo's `docs/spec/protocol.md` § Instance backends.

## 0.2.0 — M-Chat: per-chat-group conversation history (issue #63)

- Agent tool surface bumped **12 → 14**. New tools:
  - `append_message(chat_group_id, role, content, [author, ts, metadata,
    msg_id, embed=true])` → `{msg_id, ts}` — append-mostly log keyed
    by an opaque consumer-defined `chat_group_id`. Debits the Writes
    quota. Embedding is opt-out per call.
  - `list_messages(chat_group_id, [since, until, limit=100, cursor,
    direction='desc'])` → `{messages[], next_cursor?}` — time-ordered
    read with half-open `[since, until)` interval and `(ts, msg_id)`
    cursor pagination. Debits Queries.
- Admin RPC: `EscurelAdmin.DeleteChatHistory(tenant_id,
  [chat_group_id, before_ts])` — GDPR right-to-erasure + retention
  pruning. No agent-facing chat-delete tool by design.
- `references/02-tool-surface.md` gains a "Chat tools" section that
  documents the opt-out embedding policy, the opaque `chat_group_id`
  contract, and the admin-only delete path. Pointers from
  `references/03`, `references/05`, `references/08` updated.
- Distinct from `update_page`: chat does **not** rewrite a page or
  embed every block. Routing chat through `update_page` is now an
  explicit anti-pattern.
- ADR: `docs/adr/0002-chat-message-surface.md` in the escurel repo.

## 0.1.0 — initial release

- Progressive-disclosure index over eleven references covering both
  consumption styles (over-the-wire / CLI; the typed Rust path) and both
  emphases (designing the tenant data model; the dev/test loop).
- Conceptual layer: what Escurel is (`references/00`), the skill/instance
  data model + the kind/time/origin axes + the mandatory `escurel`
  meta-skill (`references/01`), and the twelve agent tools
  (`references/02`).
- Consumption paths: MCP-over-HTTP + gRPC (`references/03`), the `escurel`
  CLI (`references/04`), and the Rust `escurel-client` crate
  (`references/05`).
- Dev loop: no-mock integration tests with `escurel-test-support`
  (`references/06`), fixture seeding through the public write path
  (`references/07`), auth/tenancy with `AuthMode`/`mint_token`
  (`references/08`), and local iteration given there is no standalone
  `serve` binary yet (`references/09`).
- Hard prohibitions, the operator/admin boundary, and cross-references to
  `triton-platform` and `substrate-platform` (`references/10`).
- References navigate to the canonical spec under `docs/` and the source
  under `crates/` / `examples/` (resolved through the symlink into the
  Escurel checkout) rather than restating them. No `templates/` — the
  references point at `examples/echo-app/` as the thing to copy.
