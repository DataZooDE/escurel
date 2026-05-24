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

New ADRs are numbered sequentially (`0002-…`, `0003-…`). An ADR
is never edited after acceptance except to update its **Status**
line; superseding decisions get their own ADR that links back.
