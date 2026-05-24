# `links` primary key must include `dst_anchor` (spec divergence)

**Date:** 2026-05-24
**Scope:** `escurel-index`; impacts `docs/spec/storage.md`

## Symptom

Codex review of PR #10 surfaced the issue, and a regression
integration test confirmed it: when a page body cites two
different anchors of the same target — e.g.

```text
See [[customer::acme#billing]] and [[customer::acme#renewals]].
```

— only the first link landed in the `links` table. The second
was silently dropped by `INSERT OR IGNORE`.

## Cause

The spec's `links` schema (`docs/spec/storage.md §DuckDB schema`)
declares the primary key as `(src_page, src_anchor, dst_page,
link_skill)` — **without** `dst_anchor`. So two rows that differ
only in `dst_anchor` share the same PK and the second is treated
as a duplicate by `INSERT OR IGNORE`.

That is a spec bug. `[[customer::acme#billing]]` and
`[[customer::acme#renewals]]` are semantically distinct edges in
the graph — they cite different parts of the target — and the
typed-backlink primitive (`neighbours(acme, link_skill=customer)`)
is supposed to return both.

## Fix (this PR)

Schema change in `crates/escurel-index/sql/0001_b_tables.sql`:

```diff
-    dst_anchor   VARCHAR,
+    -- DuckDB PKs forbid NULL columns; substitute '' for "no anchor".
+    dst_anchor   VARCHAR NOT NULL DEFAULT '',
     link_skill   VARCHAR NOT NULL,
     link_version VARCHAR,
-    PRIMARY KEY (src_page, src_anchor, dst_page, link_skill)
+    PRIMARY KEY (src_page, src_anchor, dst_page, dst_anchor, link_skill)
```

DuckDB primary keys cannot contain NULL columns, so we use the
empty string as the "no anchor" sentinel at the storage layer.
The API layer (M3) projects `''` back to `Option<String>::None`
when serving `neighbours()` results.

The indexer's `update_page` writes `wl.anchor.as_deref().unwrap_or("")`
into `dst_anchor`. With the new PK, `INSERT OR IGNORE` only
collapses true duplicates (same source, same anchor, same target).

## Spec follow-up (deferred)

`docs/spec/storage.md §DuckDB schema` needs a small edit to:

- include `dst_anchor` in the PK
- document the `''`-as-no-anchor sentinel
- note that `neighbours()` API responses project `''` back to
  `null`

This is a small, low-risk spec PR. Track and merge before any
external consumer of the schema appears (none today).

## How to recognise next time

If `INSERT OR IGNORE` is silently dropping rows you expect to
keep: the PK is too coarse. Either change the PK to include the
discriminator, or use `INSERT` (and handle the genuine-duplicate
case differently). Codex's review prompt of "design + missing
functions" caught this — keep running periodic codex reviews on
schema work specifically.
