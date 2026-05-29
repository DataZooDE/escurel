# `as_of` time-travel must spare untimed pages (`at_ts IS NULL`)

## Symptom

The obvious way to implement an `as_of` cut is `WHERE at_ts <= ?`. Do
that and the whole workspace **disappears at T+0**: skill pages,
`query::*` stored queries, and any non-event instance carry no `at`, so
their `at_ts` is `NULL`, and `NULL <= '2026-01-01'` is `NULL` (not
true) in SQL — they get filtered out. The radial skill-wheel goes blank,
search returns nothing, `expand` on a skill 404s.

## Fix

Every `as_of` predicate is `(at_ts <= ? OR at_ts IS NULL)`. Untimed
pages are *not events on the timeline*; they are always present
regardless of the cut. This holds across all four read tools:

- `list_instances` — `AND (at_ts <= ? OR at_ts IS NULL)` on `pages`
- `expand` — same predicate on the page-row lookup (a not-yet-born
  *timed* page returns `None`; a skill never does)
- `search` — `AND (blocks.at_ts <= ? OR blocks.at_ts IS NULL)` on the
  shared filter, so it hits both the vector and FTS halves
- `neighbours` — an `EXISTS` on the **source** page's `at_ts` with the
  same NULL-spare clause, so edges from not-yet-born sources vanish but
  edges from untimed sources stay

Gated by `crates/escurel-index/tests/as_of.rs`
(`*_keeps_untimed_*` cases assert the NULL survivors explicitly).

## Also worth knowing

`at_ts` is the raw RFC 3339 `at` string cast to a DuckDB `TIMESTAMP`
(timezone-naive) at write time. The `as_of` bind is compared the same
way, so offset-bearing inputs (`…+02:00`, `…Z`) sort consistently with
the stored values — don't "normalise" one side without the other.

## How to recognise it next time

Any new time-filtered read path: write the "untimed page survives the
cut" test *first*. If a skill or stored query vanishes when you scrub
time, you forgot the `OR <col> IS NULL`.
