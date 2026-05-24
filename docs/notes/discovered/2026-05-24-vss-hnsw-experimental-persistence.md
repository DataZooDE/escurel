# vss HNSW persistence requires an "experimental" opt-in

**Date:** 2026-05-24
**Scope:** `escurel-index` schema migrator

## Symptom

After loading the vss extension successfully, this DDL still fails:

```sql
CREATE INDEX hnsw_blocks_vec ON blocks USING HNSW (dense_vec);
```

with:

```
Binder Error: HNSW indexes can only be created in in-memory
databases, or when the configuration option
'hnsw_enable_experimental_persistence' is set to true.
```

## Cause

The vss extension treats persistent (on-disk) HNSW as experimental
and gates it behind `SET hnsw_enable_experimental_persistence = true`.
The default behaviour is to refuse HNSW indexes on file-backed
DuckDB databases.

The Escurel storage spec (`docs/spec/storage.md §HNSW persistence
model`) is built on the assumption that the on-disk HNSW index
loads as-is on `Connection::open`, rolls back atomically with the
write transaction on mid-write SIGKILL, and is rebuildable from
canonical markdown in ~32 ms/page on cattle-node-loss. All of
that depends on persistent HNSW — the in-memory mode is not an
option for us.

## Fix

Set the flag in the autoload stage, immediately after `LOAD vss`:

```sql
SET hnsw_enable_experimental_persistence = true;
```

The "experimental" label is a vss-side stability marker, not a
correctness one — the on-disk format works and is rebuildable
via `Migrator::rebuild` if it ever changes. The
[ADR-0001 pre-deployment retrieval-quality gate](../../adr/0001-duckdb-only-storage.md#pre-deployment-gate)
exercises this path before production rollout.

## How to recognise next time

If a DuckDB extension errors with a message that contains
"experimental" or "set <flag> to true": there is almost always a
SET PRAGMA toggle you can put in your bootstrap to enable the
feature. The extension's docs usually name the flag in the error
message itself.

## Watch for

- If a future vss release renames or removes the flag — pin the
  duckdb-rs version and run the M1 retrieval-quality gate again
  on upgrade.
- If the extension promotes the feature to non-experimental, the
  SET becomes a no-op but stays harmless. Leave it in until we
  drop support for the version range that needed it.
