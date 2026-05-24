# DuckDB autoload doesn't fire for `USING HNSW` DDL tokens

**Date:** 2026-05-24
**Scope:** `escurel-index` schema migrator

## Symptom

With `autoload_known_extensions = true` and
`autoinstall_known_extensions = true` set, this DDL fails:

```sql
CREATE INDEX hnsw_blocks_vec
    ON blocks USING HNSW (dense_vec)
    WITH (metric = 'cosine');
```

with `Binder Error: Unknown index type: HNSW`.

## Cause

DuckDB autoload triggers when the binder resolves an unknown
function or type reference in a SQL expression. `USING HNSW` is
parsed as part of `CREATE INDEX` syntax and does NOT resolve
through the function/type lookup path, so autoload never fires.
The vss extension is therefore never loaded, and `HNSW` stays
unknown to the index binder.

Same applies to `PRAGMA create_fts_index` — the PRAGMA dispatcher
checks for registered PRAGMAs in the catalog *before* autoload
gets a chance.

## Fix

Add explicit `LOAD vss; LOAD fts;` to the migration's autoload
stage. With `autoinstall_known_extensions = true` still set, the
LOAD itself downloads + caches the extension binary on first call
— no separate INSTALL needed.

The migration that ships with the indexer:

```sql
SET autoinstall_known_extensions = true;
SET autoload_known_extensions    = true;
LOAD vss;
LOAD fts;
```

## How to recognise next time

If you see `Unknown index type: <X>` or `Catalog Error: PRAGMA
<X> does not exist` from a DDL that *should* be loading an
extension transparently: the autoload trigger probably doesn't
match the syntactic position. Add an explicit `LOAD <extension>;`
just before the DDL.

## Related

- [`2026-05-24-vss-hnsw-experimental-persistence.md`](2026-05-24-vss-hnsw-experimental-persistence.md)
  — the second hurdle once vss is loaded.
- [`2026-05-24-fts-no-refresh-pragma.md`](2026-05-24-fts-no-refresh-pragma.md)
  — `PRAGMA refresh_fts_index` is not in the current fts; spec
  storage.md is out of date.
