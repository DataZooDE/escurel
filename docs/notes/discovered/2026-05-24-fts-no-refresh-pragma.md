# fts extension has no `refresh_fts_index` PRAGMA

**Date:** 2026-05-24
**Scope:** spec / `escurel-index` indexer (future PR 5+)

## Symptom

Spec `docs/spec/storage.md §DuckDB schema` and §Indexer step 5
both reference `PRAGMA refresh_fts_index('blocks')` as the
incremental-update PRAGMA for the FTS index after row
insert / update.

Calling it against the current fts extension produces:

```
Catalog Error: Pragma Function with name refresh_fts_index does
not exist! Did you mean "create_fts_index"?
```

## Cause

The current fts extension exposes only two PRAGMAs:

- `PRAGMA create_fts_index(table, doc_id, body, ...)` — builds
  the index over the current rows in `table`.
- `PRAGMA drop_fts_index(table)` — drops it.

There is **no incremental refresh**. The supported pattern to pick
up new rows is one of:

1. `PRAGMA create_fts_index(..., overwrite = 1)` — rebuilds the
   index in place.
2. `PRAGMA drop_fts_index(table); PRAGMA create_fts_index(...);`
   — drop first, then recreate.

`overwrite = 1` is the simpler one and the one the test in
`crates/escurel-index/tests/migrate.rs` uses.

## Implication for the spec

`docs/spec/storage.md` is wrong on this one detail. It will need
a small edit before the indexer PR lands:

- Replace "`PRAGMA refresh_fts_index('blocks')`" with the
  `overwrite = 1` rebuild incantation.
- Note that FTS rebuild is O(rows), so per-write rebuild is
  unacceptable for hot pages. The indexer should batch or defer
  FTS rebuild (e.g. run it at write-lock release time, or behind
  a debounce, or on a periodic compact_db pass).

This affects the indexer write-path latency budget in
`platform.md` for `update_page`. Likely the right move is to keep
the per-write `vss` HNSW update (incremental) and amortise the FTS
rebuild — possibly behind the `compact_db` admin endpoint.

## Action items

- [ ] Open a follow-up to fix the spec wording (small PR, low
      priority).
- [ ] Decide the FTS rebuild cadence in PR 5 / PR 6 when the
      indexer's hot write path lands. Discuss with the user
      before picking.

## How to recognise next time

If you see "Pragma Function with name <X> does not exist" — check
the extension's current PRAGMA list before assuming it should
work. Spec ages faster than extension APIs.
