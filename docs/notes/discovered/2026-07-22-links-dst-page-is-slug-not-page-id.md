# `links.dst_page` holds the wikilink slug, not the target's page_id

**Date:** 2026-07-22
**Context:** implementing `delete_page` (#300)

## Symptom

While building `delete_page` I first tried to remove every edge a page
participates in with:

```sql
DELETE FROM links WHERE src_page = ? OR dst_page = ?   -- ? = page_id
```

The `dst_page = ?` half matched nothing. A page `c2` linking to `c1` via
`[[customer::c1]]` still showed the edge afterwards.

## Cause

The `links` table is asymmetric about what its two endpoint columns hold:

- `src_page` = the **full page_id** of the source page
  (e.g. `markdown/instances/customer/c2.md`).
- `dst_page` = the **wikilink target slug** (e.g. `c1`), paired with
  `link_skill` = the target's skill. It is NOT a page_id.

`Indexer::update_page` inserts links with `dst_page = wl.id` (the parsed
wikilink id), and `Indexer::neighbours` resolves inbound edges by looking up
the page's `(slug, skill)` and matching `WHERE l.dst_page = <slug> AND
l.link_skill = <skill>` — never by the destination page_id. So a target's
inbound edges are keyed by slug+skill, and a source's outbound edges by
page_id.

## Fix / how to recognise it

For `delete_page` the right scope turned out to be **outbound only** —
`DELETE FROM links WHERE src_page = ?` (the page's own edges), exactly what
`update_page` refreshes. Inbound edges belong to the still-live source pages'
content; a rebuild would recreate them, so removing them would only diverge
the index from the source until the next rebuild. They become ordinary
dangling wikilinks (a `validate` finding); the retracted page itself no longer
resolves.

When reasoning about the link graph: `src_page` is a page_id, `dst_page` is a
slug. Any "delete/patch edges pointing at page X" logic must match
`(dst_page = <X.slug>, link_skill = <X.skill>)`, not `dst_page = <X.page_id>`.
