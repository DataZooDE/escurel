# vss HNSW tolerates rows with NULL dense_vec

**Date:** 2026-05-25
**Scope:** `escurel-index` chat_messages schema (issue #63)

## Symptom

For the per-chat-group conversation history surface (issue #63,
M-Chat), append_message should be able to skip embedding when the
caller passes `embed = false` (cheap inserts for high-volume sources).
That means some rows in `chat_messages` carry a real `FLOAT[768]`
vector and some carry `NULL`.

Open question before designing the schema: does the vss HNSW index
break when a column it covers contains NULL? Two failure modes were
plausible:

1. `INSERT INTO chat_messages (..., dense_vec) VALUES (..., NULL)`
   errors at the index-update step.
2. A subsequent `ORDER BY array_cosine_distance(dense_vec, ?)` returns
   NaN / panics / silently corrupts results when NULL rows are scanned.

If either happens, the table needs a two-table layout
(`chat_messages` + `chat_messages_embedded`) reconciled via UNION at
read time.

## Verdict

Neither failure mode reproduces on the duckdb + vss version pinned in
this workspace. A standalone probe (FLOAT[768] column, HNSW index with
`metric = cosine, ef_construction = 128, ef_search = 64, M = 16`)
accepted a NULL insert (`Ok(1)`), and a similarity query with
`WHERE dense_vec IS NOT NULL ORDER BY array_cosine_distance(...)`
returned the correct nearest neighbour. No errors, no panics, no
corruption of subsequent inserts.

Conclusion: a **single-table layout** for `chat_messages` is safe.
Embedded and non-embedded messages live in one row each; similarity
search filters `WHERE dense_vec IS NOT NULL` (or equivalently
`WHERE embedded = TRUE`).

## How to recognise next time

If a vector-indexed column needs to accept NULLs, write a 20-line
probe first: create the table + index, insert one row with a value
and one with NULL, then run a similarity query that filters out the
NULL rows. The vss source tree does not document NULL handling
explicitly, so empirical verification is the cheapest answer.

## Watch for

- If a future vss release tightens NULL handling, the M-Chat
  integration test `append_with_embed_false_skips_vector` will fail
  loudly — the test exercises the exact pattern.
- DuckDB's array literal type-casts (`[...]::FLOAT[768]`) do **not**
  coerce a `NULL` value; pass a true SQL `NULL` (no cast) when you
  want to omit the vector. The vss-side index update is what was at
  risk, not the SQL binder.
