# The SQL search lane scanned every source on every search

**Symptom.** In the lab, `search` without `page_kind: skill` (or a filter) took more than 90 s.
A search limited to one skill took 3–6 s, even for a markdown skill with no SQL instances at all.
`search` with `filter: {}` (which switches the extra lanes off) took 0.6 s over the same corpus.

**Cause.** For each `sql_view` instance the SQL lane ran
`SELECT count(*) FROM vw_<skill>__<id> WHERE <search_text col> ILIKE '%q%'`.
The view is a lazy `CREATE VIEW … AS SELECT * FROM read_parquet('s3://…')`, so every search read every source in full.
That happened once per query variant, with the tenant's single connection mutex held for each scan.
The slow single-skill searches came from lock contention: other callers' unfiltered searches held the connection one scan at a time, so every request on the tenant waited behind them.

**Fix.** The lane reads `sql_view_search_text`, a local, non-lake projection.
It holds the distinct `concat_ws(chr(31), search_text cols)` values with their row counts, keyed by `(view, sig)`, where sig is the binding hash plus the columns.
The projection is captured by `create_instance`, by `rebuild`, and by a background task after writer boot.
The capture runs on `conn.try_clone()`, so the source scan never holds the shared connection.
Search never falls back to the source: a view with no projection yet simply has no SQL candidate.

**Recognise it next time.**
- A tenant whose tools get slow when `search` is busy is a sign of shared-connection contention.
- Compare `search` with `filter: {}` against the same call without it. A large gap points at the extra lanes.
- The rows lane (`rows_search_candidates`) has the same shape for `instances: rows` skills.
  It even re-materialises the view on every search.
  Lab had no rows skills when this was measured.

## Second cause, found after the projection shipped (same day)

Lab search was still hanging with #849 deployed, so the projection fixed only part of the problem.

- **Correction:** `filter: {}` is *not* "lanes off". `is_empty_filter` treats it as no filter at all. The 0.6 s
  measured above was simply an unfiltered search on a quiet tenant. The reliable switch is `as_of`, which turns
  the extra lanes off.
- **What was still slow:** the extra lanes took the connection mutex hundreds of times per search:
  - `list_skills` twice;
  - `rows_source` and `remote_rows::source` once per skill;
  - three acquisitions per `sql_view` instance;
  - and all of that once per query variant.
- **Why that hurt:** the tokio mutex is fair, and the async runner polls `list_events` (about 1.4 s holding the
  lock), `list_skills` and `list_inbox` around the clock. So every acquisition queued behind the runner.
- **Measured:** `page_kind: skill` took 1.3 s and `as_of` (lanes off) 2.3 s. A single skill took about 7 s,
  while `page_kind: instance` and an unfiltered search never completed.
- **Fix:** the lanes decide from one catalogue read and count under one lock.
- **Regression test:** `the_search_lanes_take_the_connection_a_bounded_number_of_times` models the neighbour.
  It ran 3.4 s before the fix and stays under 0.8 s after.
- **Recognise it next time:** latency that grows with the number of skills, rather than with the data, under
  background load.
