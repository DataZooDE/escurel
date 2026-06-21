# Changelog — escurel-platform skill

The skill version tracks the consumer-facing contract, not the Escurel
binary version. The Escurel repo's checked-out git ref is the true version
pin (see `SKILL.md` → "How this skill is installed").

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
