# The CRDT backend's second `Connection::open` clobbers chat_messages

**Date:** 2026-06-18
**Scope:** `escurel-server` binary (`config.rs` `EscurelConfig::build`)

## Symptom

In deployed nonprod, per-chat-group history never round-tripped: consumers
(dz-agent-template AND dz-carl) saw `list_messages` return empty even though
`append_message` returned `status:ok`. A snapshot of the deployed tenant DB
showed `chat_messages` had **0 rows** (while `pages`/`blocks` were intact), i.e.
appends committed in-process but were LOST — confirmed across a restart.

It did NOT reproduce via `EscurelProcess` (in-process, single directly-built
indexer, no CRDT) nor in any `Indexer`-direct unit test. It reproduced ONLY by
booting the real `escurel-server` binary, appending, restarting, and re-reading.

## Cause

`EscurelConfig::build` opened the CRDT backend on a **second
`Connection::open(&db_path)`** — a SEPARATE DuckDB *database instance* on the
same file (its own buffer manager + WAL), not a second connection to the same
instance. The two instances' checkpoints race; the CRDT instance, which never
sees the indexer instance's `chat_messages` inserts, clobbers them when it
checkpoints. The earlier notes
([2026-05-24-duckdb-second-connection-stale](2026-05-24-duckdb-second-connection-stale.md),
[2026-05-26-server-binary-crdt-second-connection](2026-05-26-server-binary-crdt-second-connection.md))
predicted exactly this; the "disjoint tables → safe" assumption did not hold for
the indexer's own table writes (chat) once both instances checkpoint a
long-running process.

## Fix

Open the CRDT connection as `conn.try_clone()` — a second connection to the
**already-opened** database (one instance, shared MVCC/WAL) — instead of a
second `Connection::open`. One-line wiring change in `config.rs`. Both the
indexer and the CRDT backend now share one instance, so neither clobbers the
other.

## Regression test

`crates/escurel-server/tests/binary_boots.rs::chat_history_survives_server_restart`
boots via the real config path, appends, **shuts down, reboots on the same data
dir**, and asserts the messages survive. Red before the fix (`messages:[]`,
left=0/right=2), green after.

## How to recognise

Any "writes commit but vanish after restart / are invisible to a fresh open" on
a DuckDB-backed table, when the binary opens more than one `Connection::open` on
the same file. The rule: **never open the same DuckDB file twice; share one
connection (`try_clone`) or one `Arc<Mutex<Connection>>`.**
