# `frontmatter_index` is a dead table — filter on `pages.frontmatter` instead

## Symptom

`crates/escurel-index/sql/0001_b_tables.sql` declares a
`frontmatter_index (page_id, key, value JSON, value_ts)` table with an
`fm_key_value` index, and the schema docs describe it as "flattened
key/value for skill-specific filters". It looks like the obvious thing
to JOIN when you want to filter instances by a frontmatter field
(`source = 'gmail'`, `tier = 'enterprise'`, …).

It is **never written to.** No code path inserts rows. A JOIN against
it returns zero rows for every page. (`grep -rn "INTO frontmatter_index"
crates/` finds nothing; the only references are the `INSPECTABLE_TABLES`
allow-list and the schema/migration tests.)

## Fix

When PR-5 added the `(key, value)` filter to `Indexer::list_instances`,
it matched directly against the canonical stored frontmatter on the
`pages` table — `pages.frontmatter` is a `JSON NOT NULL` column — via:

```sql
... WHERE page_type = 'instance' AND skill = ?
        AND json_extract_string(frontmatter, ?) = ?
```

with the path bound as `'$.<key>'` and the value bound as text. No
separate index to keep in sync, no migration, and it reads the single
source of truth.

## How to recognise it next time

If you're about to JOIN `frontmatter_index` (or any "obviously there"
secondary index), first confirm something populates it:
`grep -rn "INTO <table>" crates/`. For frontmatter lookups, prefer
`json_extract_string(frontmatter, '$.key')` on `pages` until the day we
actually decide to maintain a denormalised index (at which point the
write path in `update_page` must populate it and a test must assert a
row lands).
