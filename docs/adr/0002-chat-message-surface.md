# ADR-0002 — Per-chat-group conversation history (chat_messages)

**Status:** Accepted, 2026-05-26.
**Issue:** [#63](https://github.com/DataZooDE/escurel/issues/63).
**Lands as:** M-Chat (PRs #66, #68, and the M-Chat.3 PR that
introduces this ADR).

## Context

Carl — DataZoo's Stuttgart.ai community agent — needs durable,
inspectable per-chat-group conversation history with a ~30-day
retention window and GDPR-style right-to-erasure. The twelve agent
tools defined by [ADR-0001](0001-duckdb-only-storage.md) and the
[agent interface contract](../contract/agent-interface.md) model
**typed, distilled knowledge** (skills + instances + per-block HNSW
embeddings). Routing raw chat through them is wrong on three axes:

- **`update_page` is whole-page.** Appending one message rewrites the
  full body of the page that holds the conversation. The write cost
  is O(history), not O(1).
- **Every block is embedded.** Hybrid search treats every authored
  block as recall material. Raw chat messages rarely justify the
  embedding cost; high-volume sources (community spaces, event days)
  drive that cost into the bottleneck.
- **No append/read-back primitive.** The twelve agent tools have no
  shape for "give me the last N messages in chat group X, time-
  ordered" — it has to be expressed as a `run_stored_query` against
  an authored `[[query::*]]` instance, which assumes per-page
  authoring.

Carl's projected peak is ~50 msg/sec across concurrent users on
event-day load.

## Decision

Introduce a first-class **conversation history** surface, distinct
from the typed-instance KB but living in the same per-tenant DuckDB
file. The shape:

- **`chat_messages` table** with PK `(chat_group_id, ts, msg_id)`,
  HNSW on a nullable `dense_vec FLOAT[768]`. `chat_group_id` is
  opaque to escurel — the consumer (e.g. Carl) owns the identifier
  scheme.
- **Two new agent tools** (MCP + gRPC):
  - `append_message(chat_group_id, role, content, [author, ts,
    metadata, msg_id, embed])` → `{msg_id, ts}`. Debits the **Writes**
    quota dimension.
  - `list_messages(chat_group_id, [since, until, limit, cursor,
    direction])` → `{messages: [...], next_cursor?}`. Debits **Queries**.
- **One new admin RPC** (gRPC only):
  - `EscurelAdmin.DeleteChatHistory(tenant_id, [chat_group_id,
    before_ts, author])` → `{deleted}`. Admin-role required. Powers both
    retention pruning and GDPR right-to-erasure (a whole group *or* a
    single member). MCP twin: `admin_delete_chat_history`.

Agent contract size: **12 → 14 tools**. The contract doc + skill doc
are bumped in the same PR sequence (`docs/contract/agent-interface.md`
gains an "Append / read-back" axis; the `escurel-platform` skill
documents the consumer-side recipe).

> **Update (2026-07-06):** gRPC / `escurel-proto` has since been
> retired — escurel is HTTP-only (MCP-over-HTTP + WebSocket). The
> "MCP + gRPC" and "gRPC only" transport wording above is historical:
> the shipped surface is **MCP-only** — `append_message`,
> `list_messages`, and the admin `admin_delete_chat_history` (there is
> no `EscurelAdmin.DeleteChatHistory` gRPC RPC). The *decision* — a
> first-class per-chat-group conversation surface with admin-only
> erasure — is unchanged; only the transport is.

### Embedding policy

Embedding is **opt-out per call** via `embed: bool` (default `true`).
The `vss` HNSW index tolerates NULL rows in the indexed column —
empirically verified in
[`docs/notes/discovered/2026-05-25-vss-hnsw-tolerates-null-rows.md`](../notes/discovered/2026-05-25-vss-hnsw-tolerates-null-rows.md)
— so a single-table layout works: non-embedded rows hold
`dense_vec = NULL`, and any future similarity-search path filters
with `WHERE dense_vec IS NOT NULL`.

### Erasure / retention

Deletion is **admin-only**. The agent surface has no delete tool by
design: GDPR erasure and 30-day pruning are operator concerns and
should not be exposed to the chat-write path. The single admin RPC
covers all combinations; the three filters (`chat_group_id`,
`before_ts`, `author`) compose with **AND**:

| `chat_group_id` | `before_ts` | `author` | Effect                              |
|-----------------|-------------|----------|-------------------------------------|
| set             | unset       | unset    | Erase one chat group (GDPR)         |
| unset           | unset       | set      | Erase one member across all groups (GDPR right-to-erasure) |
| set             | unset       | set      | Erase one member within one group   |
| set             | set         | —        | Prune one group's history < cutoff  |
| unset           | set         | —        | Prune all groups' history < cutoff  |
| unset           | unset       | unset    | Nuke the tenant's chat log entirely |

The `author` filter closes the per-member erasure gap raised in
[issue #63](https://github.com/DataZooDE/escurel/issues/63): a consumer
operator can honour a single member's right-to-erasure without dropping
the rest of a shared group's conversation.

Scheduling — the periodic cron that calls `DeleteChatHistory(before_ts =
now - 30d)` — lives **outside escurel**, in the consumer (Carl) or as
a substrate periodic job. Escurel ships the building block; it does
not bake a fixed retention window into the platform.

### Why not the instance pattern

We considered modelling chat as a `chat::<group_id>` skill instance
with one block per message, appended via `apply_op`. The
shape works in principle but inherits all three of the issues this
ADR exists to address: page-rewrite semantics, mandatory embedding,
and no clean "give me the last N messages" primitive without a
stored query per group. Carl's peak load (~50 msg/sec) compounds the
write-lock contention with page updates and CRDT ops.

### Why not external attachment (DuckLake / Iceberg)

The spec's own scaling guidance
(`docs/spec/storage.md §Event volume`) recommends external attachment
above ~1M events. Carl's projected volume — ~50 msg/sec peak,
30-day retention — sits well under that ceiling for any single
tenant. Inline `chat_messages` keeps the operational surface to one
DuckDB per tenant; the attachment path is available later if a
deployment grows past the inline budget.

## Consequences

- **Agent contract bump.** The published twelve-tool list grows to
  fourteen. Consumers that pin against the contract doc must be
  updated; the wire is additive (no breaking changes to existing
  tools).
- **Schema stage bump.** `Migrator::up()` adds a fourth stage
  (`0002_chat_messages.sql`). The v1 migrator is still one-shot;
  incremental migrations are a follow-up.
- **Quota dimensions.** `append_message` debits **Writes**;
  `list_messages` debits **Queries**. No new dimension introduced.
  Embedding cost is folded into the existing Writes budget rather
  than carved off to **Embeds** (consistent with `update_page`).
- **Per-tenant write-lock contention.** Chat appends serialise with
  page updates + CRDT ops on the per-tenant DuckDB mutex. At 50
  msg/sec with embedding enabled the embed call dominates (~10–50 ms).
  Opt-out (`embed: false`) is the relief valve for high-volume
  sources; if contention bites in production a per-tenant chat-only
  secondary connection is the natural next step (out of scope here).
- **Retention scheduling is the consumer's responsibility.** Escurel
  exposes `DeleteChatHistory(before_ts = T)`; the cron that calls it
  daily lives in Carl (or as a substrate periodic job).

## Open follow-ups

- Per-tenant indexer routing. The current gateway holds a single
  `Indexer` per `AppState`, so all tenants share one DuckDB at the
  indexer layer. The chat tools inherit this; cross-tenant isolation
  is a gateway-wide workstream, not an M-Chat scope.
- Hybrid retrieval over chat (semantic recall of past messages). The
  HNSW index is present; a `search_messages` tool would be additive
  if Carl ever wants it.
- Streaming `list_messages` (server-stream gRPC) for long history
  windows. Today's paginated reply is sufficient for the 30-day window.
