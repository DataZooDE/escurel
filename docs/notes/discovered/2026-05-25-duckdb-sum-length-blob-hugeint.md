# DuckDB `SUM(LENGTH(blob))` returns HUGEINT — `get::<_, i64>` silently zeroes

**Symptom.** While wiring the `compact_lanes` admin RPC we wanted to
report the total byte volume reclaimed per page:

```sql
SELECT COALESCE(SUM(LENGTH(op_bytes)), 0)
FROM crdt_ops WHERE page_id = ? AND hlc <= ?
```

Pulled into Rust as `row.get::<_, i64>(0)` and the result was always
`0`, even when the page held many non-empty `op_bytes` rows. No
error — `query_row(...).unwrap_or(0)` silently absorbed the type
mismatch and we returned a perfectly-shaped wire envelope with
`bytes_reclaimed = 0`. The integration test caught it
(`compact_lanes_streams_one_progress_per_page_with_subsumed_ops`),
not the unit test.

**Cause.** In DuckDB, `SUM` over an integer column widens its result
to `HUGEINT` (a 128-bit type) to avoid overflow. The `duckdb` crate's
`row.get::<_, i64>(idx)` getter only succeeds when the column type
is `BIGINT` or narrower; it returns an `InvalidColumnType` error for
`HUGEINT`. Our `.unwrap_or(0)` then swallowed the error and
returned the default. `OCTET_LENGTH(blob)` has the same shape as
`LENGTH(blob)` here — the issue is the SUM widening, not the inner
function.

**Fix.** Cast the SUM back to `BIGINT` inside the SQL, where you
know the row count is small enough for the reduction to fit:

```sql
SELECT CAST(COALESCE(SUM(OCTET_LENGTH(op_bytes)), 0) AS BIGINT)
FROM crdt_ops WHERE page_id = ? AND hlc <= ?
```

`OCTET_LENGTH` is the spec-correct function for "byte count of a
BLOB"; DuckDB accepts both but `OCTET_LENGTH` is the one to reach
for first because the name encodes the intent. The explicit
`AS BIGINT` cast is the load-bearing change — it makes the getter
column type match the binding.

**How to recognise it next time.** A `SUM(...)` query whose result
unexpectedly reads as zero through `query_row(...).get::<_, i64>(0)`
is almost certainly a HUGEINT-vs-BIGINT type mismatch. The two
diagnostics are: drop the `unwrap_or(0)` masking briefly to see the
underlying `InvalidColumnType("HUGEINT")`, or run the query in the
DuckDB CLI and look at the result type. The fix is always a `CAST
... AS BIGINT` (or moving to `get::<_, i128>(0)` if you truly need
the wider range).
