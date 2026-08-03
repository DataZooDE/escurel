# Changelog — escurel-platform skill

The skill version tracks the consumer-facing contract, not the Escurel
binary version. The Escurel repo's checked-out git ref is the true version
pin (see `SKILL.md` → "How this skill is installed").

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
