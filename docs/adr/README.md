# Architecture Decision Records

This directory holds the load-bearing architectural decisions
the v1 spec rests on. ADRs follow the
[MADR](https://adr.github.io/madr/) format: each record states
the *context*, the *decision*, the alternatives considered, and
the *consequences*.

The spec files in [`../spec/`](../spec/) reflect the *outcome* of
these decisions; the ADRs preserve the *reasoning* and the
acceptance gates that apply.

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-duckdb-only-storage.md) | DuckDB-only per-tenant storage | Accepted — pre-deployment gate open |
| [0002](0002-chat-message-surface.md) | Per-chat-group conversation history (chat_messages) | Accepted |
| [0003](0003-capture-webhook-hmac-auth.md) | Authenticated capture webhook (HMAC-SHA256 + tenant identity) | Accepted |
| [0004](0004-rbac-groups.md) | Group/role-based per-instance ACL (group ACL v1) | Accepted |
| [0005](0005-page-layer-model.md) | The page `layer` model (pinned base vs editable overlay) | Accepted |

New ADRs are numbered sequentially (`0002-…`, `0003-…`). An ADR
is never edited after acceptance except to update its **Status**
line; superseding decisions get their own ADR that links back.
