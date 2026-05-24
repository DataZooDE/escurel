# DuckDB: a second `Connection::open` to the same file may not
# see the first connection's recent commits

**Date:** 2026-05-24
**Scope:** `escurel-index` test design; eventual M3 admin tooling

## Symptom

Inside an integration test for `Indexer::rebuild`, the assertion
`count_pages == 2` failed with `left: 3, right: 2`, even though
the indexer's own `audit()` (which uses the indexer's connection)
returned a clean diff right before. Diagnostic prints:

```
[diag] after delete, store listed 2 keys (correct)
[diag] after rebuild, count_pages via second Connection::open = 3
[diag] audit drift (via indexer conn) = clean
```

The test helper `count_pages` was opening a **fresh**
`Connection::open(path)` to count rows. The indexer's connection
saw 2 (correct); the fresh second connection saw 3 (stale).

## Cause

DuckDB's concurrent-access model: only one writer at a time per
file, and a second connection opened mid-test does not necessarily
see the writer's most recent commits without an explicit
`PRAGMA` `CHECKPOINT` (or until the writer closes).

The first `count_pages` call succeeded against the same file
*before* any rebuild ran, because the writer had committed and
the second connection's opening view happened to align. After a
burst of write transactions (`DELETE … DELETE … INSERT … INSERT
…` from `rebuild`), the second connection's snapshot lagged.

## Fix in this PR

Stop opening a second `Connection::open` in test helpers; always
query through the indexer's own connection (or assert via the
indexer's `audit()` API). The harness's helpers were rewritten
to call `h.indexer.audit()` instead of opening a fresh DuckDB.

## Implication for production

Anywhere we expose read access to a DuckDB file that's also being
written by an `Indexer`, we MUST go through the indexer's
connection — or issue `PRAGMA force_checkpoint` before reading
from a separate connection.

The M3 transport layer (`escurel-server`) should follow this
pattern:

- Per-tenant `TenantHandle` holds one writer connection + a
  bounded pool of reader connections backed by the same DuckDB
  file.
- Readers MUST share the same database instance — not separately
  `Connection::open` the file from different threads — so they
  see the writer's committed state via DuckDB's in-process MVCC.

## How to recognise next time

If a "fresh" connection's `SELECT count(*)` returns a stale view
while another connection's recent transactions are clearly
committed: stop opening fresh connections to the same file in
the same process. Use the writer's connection, or use a
connection pool that shares one DuckDB instance.

## Related

- spec `docs/spec/platform.md §Concurrency` already mandates the
  connection-pool pattern for production. This note documents
  the *test-time* trap that motivates it.
