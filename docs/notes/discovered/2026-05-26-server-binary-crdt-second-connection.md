# Server binary wires the CRDT backend off a second DuckDB connection

**Date:** 2026-05-26
**Scope:** `escurel-server` binary (`config.rs` `EscurelConfig::build`)

## Symptom / tension

`Indexer::new` takes an **owned** `duckdb::Connection` and wraps it in
its own private `tokio::sync::Mutex<Connection>` (no accessor exposes
it). `DuckdbCrdtBackend::new` wants an `Arc<Mutex<Connection>>`. The
backend module note
(`2026-05-24-duckdb-second-connection-stale.md`) says production should
share **one** `Arc<Mutex<Connection>>` across the indexer and the
backend so reads see writes.

Those two facts are in direct conflict under today's APIs: there is no
way to both hand the indexer its owned connection *and* share the same
connection (as an `Arc<Mutex<…>>`) with the CRDT backend.

## What the binary does (and why it's safe here)

`EscurelConfig::build` opens **two** connections to the same
`escurel.duckdb`: one owned by the `Indexer`, one wrapped in
`Arc::new(Mutex::new(_))` for `DuckdbCrdtBackend`.

The stale-read trap in the older note is specifically about a reader on
one connection failing to see a *writer's* recent commits to the
**same tables** (`pages`/`links`/`blocks`). The indexer and the CRDT
backend write **disjoint** table sets — the indexer never touches
`crdt_ops`/`crdt_snapshots`, and the backend never touches
`pages`/`links`/`blocks`. So no cross-connection read ever needs to
observe the other connection's writes. DuckDB's single-writer rule is
still respected because the two connections never write the same row.

## How to recognise / when to revisit

If a future feature makes the indexer and the CRDT backend touch a
shared table (e.g. an audit row written by both), this two-connection
split becomes unsafe and you'll see the classic stale-count symptom
from the 2026-05-24 note. The proper fix at that point is to give
`Indexer` a constructor that accepts a shared
`Arc<Mutex<Connection>>` (or a small connection-pool handle) and thread
the *same* handle into `DuckdbCrdtBackend::new`. That is a deliberate
escurel-internal API change, out of scope for the M5-bin PR.
